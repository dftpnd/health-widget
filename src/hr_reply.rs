//! Кнопка «Ответить HR»: берём текст рекрутёра из буфера обмена, отдаём его автопилоту
//! (`autopilot reply-hr --profile <name>` — LLM DeepSeek от лица профиля), готовый ответ
//! кладём обратно в буфер. LLM/ключи/промпты — на стороне автопилота; здесь только буфер
//! обмена, запуск процесса и статус для UI. Чтение буфера — arboard (в фоновом потоке),
//! запись — на UI-потоке через egui (проверенный путь copy_text).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Состояние генерации ответа: обновляет фоновый поток, читает UI каждый кадр.
pub enum HrReplyState {
    Idle,
    Running,
    /// Ответ готов — UI кладёт его в буфер (на своём потоке) и переходит в Done.
    Ready(String),
    /// Ответ уже в буфере — показываем «✓» до следующего запуска.
    Done,
    Error(String),
}

/// Запустить: буфер → autopilot reply-hr → буфер. UI не блокируем (фоновый поток),
/// по завершении будим перерисовку.
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
                Ok(reply) => HrReplyState::Ready(reply),
                Err(e) => HrReplyState::Error(e),
            };
        }
        ctx.request_repaint();
    });
}

fn run(dir: &Path, bin: &Path, profile: &str) -> Result<String, String> {
    // 1. Текст рекрутёра из буфера обмена.
    let hr_text = arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|e| format!("буфер недоступен: {e}"))?;
    let hr_text = hr_text.trim().to_string();
    if hr_text.is_empty() {
        return Err("буфер пуст — скопируй сообщение HR".into());
    }
    // 2. autopilot reply-hr --profile <name>: текст в stdin, ответ из stdout.
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
    } // si сброшен → EOF, автопилот начинает работу
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
    Ok(reply)
}
