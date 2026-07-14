
use std::io::Write;
use std::process::{Command, Stdio};

const KLIPPER_ARG_LIMIT: usize = 100_000;

fn klipper(args: &[&str]) -> Option<std::process::Output> {
    Command::new("qdbus6")
        .arg("org.kde.klipper")
        .arg("/klipper")
        .args(args)
        .output()
        .ok()
}

fn strip_trailing_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

pub fn get() -> Option<String> {
    let out = klipper(&["getClipboardContents"])?;
    out.status
        .success()
        .then(|| strip_trailing_newline(String::from_utf8_lossy(&out.stdout).into_owned()))
}

pub fn set(text: &str) -> Result<(), String> {
    if text.len() < KLIPPER_ARG_LIMIT {
        if let Some(out) = klipper(&["setClipboardContents", text]) {
            if out.status.success() {
                return Ok(());
            }
        }
    }
    set_wl_copy(text)
}

fn set_wl_copy(text: &str) -> Result<(), String> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("wl-copy не запустился: {e}"))?;
    {
        let mut si = child
            .stdin
            .take()
            .ok_or_else(|| "нет stdin у wl-copy".to_string())?;
        si.write_all(text.as_bytes())
            .map_err(|e| format!("wl-copy stdin: {e}"))?;
    }
    child.wait().map_err(|e| format!("wl-copy: {e}"))?;
    Ok(())
}

pub fn set_async(text: String) {
    std::thread::spawn(move || {
        let _ = set(&text);
    });
}

#[cfg(test)]
mod tests {
    use super::strip_trailing_newline;

    #[test]
    fn strips_single_trailing_newline() {
        assert_eq!(strip_trailing_newline("текст\n".to_string()), "текст");
    }

    #[test]
    fn keeps_inner_newlines() {
        assert_eq!(strip_trailing_newline("a\nb\n".to_string()), "a\nb");
    }

    #[test]
    fn empty_stays_empty() {
        assert_eq!(strip_trailing_newline(String::new()), "");
    }
}
