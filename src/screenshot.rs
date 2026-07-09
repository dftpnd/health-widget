//! Снимок области экрана по двум кликам — напрямую через KWin, без spectacle.
//!
//! Флоу: кнопка «Скрин» (или SIGUSR2 от Tartarus) → виджет открывает прозрачный
//! полноэкранный оверлей, ловит две точки-клика (см. main.rs), затем зовёт
//! [`grab`]: тот берёт пиксели прямоугольника прямо у KWin
//! (`org.kde.KWin.ScreenShot2.CaptureArea`, см. [`crate::kwin_shot`]) — без всякого
//! внешнего инструмента и UI. PNG кладётся в
//! `~/.local/share/health-widget/screenshots/`.
//!
//! Авторизацию на CaptureArea KWin даёт по `.desktop` виджета с ключом
//! `X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2` (ставит setup).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Статус последнего снимка — читается UI под кнопкой каждый кадр.
#[derive(Clone)]
pub enum ShotStatus {
    /// Ещё ничего не снимали.
    Idle,
    /// Оверлей открыт, ждём два клика.
    Marking,
    /// Берём пиксели у KWin и сохраняем.
    Working,
    /// Сохранено; строка — полный путь к PNG.
    Saved(String),
    /// Отменено (Esc или вырожденная область).
    Cancelled,
    /// Не удалось: KWin не авторизовал / ошибка ФС.
    Failed(String),
}

/// Гарантирует, что у виджета есть `.desktop` с правом на CaptureArea — без него
/// KWin не отдаёт пиксели. Пишем в `~/.local/share/applications` при отсутствии
/// или устаревании и обновляем кэш служб. Почти всегда файл уже актуален — тогда
/// ничего не делаем. Вызывается один раз при старте.
pub fn ensure_registered() {
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy();
    let Some(data) = dirs::data_dir() else { return };
    let dir = data.join("applications");
    let file = dir.join("health-widget.desktop");
    let want = format!(
        "[Desktop Entry]\nType=Application\nName=Health Widget\nExec={exe}\n\
         Icon=applications-utilities\nTerminal=false\nNoDisplay=true\nCategories=Utility;\n\
         X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2\n"
    );
    if std::fs::read_to_string(&file).ok().as_deref() == Some(want.as_str()) {
        return; // уже актуально
    }
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::write(&file, &want).is_ok() {
        // Обновляем ksycoca, чтобы KWin увидел право сразу (best-effort).
        let _ = std::process::Command::new("kbuildsycoca6").spawn();
    }
}

/// Каталог снимков: `~/.local/share/health-widget/screenshots/`.
fn shot_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget").join("screenshots"))
}

/// Локальный таймстамп `YYYY-MM-DD_HH-MM-SS` для имени файла. Берём из SQLite
/// (rusqlite уже в зависимостях) — корректная локальная зона без новых крейтов.
fn timestamp() -> String {
    rusqlite::Connection::open_in_memory()
        .and_then(|c| {
            c.query_row(
                "SELECT strftime('%Y-%m-%d_%H-%M-%S','now','localtime')",
                [],
                |r| r.get::<_, String>(0),
            )
        })
        .unwrap_or_else(|_| "shot".to_string())
}

/// По прямоугольнику в логических координатах экрана (как геометрия окон KWin):
/// берёт пиксели напрямую у KWin и сохраняет PNG. Всё в фоновом потоке; статус и
/// перерисовка идут через `status`/`ctx`.
pub fn grab(x: i32, y: i32, w: u32, h: u32, ctx: egui::Context, status: Arc<Mutex<ShotStatus>>) {
    let finish = |status: &Arc<Mutex<ShotStatus>>, ctx: &egui::Context, s: ShotStatus| {
        *status.lock().unwrap() = s;
        ctx.request_repaint();
    };

    if w == 0 || h == 0 {
        return finish(&status, &ctx, ShotStatus::Cancelled);
    }
    let dir = match shot_dir() {
        Some(d) => d,
        None => return finish(&status, &ctx, ShotStatus::Failed("нет data-dir".into())),
    };

    std::thread::spawn(move || {
        // Дать оверлею закрыться, чтобы его вуаль не попала в кадр.
        std::thread::sleep(Duration::from_millis(120));

        let img = match crate::kwin_shot::capture_area(x, y, w, h) {
            Ok(i) => i,
            Err(e) => return finish(&status, &ctx, ShotStatus::Failed(e)),
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return finish(&status, &ctx, ShotStatus::Failed(format!("mkdir: {e}")));
        }
        let path = dir.join(format!("{}.png", timestamp()));
        match img.save(&path) {
            Ok(_) => {
                // Кладём в буфер ПУТЬ к PNG (а не саму картинку): его удобно вставить во
                // встроенный терминал колонки и скормить, напр., `claude ... < <путь>`.
                // wl-copy демонизируется и держит содержимое сам — просто spawn со stdin.
                let p = path.display().to_string();
                let _ = crate::clip::set(&p);
                finish(&status, &ctx, ShotStatus::Saved(path.display().to_string()))
            }
            Err(e) => finish(&status, &ctx, ShotStatus::Failed(format!("save: {e}"))),
        }
    });
}
