#!/usr/bin/env bash
# Настройка приватности на KDE Plasma (Wayland/KWin): виджет виден локально,
# но исключён из захвата экрана (screencast/запись/шаринг).
#
# Что делает:
#   1. Ставит/обновляет KWin-скрипт health-widget-exclude, который помечает окно
#      виджета свойством excludeFromCapture (KDE 6.6+).
#   2. Включает скрипт в kwinrc и перезагружает конфиг KWin.
#   3. Создаёт автозапуск с HEALTH_AUTO_HIDE=0 (авто-скрытие не нужно — приватность
#      обеспечивает KWin, а сам виджет остаётся видимым тебе).
#
# Требует KDE Plasma 6.6+. Идемпотентно — можно запускать повторно.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$HERE/target/release/health-widget"
SCRIPT_SRC="$HERE/kwin-script/health-widget-exclude"

if [ "${XDG_CURRENT_DESKTOP:-}" != "KDE" ]; then
  echo "⚠️  XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-<пусто>} — это не KDE."
  echo "    Этот скрипт для KDE Plasma. На GNOME используй авто-скрытие (HEALTH_AUTO_HIDE=1)."
fi

echo "==> Ставлю KWin-скрипт health-widget-exclude"
kpackagetool6 --type=KWin/Script --install "$SCRIPT_SRC" 2>/dev/null \
  || kpackagetool6 --type=KWin/Script --upgrade "$SCRIPT_SRC"

echo "==> Включаю скрипт в kwinrc"
kwriteconfig6 --file kwinrc --group Plugins --key health-widget-excludeEnabled true

echo "==> Перезагружаю конфиг KWin"
qdbus6 org.kde.KWin /KWin org.kde.KWin.reconfigure

echo "==> Автозапуск с выключенным авто-скрытием"
mkdir -p "$HOME/.config/autostart"
cat > "$HOME/.config/autostart/health-widget.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Health Widget
Comment=Приватный виджет здоровья (исключён из захвата экрана через KWin)
Exec=env HEALTH_AUTO_HIDE=0 $BIN
X-KDE-autostart-after=panel
X-GNOME-Autostart-enabled=true
EOF

echo
echo "Готово. Запусти виджет так (авто-скрытие выключено, приватность через KWin):"
echo "    HEALTH_AUTO_HIDE=0 $BIN"
echo
echo "Проверка: начни демонстрацию всего экрана — виджет виден тебе, но не в потоке."
