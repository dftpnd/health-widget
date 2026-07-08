#!/usr/bin/env python3
"""Одноразовая проверка: грузится ли faster-whisper на GPU (RTX 5080 / sm_120).

Запуск:
    venv-whisper/bin/python whisper_smoke.py [путь_к_wav]

Без аргумента прогоняет 1 секунду синтетического сигнала — доказывает, что модель
грузится и инференс идёт на CUDA (текст может быть пустым, это ок). С путём к WAV
печатает распознанный текст для глазами-проверки качества.
"""
import sys
import numpy as np
from faster_whisper import WhisperModel

def main() -> int:
    print("loading large-v3 on cuda/float16 ...", flush=True)
    model = WhisperModel("large-v3", device="cuda", compute_type="float16")
    print("model loaded OK", flush=True)

    if len(sys.argv) > 1:
        import wave
        with wave.open(sys.argv[1], "rb") as w:
            raw = w.readframes(w.getnframes())
        audio = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    else:
        audio = np.zeros(16000, dtype=np.float32)

    segments, info = model.transcribe(audio, language="ru", beam_size=5)
    text = " ".join(s.text.strip() for s in segments).strip()
    print(f"inference OK, text: {text!r}", flush=True)
    return 0

if __name__ == "__main__":
    sys.exit(main())
