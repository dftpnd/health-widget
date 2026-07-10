
use std::process::{Command, Stdio};
use std::time::Duration;

const RESOURCE_CLASS: &str = "health-widget";

pub const CLIP_CAPTION: &str = "hw-clip";

const DOTOOL_PIPE: &str = "/tmp/dotool-pipe";

fn dotool_bin(name: &str) -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".local").join("bin").join(name))
}

fn for_our_window(inner: &str) -> String {
    format!(
        "var l = workspace.windowList ? workspace.windowList() : workspace.clientList();\n\
         for (var i = 0; i < l.length; i++) {{ var w = l[i];\n\
         if (w && w.resourceClass && String(w.resourceClass) === \"{RESOURCE_CLASS}\" \
         && String(w.caption) !== \"{CLIP_CAPTION}\") {{ {inner} }} }}\n"
    )
}

fn for_clip_window(inner: &str) -> String {
    format!(
        "var l = workspace.windowList ? workspace.windowList() : workspace.clientList();\n\
         for (var i = 0; i < l.length; i++) {{ var w = l[i];\n\
         if (w && w.resourceClass && String(w.resourceClass) === \"{RESOURCE_CLASS}\" \
         && String(w.caption) === \"{CLIP_CAPTION}\") {{ {inner} }} }}\n"
    )
}

pub fn set_clip_position(x: i32, y: i32) -> bool {
    let body = for_clip_window(&format!(
        "w.keepAbove = true; var g = w.frameGeometry; \
         w.frameGeometry = {{ x: {x}, y: {y}, width: g.width, height: g.height }}; \
         var g2 = w.frameGeometry; \
         print(\"HWC-GEOM \" + Math.round(g2.x) + \" \" + Math.round(g2.y));"
    ));
    run_kwin_script(&body, "clipmove")
}

pub fn set_keep_above(on: bool) -> bool {
    run_kwin_script(&for_our_window(&format!("w.keepAbove = {on};")), "keepabove")
}

pub fn set_position(x: i32, y: i32) -> bool {
    let body = for_our_window(&format!(
        "var g = w.frameGeometry; w.frameGeometry = {{ x: {x}, y: {y}, width: g.width, height: g.height }}; \
         var g2 = w.frameGeometry; \
         print(\"HW-GEOM x=\" + Math.round(g2.x) + \" y=\" + Math.round(g2.y));"
    ));
    run_kwin_script(&body, "move")
}

pub fn move_by(dx: i32, dy: i32) -> bool {
    let body = for_our_window(&format!(
        "var g = w.frameGeometry; w.frameGeometry = {{ x: g.x + {dx}, y: g.y + {dy}, width: g.width, height: g.height }}; \
         var g2 = w.frameGeometry; \
         print(\"HW-GEOM x=\" + Math.round(g2.x) + \" y=\" + Math.round(g2.y));"
    ));
    run_kwin_script(&body, "moveby")
}

pub fn parse_geom_line(line: &str) -> Option<GeomEvent> {
    if let Some(i) = line.find("HWC-GEOM ") {
        let mut it = line[i + "HWC-GEOM ".len()..].split_whitespace();
        let x: i32 = it.next()?.parse().ok()?;
        let y: i32 = it.next()?.parse().ok()?;
        return Some(GeomEvent::Clip(x, y));
    }
    if let Some(i) = line.find("HW-GEOM ") {
        let rest = &line[i..];
        return Some(GeomEvent::Main(parse_field(rest, "x=")?, parse_field(rest, "y=")?));
    }
    None
}

#[derive(Debug, PartialEq)]
pub enum GeomEvent {
    Main(i32, i32),
    Clip(i32, i32),
}

