//! Управление окном через KWin.
//!
//! На Wayland клиент многого не может сам: ни встать «поверх всех» (eframe/winit `always_on_top`
//! там no-op), ни узнать/задать свою позицию. Зато это умеет KWin — дёргаем его скриптами по
//! D-Bus (матч окна по `resourceClass`, как в kwin-script/health-widget-exclude). Скрипты
//! одноразовые: пишем .js во временный файл, грузим (`loadScript`), выполняем (`Script.run`),
//! выгружаем. Всё через `qdbus6` — без новых зависимостей, в стиле `detect.rs`/`audio.rs`.

use std::process::Command;
use std::time::Duration;

/// resourceClass нашего окна (совпадает с kwin-script excludeFromCapture).
const RESOURCE_CLASS: &str = "health-widget";

/// Обёртка над окном по resourceClass: `for` по всем окнам с проверкой класса.
/// `inner` — тело, где `w` — наше окно.
fn for_our_window(inner: &str) -> String {
    format!(
        "var l = workspace.windowList ? workspace.windowList() : workspace.clientList();\n\
         for (var i = 0; i < l.length; i++) {{ var w = l[i];\n\
         if (w && w.resourceClass && String(w.resourceClass) === \"{RESOURCE_CLASS}\") {{ {inner} }} }}\n"
    )
}

/// Выставить/снять «поверх всех» (keepAbove). true — если скрипт выполнился.
pub fn set_keep_above(on: bool) -> bool {
    run_kwin_script(&for_our_window(&format!("w.keepAbove = {on};")), "keepabove")
}

/// Активировать окно (дать ему клавиатурный фокус компоновщика) и разминимизировать.
/// На Wayland клиент не может активировать себя сам — просит KWin. true — если скрипт выполнился.
pub fn activate() -> bool {
    run_kwin_script(
        &for_our_window("w.minimized = false; workspace.activeWindow = w;"),
        "activate",
    )
}

/// Прочитать `internalId` активного окна (того, что сейчас в фокусе компоновщика). None —
/// не удалось. Значение печатается скриптом в лог KWin, откуда читаем через journalctl —
/// тем же приёмом, что и [`get_position`]. Нужно, чтобы запомнить окно ДО активации нашего
/// и вернуть ему фокус повторным нажатием кнопки.
pub fn get_active_window_id() -> Option<String> {
    if !run_kwin_script(
        "var a = workspace.activeWindow; if (a) print(\"HW-ACTIVE id=\" + a.internalId);",
        "active",
    ) {
        return None;
    }
    std::thread::sleep(Duration::from_millis(120)); // дать строке дойти до journal
    let out = Command::new("journalctl")
        .args(["--user", "-n", "40", "--no-pager", "-o", "cat"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().rev().find(|l| l.contains("HW-ACTIVE id="))?;
    let rest = &line[line.find("id=")? + 3..];
    let id: String = rest.split_whitespace().next()?.to_string();
    (!id.is_empty()).then_some(id)
}

/// Активировать окно по его `internalId` (цикл по всем окнам, не только нашим). Используется
/// для возврата фокуса окну, которое было активным до фокуса на чат. true — если скрипт выполнился.
pub fn activate_window_by_id(id: &str) -> bool {
    let body = format!(
        "var l = workspace.windowList ? workspace.windowList() : workspace.clientList();\n\
         for (var i = 0; i < l.length; i++) {{ var w = l[i];\n\
         if (w && String(w.internalId) === \"{id}\") {{ w.minimized = false; workspace.activeWindow = w; }} }}\n"
    );
    run_kwin_script(&body, "activate-id")
}

/// Переместить окно в (x, y), сохранив размер. true — если скрипт выполнился.
pub fn set_position(x: i32, y: i32) -> bool {
    let body = for_our_window(&format!(
        "var g = w.frameGeometry; w.frameGeometry = {{ x: {x}, y: {y}, width: g.width, height: g.height }};"
    ));
    run_kwin_script(&body, "move")
}

/// Прочитать текущую позицию окна из KWin. None — если не удалось.
/// Значение печатается скриптом в лог KWin, откуда читаем через journalctl.
pub fn get_position() -> Option<(i32, i32)> {
    let body = for_our_window(
        "var g = w.frameGeometry; print(\"HW-GEOM x=\" + Math.round(g.x) + \" y=\" + Math.round(g.y));",
    );
    if !run_kwin_script(&body, "geom") {
        return None;
    }
    std::thread::sleep(Duration::from_millis(120)); // дать строке дойти до journal
    let out = Command::new("journalctl")
        .args(["--user", "-n", "40", "--no-pager", "-o", "cat"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().rev().find(|l| l.contains("HW-GEOM"))?;
    Some((parse_field(line, "x=")?, parse_field(line, "y=")?))
}

/// Достать целое после `key` в строке вида `... x=1918 y=741`.
fn parse_field(line: &str, key: &str) -> Option<i32> {
    let rest = &line[line.find(key)? + key.len()..];
    let tok: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    tok.parse().ok()
}

/// Загрузить, выполнить и выгрузить одноразовый KWin-скрипт. `tag` — суффикс имени файла.
fn run_kwin_script(body: &str, tag: &str) -> bool {
    let path = std::env::temp_dir().join(format!("health-widget-{tag}.js"));
    if std::fs::write(&path, body).is_err() {
        return false;
    }
    let p = path.to_string_lossy();

    let id = match qdbus(&[
        "org.kde.KWin",
        "/Scripting",
        "org.kde.kwin.Scripting.loadScript",
        &p,
    ]) {
        Some(out) => out.trim().to_string(),
        None => return false,
    };
    if id.parse::<i32>().is_err() {
        return false;
    }

    let obj = format!("/Scripting/Script{id}");
    let ran = qdbus(&["org.kde.KWin", &obj, "org.kde.kwin.Script.run"]).is_some();

    let _ = qdbus(&[
        "org.kde.KWin",
        "/Scripting",
        "org.kde.kwin.Scripting.unloadScript",
        &p,
    ]);
    ran
}

fn qdbus(args: &[&str]) -> Option<String> {
    let out = Command::new("qdbus6").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}
