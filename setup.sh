#!/usr/bin/env bash
# Установка системных зависимостей и первичная настройка health-widget.
# Запускать НЕ от root (sudo будет запрошен только для apt).
set -euo pipefail

echo "==> Системные dev-библиотеки для сборки eframe/egui на Wayland+X11"
sudo apt-get update
sudo apt-get install -y \
  build-essential pkg-config \
  libwayland-dev libxkbcommon-dev \
  libx11-dev libxcursor-dev libxrandr-dev libxi-dev \
  libgl1-mesa-dev

if ! command -v cargo >/dev/null 2>&1; then
  echo "==> Rust не найден. Ставлю через rustup (без sudo)."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
else
  echo "==> Rust уже установлен: $(cargo --version)"
fi

echo "==> Кладу пример данных в ~/.config/health-widget/metrics.json (если ещё нет)"
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/health-widget"
mkdir -p "$CFG_DIR"
if [ ! -f "$CFG_DIR/metrics.json" ]; then
  cp "$(dirname "$0")/examples/metrics.json" "$CFG_DIR/metrics.json"
fi

echo "==> Сборка релиза"
cargo build --release

# На KDE Plasma настраиваем настоящую приватность (окно исключается из захвата
# через KWin), а не полагаемся на авто-скрытие.
if [ "${XDG_CURRENT_DESKTOP:-}" = "KDE" ]; then
  echo "==> KDE обнаружен — ставлю KWin-исключение из захвата экрана"
  bash "$(dirname "$0")/install-kde-privacy.sh" || \
    echo "⚠️  install-kde-privacy.sh завершился с ошибкой — настрой вручную (см. README)."
fi

echo
echo "Готово. Бинарник: $(pwd)/target/release/health-widget"
if [ "${XDG_CURRENT_DESKTOP:-}" = "KDE" ]; then
  echo "Запуск (KDE): HEALTH_AUTO_HIDE=0 ./target/release/health-widget"
  echo "  Виджет виден тебе, но исключён из захвата экрана (KWin excludeFromCapture)."
else
  echo "Запуск:  ./target/release/health-widget"
fi
echo
echo "Хоткей скрыть/показать: назначь ярлык на команду"
echo "    pkill -SIGUSR1 -x health-widget"
echo "  KDE:   System Settings → Keyboard → Shortcuts → Custom Shortcuts."
echo "  GNOME: Settings → Keyboard → Custom Shortcuts."
echo "См. README.md."