pub fn follow_geometry(mut on_event: impl FnMut(GeomEvent) + Send + 'static) {
    std::thread::spawn(move || loop {
        let child = Command::new("journalctl")
            .args(["--user", "-f", "-n", "80", "-o", "cat"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else {
            std::thread::sleep(Duration::from_secs(10));
            continue;
        };
        if let Some(out) = child.stdout.take() {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(out);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if let Some(ev) = parse_geom_line(&line) {
                    on_event(ev);
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        std::thread::sleep(Duration::from_secs(2));
    });
}

fn parse_field(line: &str, key: &str) -> Option<i32> {
    let rest = &line[line.find(key)? + key.len()..];
    let tok: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    tok.parse().ok()
}

pub fn ensure_dotoold() {
    let Some(bin) = dirs::home_dir().map(|h| h.join(".local").join("bin")) else {
        return;
    };
    let dotoold = bin.join("dotoold");
    if !dotoold.exists() {
        return;
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    if let Ok(mut child) = Command::new(&dotoold)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

fn read_two_floats(body: &str, tag: &str, marker: &str) -> Option<(f64, f64)> {
    if !run_kwin_script(body, tag) {
        return None;
    }
    std::thread::sleep(Duration::from_millis(120));
    let out = Command::new("journalctl")
        .args(["--user", "-n", "40", "--no-pager", "-o", "cat"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().rev().find(|l| l.contains(marker))?;
    let rest = &line[line.find(marker)? + marker.len()..];
    let mut it = rest.split_whitespace();
    let x: f64 = it.next()?.parse().ok()?;
    let y: f64 = it.next()?.parse().ok()?;
    Some((x, y))
}

pub fn widget_center_norm() -> Option<(f64, f64)> {
    let body = format!(
        "var s = workspace.virtualScreenSize;\n\
         var l = workspace.windowList ? workspace.windowList() : workspace.clientList();\n\
         for (var i = 0; i < l.length; i++) {{ var w = l[i];\n\
         if (w && w.resourceClass && String(w.resourceClass) === \"{RESOURCE_CLASS}\" \
         && String(w.caption) !== \"{CLIP_CAPTION}\") {{\n\
         var g = w.frameGeometry;\n\
         print(\"HW-WNORM \" + ((g.x + g.width / 2) / s.width) + \" \" + ((g.y + g.height / 2) / s.height)); }} }}\n"
    );
    read_two_floats(&body, "wnorm", "HW-WNORM")
}

pub fn cursor_pos_norm() -> Option<(f64, f64)> {
    let body = "var s = workspace.virtualScreenSize; \
                print(\"HW-CNORM \" + (workspace.cursorPos.x / s.width) + \" \" + (workspace.cursorPos.y / s.height));";
    read_two_floats(body, "cnorm", "HW-CNORM")
}

pub fn warp_cursor_norm(nx: f64, ny: f64) {
    let Some(dotoolc) = dotool_bin("dotoolc") else {
        return;
    };
    let cmd = format!("mouseto {nx:.6} {ny:.6}\n");
    let pipe = DOTOOL_PIPE.to_string();
    std::thread::spawn(move || {
        if let Ok(mut child) = Command::new(&dotoolc)
            .env("DOTOOL_PIPE", &pipe)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut si) = child.stdin.take() {
                let _ = si.write_all(cmd.as_bytes());
            }
            let _ = child.wait();
        }
    });
}

pub fn type_text(text: String) -> Result<(), String> {
    let dotoolc = dotool_bin("dotoolc").ok_or_else(|| "нет dotoolc".to_string())?;
    let mut script = String::new();
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            script.push_str("key enter\n");
        }
        if !line.is_empty() {
            script.push_str("type ");
            script.push_str(line);
            script.push('\n');
        }
    }
    let mut child = Command::new(&dotoolc)
        .env("DOTOOL_PIPE", DOTOOL_PIPE)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("dotoolc не запустился: {e}"))?;
    {
        use std::io::Write;
        let mut si = child
            .stdin
            .take()
            .ok_or_else(|| "нет stdin у dotoolc".to_string())?;
        si.write_all(script.as_bytes())
            .map_err(|e| format!("dotoolc stdin: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("dotoolc: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("dotoolc завершился с ошибкой".to_string())
    }
}

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

#[cfg(test)]
mod tests {
    use super::{parse_field, parse_geom_line, GeomEvent};

    #[test]
    fn geom_line_main_window() {
        assert_eq!(
            parse_geom_line("js: HW-GEOM x=1512 y=139"),
            Some(GeomEvent::Main(1512, 139))
        );
    }

    #[test]
    fn geom_line_clip_window() {
        assert_eq!(
            parse_geom_line("js: HWC-GEOM 1918 -7"),
            Some(GeomEvent::Clip(1918, -7))
        );
    }

    #[test]
    fn geom_line_other_is_none() {
        assert_eq!(parse_geom_line("obычная строка журнала"), None);
    }

    #[test]
    fn reads_positive_int_after_key() {
        let line = "HW-GEOM x=1918 y=741";
        assert_eq!(parse_field(line, "x="), Some(1918));
        assert_eq!(parse_field(line, "y="), Some(741));
    }

    #[test]
    fn reads_negative_int() {
        assert_eq!(parse_field("x=-5 y=10", "x="), Some(-5));
    }

    #[test]
    fn missing_key_is_none() {
        assert_eq!(parse_field("нет координат", "x="), None);
    }

    #[test]
    fn non_numeric_value_is_none() {
        assert_eq!(parse_field("x=abc", "x="), None);
    }
}
