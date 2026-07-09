
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

pub enum HrReplyState {
    Idle,
    Running,
    Done,
    Error(String),
}

pub fn start(
    state: Arc<Mutex<HrReplyState>>,
    ctx: egui::Context,
    dir: PathBuf,
    bin: PathBuf,
    profile: String,
) {
    if let Ok(mut g) = state.lock() {
        *g = HrReplyState::Running;
    }
    ctx.request_repaint();
    std::thread::spawn(move || {
        let result = run(&dir, &bin, &profile);
        if let Ok(mut g) = state.lock() {
            *g = match result {
                Ok(()) => HrReplyState::Done,
                Err(e) => HrReplyState::Error(e),
            };
        }
        ctx.request_repaint();
    });
}

fn run(dir: &Path, bin: &Path, profile: &str) -> Result<(), String> {
    let paste = Command::new("wl-paste")
        .arg("-n")
        .output()
        .map_err(|e| format!("wl-paste не запустился: {e}"))?;
    let hr_text = String::from_utf8_lossy(&paste.stdout).trim().to_string();
    if hr_text.is_empty() {
        return Err("буфер пуст — скопируй сообщение HR".into());
    }
    let mut child = Command::new(bin)
        .arg("reply-hr")
        .args(["--profile", profile])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("не запустить autopilot: {e}"))?;
    {
        let mut si = child
            .stdin
            .take()
            .ok_or_else(|| "нет stdin у процесса".to_string())?;
        si.write_all(hr_text.as_bytes())
            .map_err(|e| format!("stdin: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("ожидание процесса: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().last().unwrap_or("ошибка LLM");
        return Err(format!("LLM не ответила: {last}"));
    }
    let reply = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if reply.is_empty() {
        return Err("пустой ответ LLM".into());
    }
    crate::clip::set(&reply)?;
    Ok(())
}
