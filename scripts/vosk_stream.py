#!/usr/bin/env python3
"""Потоковое распознавание речи для health-widget.

Читает СЫРОЙ моно-PCM s16le 16000 Гц из stdin (его пишет Rust-канал после
ресемплинга) и печатает построчный JSON в stdout:

    {"partial": "привет как"}   — текущая гипотеза, обновляется на лету
    {"final":   "Привет, как дела"} — распознанная законченная фраза

Никакой сети: движок и модель локальные (Vosk). Логи Kaldi заглушены, чтобы
не засорять stdout — по нему идёт только JSON.

Аргументы:
    argv[1] — путь к каталогу модели Vosk (напр. vosk-model-small-ru-0.22)
"""
import sys
import json

try:
    from vosk import Model, KaldiRecognizer, SetLogLevel
except Exception as e:  # vosk не установлен в этом интерпретаторе
    sys.stderr.write(f"vosk import failed: {e}\n")
    sys.exit(3)

SAMPLE_RATE = 16000
# 0.1 c звука = 16000 * 2 байта * 0.1 = 3200 байт. Небольшой чанк — низкая задержка.
CHUNK = 3200


def main() -> int:
    if len(sys.argv) < 2:
        sys.stderr.write("usage: vosk_stream.py <model-dir>\n")
        return 2
    model_path = sys.argv[1]

    SetLogLevel(-1)  # тишина в stderr от Kaldi
    model = Model(model_path)
    rec = KaldiRecognizer(model, SAMPLE_RATE)

    stdin = sys.stdin.buffer
    last_partial = ""
    while True:
        data = stdin.read(CHUNK)
        if not data:
            break  # Rust закрыл пайп — канал выключили
        if rec.AcceptWaveform(data):
            text = json.loads(rec.Result()).get("text", "").strip()
            if text:
                sys.stdout.write(json.dumps({"final": text}, ensure_ascii=False) + "\n")
                sys.stdout.flush()
            last_partial = ""
        else:
            partial = json.loads(rec.PartialResult()).get("partial", "").strip()
            if partial and partial != last_partial:
                last_partial = partial
                sys.stdout.write(json.dumps({"partial": partial}, ensure_ascii=False) + "\n")
                sys.stdout.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
