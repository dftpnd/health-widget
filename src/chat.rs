//! Чат-колонка виджета: общий ассистент на DeepSeek. LLM живёт в автопилоте — на каждое
//! сообщение шеллим `autopilot chat` (история JSON в stdin → ответ в stdout), как в
//! `hr_reply.rs`. История эфемерная, в памяти. Картинки распознаём локально tesseract'ом.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

/// Роль строки в ленте. Error — локальная (в LLM не уходит), красным в UI.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Error,
}

pub struct ChatMessage {
    pub role: Role,
    pub text: String,
}

/// Состояние чата: лента, поле ввода, прикреплённый OCR-текст, флаг «идёт запрос» и
/// «почтовый ящик» ответов из фонового потока (UI разгребает каждый кадр).
#[derive(Default)]
pub struct ChatState {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub attachment: Option<String>,
    sending: Arc<AtomicBool>,
    inbox: Arc<Mutex<Vec<ChatMessage>>>,
}

#[derive(Serialize)]
struct WireMsg<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct WirePayload<'a> {
    messages: Vec<WireMsg<'a>>,
}

/// Собрать JSON истории для stdin `autopilot chat`. Error-строки исключаем — LLM их не видит.
pub fn history_json(messages: &[ChatMessage]) -> String {
    let wire: Vec<WireMsg> = messages
        .iter()
        .filter_map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Error => return None,
            };
            Some(WireMsg { role, content: &m.text })
        })
        .collect();
    serde_json::to_string(&WirePayload { messages: wire })
        .unwrap_or_else(|_| "{\"messages\":[]}".to_string())
}

/// Склеить текст пользователя с OCR-вложением (если оно непустое).
pub fn compose_message(input: &str, attachment: Option<&str>) -> String {
    match attachment {
        Some(ocr) if !ocr.trim().is_empty() => {
            format!("{}\n\n[текст с картинки]:\n{}", input.trim(), ocr.trim())
        }
        _ => input.trim().to_string(),
    }
}

impl ChatState {
    /// Идёт ли сейчас запрос к LLM (UI блокирует повторную отправку).
    pub fn is_sending(&self) -> bool {
        self.sending.load(Ordering::Relaxed)
    }

    /// Отправить текущий ввод (+ вложение). Сразу добавляет user-строку; ответ/ошибку —
    /// из фонового потока. No-op при пустом вводе или пока идёт предыдущий запрос.
    pub fn send(&mut self, ctx: &egui::Context, dir: PathBuf, bin: PathBuf) {
        if self.is_sending() {
            return;
        }
        let text = compose_message(&self.input, self.attachment.as_deref());
        if text.is_empty() {
            return;
        }
        self.messages.push(ChatMessage { role: Role::User, text });
        self.input.clear();
        self.attachment = None;
        let json = history_json(&self.messages);
        self.sending.store(true, Ordering::Relaxed);
        let sending = self.sending.clone();
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let msg = match run_chat(&dir, &bin, &json) {
                Ok(reply) => ChatMessage { role: Role::Assistant, text: reply },
                Err(e) => ChatMessage { role: Role::Error, text: e },
            };
            if let Ok(mut g) = inbox.lock() {
                g.push(msg);
            }
            sending.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    /// Перенести пришедшие из фонового потока сообщения в ленту (звать каждый кадр).
    pub fn drain_inbox(&mut self) {
        if let Ok(mut g) = self.inbox.lock() {
            self.messages.append(&mut g);
        }
    }

    /// Очистить историю и ввод.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.input.clear();
        self.attachment = None;
    }
}

/// Один запрос к автопилоту: JSON истории в stdin → ответ ассистента из stdout.
fn run_chat(dir: &Path, bin: &Path, json: &str) -> Result<String, String> {
    let mut child = Command::new(bin)
        .arg("chat")
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("не запустить autopilot: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "нет stdin у процесса".to_string())?
        .write_all(json.as_bytes())
        .map_err(|e| format!("stdin: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("ожидание процесса: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("LLM не ответила: {}", err.lines().last().unwrap_or("ошибка")));
    }
    let reply = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if reply.is_empty() {
        return Err("пустой ответ LLM".to_string());
    }
    Ok(reply)
}

/// OCR картинки из буфера обмена: wl-paste PNG → временный файл → tesseract.
pub fn ocr_clipboard() -> Result<String, String> {
    let img = Command::new("wl-paste")
        .args(["-t", "image/png"])
        .output()
        .map_err(|e| format!("wl-paste: {e}"))?;
    if !img.status.success() || img.stdout.is_empty() {
        return Err("в буфере нет картинки".to_string());
    }
    let path = std::env::temp_dir().join("health-widget-ocr.png");
    std::fs::write(&path, &img.stdout).map_err(|e| format!("temp: {e}"))?;
    ocr_file(&path)
}

/// OCR файла-картинки: tesseract rus+eng, распознанный текст из stdout.
pub fn ocr_file(path: &Path) -> Result<String, String> {
    let out = Command::new("tesseract")
        .arg(path)
        .arg("stdout")
        .args(["-l", "rus+eng"])
        .output()
        .map_err(|e| format!("tesseract не запущен: {e}"))?;
    if !out.status.success() {
        return Err("tesseract вернул ошибку (установлен ли пакет rus?)".to_string());
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return Err("на картинке не распознан текст".to_string());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(role: Role, text: &str) -> ChatMessage {
        ChatMessage { role, text: text.to_string() }
    }

    #[test]
    fn history_json_skips_errors_and_marks_roles() {
        let msgs = vec![
            m(Role::User, "привет"),
            m(Role::Assistant, "здравствуй"),
            m(Role::Error, "сеть упала"),
        ];
        let j = history_json(&msgs);
        assert!(j.contains("\"role\":\"user\""));
        assert!(j.contains("\"role\":\"assistant\""));
        assert!(j.contains("здравствуй"));
        assert!(!j.contains("сеть упала"), "Error-строка не должна уходить в LLM");
    }

    #[test]
    fn compose_appends_attachment() {
        let out = compose_message("что тут?", Some("Зарплата 200к"));
        assert!(out.contains("что тут?"));
        assert!(out.contains("[текст с картинки]"));
        assert!(out.contains("Зарплата 200к"));
    }

    #[test]
    fn compose_trims_and_ignores_blank_attachment() {
        assert_eq!(compose_message("  привет  ", None), "привет");
        assert_eq!(compose_message("привет", Some("   ")), "привет");
    }

    #[test]
    fn clear_resets_everything() {
        let mut c = ChatState::default();
        c.messages.push(m(Role::User, "x"));
        c.input = "draft".into();
        c.attachment = Some("ocr".into());
        c.clear();
        assert!(c.messages.is_empty());
        assert!(c.input.is_empty());
        assert!(c.attachment.is_none());
    }
}
