#!/usr/bin/env python3
"""Потоковое распознавание речи для health-widget через faster-whisper.

Читает СЫРОЙ моно-PCM s16le 16000 Гц из stdin (его пишет Rust-канал после ресемплинга)
и печатает построчный JSON в stdout — только законченные фразы:

    {"final": "Запусти Kubernetes"}

Endpointing по энергии (пауза => конец фразы), инференс на GPU (CUDA), два рычага под
IT-термины: hotwords/initial_prompt при распознавании + словарь пост-коррекции.
Никакой сети в рантайме (модель кэшируется локально при установке).

Аргументы:
    argv[1] — имя или путь модели faster-whisper (напр. large-v3)
Окружение:
    WHISPER_DEVICE (cuda), WHISPER_COMPUTE (float16)
Файлы баз (рядом со скриптом, опциональны):
    it_hotwords.txt, it_corrections.tsv
"""
import os
import re
import sys
import json
import time
import threading
from typing import Iterable


def parse_hotwords(lines: Iterable[str], limit: int = 80) -> str:
    terms = []
    for line in lines:
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        terms.append(s)
        if len(terms) >= limit:
            break
    return ", ".join(terms)


def parse_corrections(lines: Iterable[str]):
    pairs = []
    for line in lines:
        s = line.rstrip("\n")
        if not s or s.startswith("#") or "\t" not in s:
            continue
        wrong, right = s.split("\t", 1)
        wrong, right = wrong.strip(), right.strip()
        if not wrong or not right:
            continue
        rx = re.compile(r"(?<!\w)" + re.escape(wrong) + r"(?!\w)", re.IGNORECASE)
        pairs.append((rx, right))
    return pairs


def apply_corrections(text: str, pairs) -> str:
    for rx, right in pairs:
        text = rx.sub(right, text)
    return text


def load_hotwords(path: str) -> str:
    try:
        with open(path, encoding="utf-8") as f:
            return parse_hotwords(f)
    except OSError:
        return ""


def load_corrections(path: str):
    try:
        with open(path, encoding="utf-8") as f:
            return parse_corrections(f)
    except OSError:
        return []


SAMPLE_RATE = 16000
FRAME = 320                 # 20 мс
FRAME_BYTES = FRAME * 2
SILENCE_RMS = 500.0         # порог RMS int16: тишина/речь
SILENCE_TAIL = 0.6          # сек тишины => конец фразы
MIN_SPEECH = 0.3            # короче — считаем шумом, не транскрибируем
MAX_SEGMENT = 15.0          # форс-флаш при непрерывной речи

_buf = bytearray()
_buf_lock = threading.Lock()
_stdin_open = True


def _drain_stdin():
    """Отдельный поток: непрерывно вычитываем stdin, чтобы Rust не упирался в
    заполненный пайп, пока грузится модель или идёт инференс."""
    global _stdin_open
    while True:
        b = sys.stdin.buffer.read(4096)
        if not b:
            _stdin_open = False
            return
        with _buf_lock:
            _buf.extend(b)


def _take(n: int) -> bytes:
    with _buf_lock:
        if not _buf:
            return b""
        m = min(n, len(_buf))
        out = bytes(_buf[:m])
        del _buf[:m]
        return out


def _ensure_cuda_libpath():
    """CUDA-либы (cublas/cudnn) стоят pip-колёсами внутри venv и не на пути загрузчика.
    Выставляем LD_LIBRARY_PATH и перезапускаем интерпретатор — линкер читает путь только
    при старте процесса. Guard по _WHISPER_LDPATH защищает от повторного re-exec.
    Вызывается только из main() (не при импорте), чтобы не ломать unit-тесты."""
    if os.environ.get("_WHISPER_LDPATH"):
        return
    import glob
    libs = glob.glob(os.path.join(sys.prefix, "lib", "python*", "site-packages",
                                  "nvidia", "*", "lib"))
    os.environ["_WHISPER_LDPATH"] = "1"
    if not libs:
        return
    prev = os.environ.get("LD_LIBRARY_PATH", "")
    os.environ["LD_LIBRARY_PATH"] = ":".join(libs + ([prev] if prev else []))
    os.execv(sys.executable, [sys.executable] + sys.argv)


def main() -> int:
    _ensure_cuda_libpath()
    if len(sys.argv) < 2:
        sys.stderr.write("usage: whisper_stream.py <model>\n")
        return 2
    model_name = sys.argv[1]

    import numpy as np
    from faster_whisper import WhisperModel

    base = os.path.dirname(os.path.abspath(__file__))
    hotwords = load_hotwords(os.path.join(base, "it_hotwords.txt")) or None
    corrections = load_corrections(os.path.join(base, "it_corrections.tsv"))

    device = os.environ.get("WHISPER_DEVICE", "cuda")
    compute = os.environ.get("WHISPER_COMPUTE", "float16")

    threading.Thread(target=_drain_stdin, daemon=True).start()
    model = WhisperModel(model_name, device=device, compute_type=compute)

    def emit(raw: bytes):
        audio = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
        segments, _ = model.transcribe(
            audio, language="ru", beam_size=5, vad_filter=True,
            condition_on_previous_text=False,
            hotwords=hotwords, initial_prompt=hotwords,
        )
        text = " ".join(s.text.strip() for s in segments).strip()
        text = apply_corrections(text, corrections)
        if text:
            sys.stdout.write(json.dumps({"final": text}, ensure_ascii=False) + "\n")
            sys.stdout.flush()

    pending = bytearray()
    utter = bytearray()
    speaking = False
    silence_run = 0.0

    while True:
        chunk = _take(65536)
        if chunk:
            pending.extend(chunk)
        elif not _stdin_open:
            if len(utter) > MIN_SPEECH * SAMPLE_RATE * 2:
                emit(bytes(utter))
            break
        else:
            time.sleep(0.02)
            continue

        while len(pending) >= FRAME_BYTES:
            frame = bytes(pending[:FRAME_BYTES])
            del pending[:FRAME_BYTES]
            samples = np.frombuffer(frame, dtype=np.int16).astype(np.float32)
            rms = float(np.sqrt(np.mean(samples * samples)))
            if rms >= SILENCE_RMS:
                speaking = True
                utter.extend(frame)
                silence_run = 0.0
            elif speaking:
                utter.extend(frame)
                silence_run += FRAME / SAMPLE_RATE
                if silence_run >= SILENCE_TAIL:
                    if len(utter) > MIN_SPEECH * SAMPLE_RATE * 2:
                        emit(bytes(utter))
                    utter = bytearray()
                    speaking = False
                    silence_run = 0.0
            if speaking and len(utter) >= MAX_SEGMENT * SAMPLE_RATE * 2:
                emit(bytes(utter))
                utter = bytearray()
                speaking = False
                silence_run = 0.0
    return 0


if __name__ == "__main__":
    sys.exit(main())
