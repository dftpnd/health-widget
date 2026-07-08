//! Чат-колонка виджета: общий ассистент на DeepSeek. Ходим в DeepSeek НАПРЯМУЮ — на каждое
//! сообщение шеллим `curl` к OpenAI-совместимому API (идиома проекта: инструмент вместо
//! библиотеки, без нового crate). Ключ/URL/модель читаем из `.env` автопилота
//! (`LLM_BASE_URL`/`LLM_API_KEY`/`LLM_MODEL`) — та же учётка, что «уже подключена», но без
//! запуска бинаря автопилота. История эфемерная, в памяти. Картинки распознаём tesseract'ом.

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

#[derive(Clone)]
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

/// Системный промпт чата — общий ассистент.
const CHAT_SYSTEM: &str = "Ты — полезный ассистент общего назначения. \
Отвечай кратко и по делу, на языке пользователя.";

#[derive(Serialize)]
struct WireMsg<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<WireMsg<'a>>,
}

/// Собрать JSON-тело запроса к OpenAI-совместимому API: системный промпт + история
/// (Error-строки исключаем — LLM их не видит) + модель.
pub fn build_request(messages: &[ChatMessage], model: &str) -> String {
    let mut wire = vec![WireMsg { role: "system", content: CHAT_SYSTEM }];
    for m in messages {
        let role = match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Error => continue,
        };
        wire.push(WireMsg { role, content: &m.text });
    }
    serde_json::to_string(&ChatRequest { model, messages: wire })
        .unwrap_or_else(|_| "{}".to_string())
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
    pub fn send(&mut self, ctx: &egui::Context, dir: PathBuf) {
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
        // Снимок истории для фонового потока (не держим ссылку на self.messages).
        let snapshot = self.messages.clone();
        self.sending.store(true, Ordering::Relaxed);
        let sending = self.sending.clone();
        let inbox = self.inbox.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let msg = match run_chat(&dir, &snapshot) {
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

/// Прочитать LLM-настройки из `.env` автопилота: (base_url, api_key, model).
/// Строки-комментарии и кавычки вокруг значений игнорируем; пустой/`EMPTY` ключ — ошибка.
fn read_llm_env(dir: &Path) -> Result<(String, String, String), String> {
    let path = dir.join(".env");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("нет .env автопилота ({}): {e}", path.display()))?;
    let (mut base, mut key, mut model) = (None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            "LLM_BASE_URL" => base = Some(v),
            "LLM_API_KEY" => key = Some(v),
            "LLM_MODEL" => model = Some(v),
            _ => {}
        }
    }
    let base = base.filter(|s| !s.is_empty()).ok_or("нет LLM_BASE_URL в .env")?;
    let key = key
        .filter(|s| !s.is_empty() && s != "EMPTY")
        .ok_or("нет LLM_API_KEY в .env")?;
    let model = model
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "deepseek-chat".to_string());
    Ok((base, key, model))
}

/// Один запрос к DeepSeek напрямую: собрать тело из истории и POST'нуть через curl на
/// `{base}/chat/completions`, вернуть `choices[0].message.content`. Тело — в stdin curl
/// (не в argv), логи сети — в stderr.
fn run_chat(dir: &Path, messages: &[ChatMessage]) -> Result<String, String> {
    let (base, key, model) = read_llm_env(dir)?;
    let body = build_request(messages, &model);
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let mut child = Command::new("curl")
        .args(["-sS", "--max-time", "60", "-X", "POST", &url])
        .args(["-H", "Content-Type: application/json"])
        .args(["-H", &format!("Authorization: Bearer {key}")])
        .args(["--data-binary", "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("не запустить curl: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "нет stdin у curl".to_string())?
        .write_all(body.as_bytes())
        .map_err(|e| format!("stdin: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("ожидание curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("сеть/curl: {}", err.lines().last().unwrap_or("ошибка")));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        let head: String = String::from_utf8_lossy(&out.stdout).chars().take(200).collect();
        format!("плохой ответ API: {head}")
    })?;
    if let Some(content) = v.pointer("/choices/0/message/content").and_then(|c| c.as_str()) {
        let t = content.trim();
        if t.is_empty() {
            return Err("пустой ответ LLM".to_string());
        }
        return Ok(t.to_string());
    }
    if let Some(err) = v.pointer("/error/message").and_then(|c| c.as_str()) {
        return Err(format!("DeepSeek: {err}"));
    }
    let head: String = String::from_utf8_lossy(&out.stdout).chars().take(200).collect();
    Err(format!("неожиданный ответ API: {head}"))
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

/// Есть ли у tesseract русский языковой пакет (иначе `-l rus+eng` падает целиком).
fn tesseract_has_rus() -> bool {
    Command::new("tesseract")
        .arg("--list-langs")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == "rus")
        })
        .unwrap_or(false)
}

/// OCR файла-картинки: `rus+eng`, если стоит русский пакет, иначе откат на `eng`
/// (без пакета rus tesseract с `-l rus+eng` не запускается вовсе). Текст — из stdout.
pub fn ocr_file(path: &Path) -> Result<String, String> {
    let langs = if tesseract_has_rus() { "rus+eng" } else { "eng" };
    let out = Command::new("tesseract")
        .arg(path)
        .arg("stdout")
        .args(["-l", langs])
        .output()
        .map_err(|e| format!("tesseract не запущен: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("tesseract ошибка: {}", err.lines().last().unwrap_or("?")));
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
    fn build_request_has_system_model_and_skips_errors() {
        let msgs = vec![
            m(Role::User, "привет"),
            m(Role::Assistant, "здравствуй"),
            m(Role::Error, "сеть упала"),
        ];
        let j = build_request(&msgs, "deepseek-chat");
        assert!(j.contains("\"model\":\"deepseek-chat\""));
        assert!(j.contains("\"role\":\"system\""));
        assert!(j.contains("\"role\":\"user\""));
        assert!(j.contains("\"role\":\"assistant\""));
        assert!(j.contains("здравствуй"));
        assert!(!j.contains("сеть упала"), "Error-строка не должна уходить в LLM");
    }

    #[test]
    fn read_llm_env_parses_skips_comments_and_quotes() {
        let dir = std::env::temp_dir().join("hw-chat-env-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".env"),
            "# комментарий\nLLM_BASE_URL=https://api.deepseek.com/v1\nLLM_API_KEY=\"sk-x\"\nLLM_MODEL=deepseek-chat\n",
        )
        .unwrap();
        let (base, key, model) = read_llm_env(&dir).unwrap();
        assert_eq!(base, "https://api.deepseek.com/v1");
        assert_eq!(key, "sk-x");
        assert_eq!(model, "deepseek-chat");
    }

    #[test]
    fn read_llm_env_rejects_empty_key() {
        let dir = std::env::temp_dir().join("hw-chat-env-test-empty");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env"), "LLM_BASE_URL=u\nLLM_API_KEY=EMPTY\n").unwrap();
        assert!(read_llm_env(&dir).is_err(), "EMPTY-ключ должен быть ошибкой");
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
