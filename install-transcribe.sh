#!/usr/bin/env bash
# Установка онлайн-транскрипции (Vosk) для health-widget.
#
# Ставит локальный движок распознавания речи и русскую модель в data-каталог виджета
# (~/.local/share/health-widget). Всё офлайн: после установки сеть не нужна, аудио из
# захвата никуда не уходит. Идемпотентно — повторный запуск ничего не ломает.
#
# После установки просто запусти виджет: под каждым включённым осциллографом появится
# бегущая транскрипция. Отключить можно переменной HEALTH_TRANSCRIBE=0.
set -euo pipefail

BASE="${XDG_DATA_HOME:-$HOME/.local/share}/health-widget"
MODEL_NAME="vosk-model-small-ru-0.22"
MODEL_URL="https://alphacephei.com/vosk/models/${MODEL_NAME}.zip"

mkdir -p "$BASE"

echo "==> venv: $BASE/venv"
if [ ! -x "$BASE/venv/bin/python" ]; then
  python3 -m venv "$BASE/venv"
fi
"$BASE/venv/bin/pip" install --quiet --upgrade pip

echo "==> ставлю пакет vosk (нативная либа идёт в колесе — отдельный libvosk не нужен)"
"$BASE/venv/bin/pip" install --quiet vosk

echo "==> проверяю, что движок грузится"
"$BASE/venv/bin/python" -c "import vosk" \
  || { echo "ОШИБКА: vosk не импортируется в этом Python"; exit 1; }

if [ -d "$BASE/$MODEL_NAME" ]; then
  echo "==> модель уже есть: $BASE/$MODEL_NAME"
else
  echo "==> качаю русскую модель (~46 МБ): $MODEL_URL"
  curl -fL --retry 3 -o "$BASE/model.zip" "$MODEL_URL"
  echo "==> распаковываю"
  python3 -c "import zipfile,sys; zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" \
    "$BASE/model.zip" "$BASE"
  rm -f "$BASE/model.zip"
fi

echo
echo "Готово. Транскрипция установлена в $BASE"
echo "Запусти виджет и включи каналы 🎤 / 🔊 — текст пойдёт под осциллографами."
echo "Переопределения: VOSK_MODEL=<каталог модели>, VOSK_PYTHON=<python>, HEALTH_TRANSCRIBE=0 (выкл)."
