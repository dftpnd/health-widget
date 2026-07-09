
use std::io::Write;
use std::process::{Command, Stdio};

pub fn set(text: &str) -> Result<(), String> {
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
