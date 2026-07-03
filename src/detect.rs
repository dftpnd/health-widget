//! Детект активного захвата экрана на Wayland (best-effort), кросс-десктоп.
//!
//! Проблема: детект захвата зависит от композитора.
//!  * GNOME/Mutter выставляет объекты под `org.gnome.Mutter.ScreenCast` — их видно через `busctl`.
//!  * KDE/KWin такого сервиса НЕ имеет (Mutter там нет вообще), поэтому старый детект молчал,
//!    и виджет оставался виден при шаринге.
//!
//! Решение: основной сигнал берём из PipeWire (`pw-dump`), а не из D-Bus конкретного композитора.
//! И GNOME, и KDE гонят шаринг через `xdg-desktop-portal` → PipeWire. Когда экран реально
//! захватывается, композитор создаёт PipeWire-ноду `media.class = "Stream/Output/Video"`,
//! которая переходит в состояние `running`. Это и ловим. Вебкамера — это `Video/Source`,
//! а не `Stream/Output/Video`, так что ложных срабатываний от камеры нет.
//!
//! Порядок: сначала PipeWire (кросс-десктоп), затем — как резерв — старый Mutter/D-Bus путь
//! (на случай если `pw-dump` недоступен, а GNOME есть). Любой из двух → считаем захват активным.
//!
//! Ограничения: это по-прежнему эвристика поверх «шарь одно окно» + ручного SIGUSR1-тумблера.

use std::process::Command;

/// Доступен ли хоть один способ детекта: PipeWire (`pw-dump`) или Mutter (`busctl`).
pub fn available() -> bool {
    has_tool("pw-dump", &["--version"]) || has_tool("busctl", &["--version"])
}

/// true, если сейчас активна хотя бы одна сессия захвата экрана.
pub fn screencast_active() -> bool {
    pipewire_screencast_active() || mutter_screencast_active()
}

/// Есть ли исполняемый инструмент (по успешному коду возврата на пробной команде).
fn has_tool(bin: &str, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Кросс-десктопный детект через PipeWire.
///
/// Ищем PipeWire-ноду типа `PipeWire:Interface:Node`, у которой
/// `media.class == "Stream/Output/Video"` и `state == "running"` — это композитор
/// (kwin_wayland / gnome-shell), который прямо сейчас отдаёт поток захвата экрана.
fn pipewire_screencast_active() -> bool {
    let output = match Command::new("pw-dump").output() {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };

    let objs: Vec<serde_json::Value> = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => return false,
    };

    objs.iter().any(|o| {
        o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Node") && {
            let info = match o.get("info") {
                Some(i) => i,
                None => return false,
            };
            let running = info.get("state").and_then(|s| s.as_str()) == Some("running");
            let is_screencast = info
                .get("props")
                .and_then(|p| p.get("media.class"))
                .and_then(|c| c.as_str())
                == Some("Stream/Output/Video");
            running && is_screencast
        }
    })
}

/// Резервный путь для GNOME: активная сессия появляется под `org.gnome.Mutter.ScreenCast`.
fn mutter_screencast_active() -> bool {
    let output = Command::new("busctl")
        .args([
            "--user",
            "tree",
            "org.gnome.Mutter.ScreenCast",
            "--no-pager",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            // Активная сессия захвата появляется строго по этому пути.
            // (НЕ подстрока "/Session" — она ложно матчит "/org/gnome/SessionManager".)
            text.contains("/org/gnome/Mutter/ScreenCast/Session")
                || text.contains("/org/gnome/Mutter/RemoteDesktop/Session")
        }
        _ => false,
    }
}
