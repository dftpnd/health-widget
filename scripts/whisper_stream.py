#!/usr/bin/env python3
"""Потоковое распознавание речи для health-widget через faster-whisper.

Читает СЫРОЙ моно-PCM s16le 16000 Гц из stdin (его пишет Rust-канал после ресемплинга)
и печатает построчный JSON в stdout — черновик текущей фразы и законченные куски:

    {"partial": "Запусти Kuber"}
    {"final": "Запусти Kubernetes."}

Стриминг по LocalAgreement-2: скользящий буфер декодируется каждые ~1 с нового аудио,
совпавший префикс слов двух последовательных декодов коммитится в финал, хвост уходит
черновиком. Пауза ≥0.4 с — флаш; короткая изолированная фраза при флаше декодируется
дважды (второй раз со сдвигом входа): на шуме декод нестабилен и не коммитится.
Инференс на GPU (CUDA), два рычага под IT-термины: hotwords + словарь пост-коррекции.
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
        text = rx.sub(lambda _m, r=right: r, text)
    return text

HALLUCINATIONS = {
    "продолжение следует",
    "продолжение в следующей серии",
    "субтитры",
    "субтитры сделал dimatorzok",
    "субтитры создавал dimatorzok",
    "редактор субтитров а.семкин корректор а.егорова",
    "аплодисменты",
    "аплодисменты и смех",
    "музыка",
    "играет музыка",
    "смех",
    "спасибо за просмотр",
    "спасибо за внимание",
    "подписывайтесь на канал",
    "подписывайтесь",
    "ставьте лайки",
}

HALLUCINATION_RE = (
    re.compile(r"^подпи(шись|шитесь|сывайся|сывайтесь)( на канал\w*)?$"),
    re.compile(r"^ставьте лайк\w*$"),
    re.compile(r"^(лайк\w*|не забуд\w+|ставь\w*).*подпи\w+"),
    re.compile(r"^раз[,\s]+два[,\s]+три([,\s]+четыре)?[,\s]*$"),
)

def _norm(text: str) -> str:
    """Нормализовать сегмент для сравнения с чёрным списком: lower, без внешней пунктуации
    и обрамляющих скобок (whisper пишет теги как «[Аплодисменты]»/«(музыка)»)."""
    return " ".join(text.strip().lower().strip(" .…!?,-—:;\"'[](){}«»").split())

def is_hallucination(text: str, no_speech_prob: float = 0.0,
                     avg_logprob: float = 0.0, compression_ratio: float = 0.0) -> bool:
    raw = text.strip()
    n = _norm(text)
    if not n:
        return True
    if len(raw) >= 2 and raw[0] in "[(" and raw[-1] in ")]":
        return True
    if n in HALLUCINATIONS:
        return True
    if any(rx.match(n) for rx in HALLUCINATION_RE):
        return True
    if "субтитр" in n and ("dimatorzok" in n or "семкин" in n or "корректор" in n):
        return True
    if no_speech_prob >= 0.85 and avg_logprob <= -0.6:
        return True
    if avg_logprob <= -1.0:
        return True
    if compression_ratio >= 2.4:
        return True
    return False

def flatten_words(segments) -> list[tuple[str, float, float]]:
    out = []
    for s in segments:
        if is_hallucination(
            s.text,
            getattr(s, "no_speech_prob", 0.0),
            getattr(s, "avg_logprob", 0.0),
            getattr(s, "compression_ratio", 0.0),
        ):
            continue
        for w in (s.words or []):
            out.append((w.word.strip(), w.start, w.end))
    return out

def cut_bytes(buf_len: int, committed_end: float | None, max_seconds: float) -> int:
    limit = int(max_seconds * SAMPLE_RATE) * 2
    if buf_len <= limit:
        return 0
    if committed_end is None:
        return buf_len - limit
    return min(buf_len, int(committed_end * SAMPLE_RATE) * 2)

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
FRAME = 320
FRAME_BYTES = FRAME * 2
SILENCE_RMS = 500.0
SILENCE_TAIL = 0.4
MIN_SPEECH = 0.3
MIN_NEW_AUDIO = 1.0
MAX_BUFFER = 12.0
STABILITY_MAX_SECONDS = 3.0
PERTURB_PAD_SECONDS = 0.15
PERTURB_GAIN = 0.9
SENT_END = ".?!…"
FINAL_MAX_WORDS = 30

def norm_word(w: str) -> str:
    return re.sub(r"[\W_]+", "", w.lower())

def common_prefix(a: list[tuple[str, float, float]], b: list[tuple[str, float, float]]) -> int:
    n = 0
    m = min(len(a), len(b))
    while n < m and norm_word(a[n][0]) == norm_word(b[n][0]):
        n += 1
    return n

def advance(prev_words: list[tuple[str, float, float]], committed: int, cur_words: list[tuple[str, float, float]]) -> tuple[int, list[str], str]:
    n = common_prefix(prev_words, cur_words)
    newly = [w for w, _s, _e in cur_words[committed:n]]
    committed = max(committed, n)
    partial = " ".join(w for w, _s, _e in cur_words[committed:])
    return committed, newly, partial

def take_final(pending: list[str], limit: int = FINAL_MAX_WORDS) -> tuple[str, list[str]]:
    last = -1
    for i, w in enumerate(pending):
        if w and w[-1] in SENT_END:
            last = i
    if last >= 0:
        return " ".join(pending[:last + 1]), pending[last + 1:]
    if len(pending) >= limit:
        return " ".join(pending), []
    return "", pending

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

    def to_float(raw: bytes):
        return np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0

    def perturbed(audio_f):
        pad = np.zeros(int(PERTURB_PAD_SECONDS * SAMPLE_RATE), dtype=np.float32)
        return np.concatenate([pad, audio_f * PERTURB_GAIN])

    def decode(audio_f):
        segments, _ = model.transcribe(
            audio_f, language="ru", beam_size=5, vad_filter=True,
            condition_on_previous_text=False, hotwords=hotwords,
            word_timestamps=True,
        )
        return flatten_words(list(segments))

    def emit_final(text: str):
        text = apply_corrections(text.strip(), corrections)
        if text:
            sys.stdout.write(json.dumps({"final": text}, ensure_ascii=False) + "\n")
            sys.stdout.flush()

    def emit_partial(text: str):
        sys.stdout.write(json.dumps({"partial": text}, ensure_ascii=False) + "\n")
        sys.stdout.flush()

    audio = bytearray()
    pending_bytes = bytearray()
    prev_words = []
    committed = 0
    final_words = []
    speaking = False
    silence_run = 0.0
    speech_secs = 0.0
    since_decode = 0
    min_decode_bytes = int(MIN_NEW_AUDIO * SAMPLE_RATE) * 2

    def stream_step():
        nonlocal prev_words, committed, final_words, since_decode
        cur = decode(to_float(bytes(audio)))
        committed, newly, partial = advance(prev_words, committed, cur)
        prev_words = cur
        final_words.extend(newly)
        out, final_words = take_final(final_words)
        if out:
            emit_final(out)
        emit_partial(" ".join(final_words + ([partial] if partial else [])))
        since_decode = 0
        end = cur[committed - 1][2] if 0 < committed <= len(cur) else None
        cut = cut_bytes(len(audio), end, MAX_BUFFER)
        if cut:
            del audio[:cut]
            prev_words = []
            committed = 0

    def flush():
        nonlocal prev_words, committed, final_words, speaking
        nonlocal silence_run, speech_secs, since_decode
        if audio and (committed or final_words or speech_secs >= MIN_SPEECH):
            f = to_float(bytes(audio))
            cur = decode(f)
            if (
                not committed
                and not final_words
                and len(audio) <= STABILITY_MAX_SECONDS * SAMPLE_RATE * 2
            ):
                alt = decode(perturbed(f))
                cur = cur[: common_prefix(cur, alt)]
            tail = [w for w, _s, _e in cur[committed:]]
            emit_final(" ".join(final_words + tail))
        audio.clear()
        prev_words = []
        committed = 0
        final_words = []
        speaking = False
        silence_run = 0.0
        speech_secs = 0.0
        since_decode = 0
        emit_partial("")

    while True:
        chunk = _take(65536)
        if chunk:
            pending_bytes.extend(chunk)
        elif not _stdin_open:
            flush()
            break
        else:
            time.sleep(0.02)
            continue

        while len(pending_bytes) >= FRAME_BYTES:
            frame = bytes(pending_bytes[:FRAME_BYTES])
            del pending_bytes[:FRAME_BYTES]
            samples = np.frombuffer(frame, dtype=np.int16).astype(np.float32)
            rms = float(np.sqrt(np.mean(samples * samples)))
            if rms >= SILENCE_RMS:
                speaking = True
                silence_run = 0.0
                speech_secs += FRAME / SAMPLE_RATE
            elif speaking:
                silence_run += FRAME / SAMPLE_RATE
            if speaking:
                audio.extend(frame)
                since_decode += FRAME_BYTES
                if silence_run >= SILENCE_TAIL:
                    flush()

        if speaking and since_decode >= min_decode_bytes:
            stream_step()
    return 0

if __name__ == "__main__":
    sys.exit(main())
