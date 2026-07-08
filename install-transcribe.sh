#!/usr/bin/env bash
# Установка онлайн-транскрипции (faster-whisper) для health-widget.
#
# Ставит faster-whisper (модель Whisper large-v3, движок CTranslate2) в ОТДЕЛЬНЫЙ venv на
# Python 3.12 в data-каталоге виджета (~/.local/share/health-widget/venv-whisper). Инференс
# на GPU (CUDA). CUDA-либы ставятся pip-колёсами — системный CUDA toolkit не нужен.
# Идемпотентно — повторный запуск ничего не ломает.
#
# После установки запусти виджет: под каждым включённым осциллографом появится транскрипция
# фразами. Отключить — HEALTH_TRANSCRIBE=0.
set -euo pipefail

BASE="${XDG_DATA_HOME:-$HOME/.local/share}/health-widget"
VENV="$BASE/venv-whisper"
MODEL="${WHISPER_MODEL:-large-v3}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$BASE"

echo "==> venv (Python 3.12): $VENV"
if [ ! -x "$VENV/bin/python" ]; then
  uv venv "$VENV" --python 3.12
fi

echo "==> ставлю faster-whisper и CUDA-либы (колёса)"
uv pip install --python "$VENV/bin/python" \
  faster-whisper nvidia-cublas-cu12 nvidia-cudnn-cu12

echo "==> копирую хелпер и базы терминов в $BASE (базы — только если их ещё нет)"
cp "$SCRIPT_DIR/scripts/whisper_stream.py" "$BASE/whisper_stream.py"
[ -f "$BASE/it_hotwords.txt" ]    || cp "$SCRIPT_DIR/scripts/it_hotwords.txt" "$BASE/it_hotwords.txt"
[ -f "$BASE/it_corrections.tsv" ] || cp "$SCRIPT_DIR/scripts/it_corrections.tsv" "$BASE/it_corrections.tsv"

echo "==> предзагрузка модели $MODEL и проверка GPU-инференса"
# CUDA-либы стоят колёсами внутри venv, не на пути линкера — smoke-скрипт guard'а не имеет,
# поэтому выставляем путь здесь (whisper_stream.py в рантайме чинит это сам).
SITE="$VENV/lib/python3.12/site-packages"
export LD_LIBRARY_PATH="$SITE/nvidia/cublas/lib:$SITE/nvidia/cudnn/lib:${LD_LIBRARY_PATH:-}"
"$VENV/bin/python" "$SCRIPT_DIR/scripts/whisper_smoke.py"

echo
echo "Готово. Whisper-транскрипция установлена в $BASE"
echo "Запусти виджет и включи каналы 🎤 / 🔊 — текст пойдёт под осциллографами."
echo "Переопределения: WHISPER_MODEL, WHISPER_PYTHON, WHISPER_DEVICE, WHISPER_COMPUTE; HEALTH_TRANSCRIBE=0 (выкл)."
echo "Базы IT-терминов: $BASE/it_hotwords.txt (hotwords), $BASE/it_corrections.tsv (пост-коррекция) — правь под себя."
