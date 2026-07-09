
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub enum ShotStatus {
    Idle,
    Marking,
    Working,
    Saved(String),
    Cancelled,
    Failed(String),
}

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
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::write(&file, &want).is_ok() {
        let _ = std::process::Command::new("kbuildsycoca6").spawn();
    }
}

fn shot_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget").join("screenshots"))
}

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
                let p = path.display().to_string();
                let _ = crate::clip::set(&p);
                finish(&status, &ctx, ShotStatus::Saved(path.display().to_string()))
            }
            Err(e) => finish(&status, &ctx, ShotStatus::Failed(format!("save: {e}"))),
        }
    });
}
