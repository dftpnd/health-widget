# Чат-колонка виджета — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Добавить в виджет вторую скрываемую колонку — общий чат-ассистент на DeepSeek, переиспользуя LLM-клиент автопилота.

**Architecture:** Автопилот получает подкоманду `autopilot chat` (JSON-история в stdin → ответ в stdout), которая зовёт существующий DeepSeek-клиент. Виджет держит историю в памяти и на каждое сообщение шеллит эту команду (идиома `hr_reply.rs`). Картинки → локальный OCR (tesseract). Вёрстка — правая `SidePanel::right`; текущий контент остаётся в `CentralPanel` как левая колонка.

**Tech Stack:** Rust (egui/eframe 0.31, serde_json), Python (autopilot: openai-SDK, pydantic-settings, pytest), внешние процессы `autopilot`/`wl-paste`/`tesseract`.

## Global Constraints

- Два репозитория: `~/projects/work-autopilot` (Python) и `~/projects/health-widget` (Rust).
- Новых crate-зависимостей в виджет НЕ добавлять (serde/serde_json/egui уже есть).
- Идиома вызова LLM — шелл-аут бинаря автопилота, как в `src/hr_reply.rs` (stdin→stdout, логи в stderr).
- История чата — эфемерная, в памяти виджета. Error-строки в LLM не отправляются.
- OCR — `tesseract <img> stdout -l rus+eng`. Предпосылка развёртывания: пакет `tesseract-ocr-rus` (в системе есть только `eng`+`osd`) — ставится вручную, кодом не решается.
- Ширина чат-колонки: `CHAT_W = 340.0`.
- Системный промпт чата — общий ассистент (без рабочего контекста профиля/вакансий).

---

### Task 1: Метод `QwenBrain.chat(messages)` в автопилоте

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/llm.py`
- Test: `~/projects/work-autopilot/tests/test_llm_chat.py`

**Interfaces:**
- Produces: `QwenBrain.chat(self, messages: list[dict[str, str]]) -> str` — принимает готовый список сообщений `{"role","content"}`, возвращает текст ответа ассистента.

- [ ] **Step 1: Написать падающий тест**

Create `~/projects/work-autopilot/tests/test_llm_chat.py`:

```python
"""QwenBrain.chat: многоходовой запрос — messages как есть уходят в клиент,
ответ ассистента возвращается строкой."""
from autopilot.config import Settings
from autopilot.llm import QwenBrain


class _Msg:
    def __init__(self, content): self.content = content
class _Choice:
    def __init__(self, content): self.message = _Msg(content)
class _Resp:
    def __init__(self, content): self.choices = [_Choice(content)]
class _Completions:
    def __init__(self, captured): self._c = captured
    def create(self, **kwargs):
        self._c.update(kwargs)
        return _Resp("привет!")
class _Chat:
    def __init__(self, captured): self.completions = _Completions(captured)
class _Client:
    def __init__(self, captured): self.chat = _Chat(captured)


def test_chat_passes_messages_and_returns_content():
    brain = QwenBrain(Settings())
    captured: dict = {}
    brain._client = _Client(captured)
    msgs = [
        {"role": "system", "content": "s"},
        {"role": "user", "content": "привет"},
    ]
    out = brain.chat(msgs)
    assert out == "привет!"
    assert captured["messages"] == msgs
    assert captured["model"] == brain._model


def test_chat_empty_reply_raises():
    brain = QwenBrain(Settings())
    brain._client = _Client({})
    # подменяем на пустой ответ
    brain._client.chat.completions.create = lambda **k: _Resp("   ")
    try:
        brain.chat([{"role": "user", "content": "x"}])
        assert False, "ожидали RuntimeError на пустой ответ"
    except RuntimeError:
        pass
```

- [ ] **Step 2: Запустить тест — убедиться, что падает**

Run: `cd ~/projects/work-autopilot && .venv/bin/pytest tests/test_llm_chat.py -v`
Expected: FAIL — `AttributeError: 'QwenBrain' object has no attribute 'chat'`.

- [ ] **Step 3: Реализовать метод**

In `src/autopilot/llm.py`, add inside `class QwenBrain` (после `complete`, до `complete_json`):

```python
    @retry(stop=stop_after_attempt(3), wait=wait_exponential(min=1, max=10))
    def chat(self, messages: list[dict[str, str]]) -> str:
        """Многоходовой чат: messages = [{"role": "system"|"user"|"assistant",
        "content": ...}] уходят в API как есть. Возвращает текст ответа ассистента.
        Ретраи/температура — как в complete."""
        logger.debug("LLM чат: model={} сообщений={}", self._model, len(messages))
        resp = self._client.chat.completions.create(
            model=self._model,
            messages=messages,
            max_tokens=self._max_tokens,
            temperature=0.6,
            extra_body=self._extra_body(),
        )
        content = (resp.choices[0].message.content or "").strip()
        if not content:
            raise RuntimeError("LLM вернула пустой ответ")
        return content
```

- [ ] **Step 4: Запустить тест — убедиться, что проходит**

Run: `cd ~/projects/work-autopilot && .venv/bin/pytest tests/test_llm_chat.py -v`
Expected: PASS (2 passed).

- [ ] **Step 5: Commit**

```bash
cd ~/projects/work-autopilot
git add src/autopilot/llm.py tests/test_llm_chat.py
git commit -m "feat(llm): QwenBrain.chat — многоходовой запрос для чата виджета"
```

---

### Task 2: Подкоманда `autopilot chat` (handler + CLI)

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/orchestrator.py`
- Modify: `~/projects/work-autopilot/src/autopilot/__main__.py`
- Test: `~/projects/work-autopilot/tests/test_chat_reply.py`

**Interfaces:**
- Consumes: `QwenBrain.chat(messages)` (Task 1).
- Produces: `orchestrator.chat_reply(settings: Settings, messages: list[dict[str, str]]) -> str`; CLI-команда `chat` (JSON `{"messages":[...]}` в stdin → ответ в stdout).

- [ ] **Step 1: Написать падающий тест**

Create `~/projects/work-autopilot/tests/test_chat_reply.py`:

```python
"""chat_reply: общий ассистент. Системный промпт добавляется первым, если его нет;
существующий system сохраняется. LLM-клиент замокан."""
import autopilot.orchestrator as orch
from autopilot.config import Settings


def test_prepends_system_prompt(monkeypatch):
    seen: dict = {}
    monkeypatch.setattr(orch.QwenBrain, "chat",
                        lambda self, m: (seen.update(messages=m) or "ok"))
    out = orch.chat_reply(Settings(), [{"role": "user", "content": "привет"}])
    assert out == "ok"
    assert seen["messages"][0]["role"] == "system"
    assert seen["messages"][1] == {"role": "user", "content": "привет"}


def test_keeps_existing_system(monkeypatch):
    seen: dict = {}
    monkeypatch.setattr(orch.QwenBrain, "chat",
                        lambda self, m: (seen.update(messages=m) or "ok"))
    msgs = [
        {"role": "system", "content": "custom"},
        {"role": "user", "content": "hi"},
    ]
    orch.chat_reply(Settings(), msgs)
    assert seen["messages"] == msgs
```

- [ ] **Step 2: Запустить тест — убедиться, что падает**

Run: `cd ~/projects/work-autopilot && .venv/bin/pytest tests/test_chat_reply.py -v`
Expected: FAIL — `AttributeError: module 'autopilot.orchestrator' has no attribute 'chat_reply'`.

- [ ] **Step 3: Реализовать handler**

In `src/autopilot/orchestrator.py`, add near `reply_to_hr` (например сразу после него):

```python
CHAT_SYSTEM = (
    "Ты — полезный ассистент общего назначения. Отвечай кратко и по делу, "
    "на языке пользователя."
)


def chat_reply(settings: Settings, messages: list[dict[str, str]]) -> str:
    """Общий чат-ассистент виджета: история messages (user/assistant) → ответ
    ассистента. Системный промпт CHAT_SYSTEM добавляется первым, если во входе нет
    system-сообщения. Без браузера и без site-бандла."""
    brain = QwenBrain(settings)
    if not messages or messages[0].get("role") != "system":
        messages = [{"role": "system", "content": CHAT_SYSTEM}, *messages]
    return brain.chat(messages)
```

- [ ] **Step 4: Запустить тест — убедиться, что проходит**

Run: `cd ~/projects/work-autopilot && .venv/bin/pytest tests/test_chat_reply.py -v`
Expected: PASS (2 passed).

- [ ] **Step 5: Подключить CLI-команду**

In `src/autopilot/__main__.py`:

(a) добавить `"chat"` в список `choices` аргумента `command`:

```python
        choices=["login", "login-google", "once", "run", "scan-status", "cleanup", "reply-hr", "chat"],
```

(b) убедиться, что `chat_reply` импортируется из orchestrator рядом с `reply_to_hr` (добавить в существующий `from .orchestrator import ...`, где уже есть `reply_to_hr`).

(c) добавить обработку `chat` ДО загрузки бандла (chat бандл не нужен) — рядом с блоками `login`/`login-google`, перед строкой `bundle = load_site_bundle(...)`:

```python
    if args.command == "chat":
        import json
        raw = sys.stdin.read().strip()
        if not raw:
            print("пустой ввод: нет JSON истории в stdin", file=sys.stderr)
            raise SystemExit(2)
        try:
            payload = json.loads(raw)
            messages = payload["messages"]
        except (json.JSONDecodeError, KeyError, TypeError) as exc:
            print(f"плохой JSON истории: {exc}", file=sys.stderr)
            raise SystemExit(2)
        print(chat_reply(settings, messages))
        return
```

- [ ] **Step 6: Проверить команду вручную (без сети — на плохом вводе)**

Run: `cd ~/projects/work-autopilot && echo '' | .venv/bin/autopilot chat; echo "код=$?"`
Expected: в stderr «пустой ввод…», `код=2`.

Run: `cd ~/projects/work-autopilot && printf '{"messages":[{"role":"user","content":"скажи ровно слово тест"}]}' | .venv/bin/autopilot chat`
Expected (нужен рабочий ключ DeepSeek в `.env`): в stdout короткий ответ модели. Если ключа нет — непустая ошибка в stderr и ненулевой код (это ок, проверяем что stdout не мусорит).

- [ ] **Step 7: Commit**

```bash
cd ~/projects/work-autopilot
git add src/autopilot/orchestrator.py src/autopilot/__main__.py tests/test_chat_reply.py
git commit -m "feat(cli): подкоманда autopilot chat — общий чат-ассистент для виджета"
```

---

### Task 3: Модуль `chat.rs` в виджете (логика + шелл-аут)

**Files:**
- Create: `~/projects/health-widget/src/chat.rs`
- Modify: `~/projects/health-widget/src/main.rs` (добавить `mod chat;` рядом с прочими `mod`)

**Interfaces:**
- Produces:
  - `pub enum Role { User, Assistant, Error }`
  - `pub struct ChatMessage { pub role: Role, pub text: String }`
  - `pub struct ChatState` c полями `pub messages: Vec<ChatMessage>`, `pub input: String`, `pub attachment: Option<String>` и методами:
    - `pub fn is_sending(&self) -> bool`
    - `pub fn send(&mut self, ctx: &egui::Context, dir: std::path::PathBuf, bin: std::path::PathBuf)`
    - `pub fn drain_inbox(&mut self)`
    - `pub fn clear(&mut self)`
  - `pub fn ocr_clipboard() -> Result<String, String>`
  - `pub fn ocr_file(path: &std::path::Path) -> Result<String, String>`
  - свободные функции (тестируемые): `history_json(&[ChatMessage]) -> String`, `compose_message(&str, Option<&str>) -> String`

- [ ] **Step 1: Написать модуль с падающими тестами**

Create `~/projects/health-widget/src/chat.rs`:

```rust
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
```

- [ ] **Step 2: Подключить модуль**

In `src/main.rs`, add near other module declarations (после `mod audio;` … `mod transcribe;`):

```rust
mod chat;
```

- [ ] **Step 3: Запустить тесты — убедиться, что проходят**

Run: `cd ~/projects/health-widget && cargo test --release chat::tests`
Expected: PASS — `4 passed`. (Компиляция подтверждает, что `send`/`ocr_*`/`run_chat` тоже собираются.)

- [ ] **Step 4: Commit**

```bash
cd ~/projects/health-widget
git add src/chat.rs src/main.rs
git commit -m "feat(chat): модуль chat.rs — история, шелл-аут autopilot chat, OCR"
```

---

### Task 4: Вёрстка — переключатель, колонка, ресайз окна

**Files:**
- Modify: `~/projects/health-widget/src/main.rs`

**Interfaces:**
- Consumes: `chat::ChatState`, `chat::Role`, `ChatState::{drain_inbox, clear, is_sending}` (Task 3).
- Produces: поля `App { chat: chat::ChatState, chat_open: bool, width_one_col: Option<f32> }`; метод `App::draw_chat(&mut self, ui: &mut egui::Ui)`; константа `CHAT_W`.

- [ ] **Step 1: Добавить поля состояния и константу**

In `src/main.rs`:

(a) рядом с другими константами добавить:

```rust
/// Ширина скрываемой чат-колонки (точки).
const CHAT_W: f32 = 340.0;
```

(b) в `struct App { … }` добавить поля:

```rust
    /// Чат-ассистент (правая колонка). История эфемерная.
    chat: chat::ChatState,
    /// Открыта ли чат-колонка (иначе окно — одна колонка, как раньше).
    chat_open: bool,
    /// Запомненная ширина окна в режиме одной колонки — чтобы точно вернуться при закрытии чата.
    width_one_col: Option<f32>,
```

(c) в `App::new(...)` в конструкторе `Self { … }` добавить инициализацию:

```rust
            chat: chat::ChatState::default(),
            chat_open: false,
            width_one_col: None,
```

- [ ] **Step 2: Добавить метод отрисовки чат-колонки**

In `src/main.rs`, добавить метод в `impl App` (рядом с другими `fn draw_*`/хелперами App):

```rust
    /// Нарисовать содержимое чат-колонки: шапка + лента сообщений. Ввод — в Task 5.
    fn draw_chat(&mut self, ui: &mut egui::Ui) {
        self.chat.drain_inbox();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("💬 Чат")
                    .size(13.0)
                    .strong()
                    .color(egui::Color32::from_rgb(180, 200, 255)),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("очистить").clicked() {
                    self.chat.clear();
                }
            });
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("chat_log")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for msg in &self.chat.messages {
                    let (who, color) = match msg.role {
                        chat::Role::User => ("ты", egui::Color32::from_rgb(150, 210, 170)),
                        chat::Role::Assistant => ("ассистент", egui::Color32::from_rgb(180, 200, 255)),
                        chat::Role::Error => ("ошибка", egui::Color32::from_rgb(230, 120, 120)),
                    };
                    ui.label(egui::RichText::new(who).size(10.0).color(color));
                    ui.label(egui::RichText::new(&msg.text).size(14.0));
                    ui.add_space(4.0);
                }
                if self.chat.is_sending() {
                    ui.label(
                        egui::RichText::new("…думает")
                            .italics()
                            .size(12.0)
                            .color(egui::Color32::from_rgb(140, 146, 158)),
                    );
                }
            });
    }
```

- [ ] **Step 3: Кнопка-переключатель 💬 в заголовке**

In `src/main.rs`, в строке заголовка (блок `ui.with_layout(egui::Layout::right_to_left…)` рядом с `📌`), объявить флаг перед `ui.horizontal(|ui| {` заголовка:

```rust
                let mut toggle_chat = false;
```

и добавить кнопку сразу после кнопки `📌` (внутри right_to_left-лейаута):

```rust
                        if ui
                            .selectable_label(self.chat_open, "💬")
                            .on_hover_text("Чат-ассистент")
                            .clicked()
                        {
                            toggle_chat = true;
                        }
```

- [ ] **Step 4: Обработать переключение + ресайз окна**

In `src/main.rs`, рядом с `if toggle_pin { … }` (внутри той же CentralPanel-замыкания) добавить:

```rust
                if toggle_chat {
                    self.chat_open = !self.chat_open;
                    let cur = ctx.screen_rect();
                    if self.chat_open {
                        self.width_one_col = Some(cur.width());
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                            cur.width() + CHAT_W,
                            cur.height(),
                        )));
                    } else {
                        let target = self
                            .width_one_col
                            .take()
                            .unwrap_or((cur.width() - CHAT_W).max(200.0));
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                            target,
                            cur.height(),
                        )));
                    }
                }
```

- [ ] **Step 5: Показать SidePanel перед CentralPanel**

In `src/main.rs`, прямо перед строкой `let inner = egui::CentralPanel::default().frame(frame).show(ctx, |ui| {` вставить:

```rust
            if self.chat_open {
                egui::SidePanel::right("chat_panel")
                    .resizable(true)
                    .default_width(CHAT_W)
                    .frame(frame)
                    .show(ctx, |ui| {
                        self.draw_chat(ui);
                    });
            }
```

(`frame` — та же переменная `egui::Frame`, что уже собрана выше для CentralPanel; она в области видимости.)

- [ ] **Step 6: Собрать и проверить вручную**

Run: `cd ~/projects/health-widget && cargo build --release`
Expected: `Finished`.

Перезапустить виджет (как в предыдущей задаче: убить старый, `setsid ./target/release/health-widget`). Проверить:
- в заголовке есть кнопка 💬;
- клик по 💬 — справа появляется колонка «💬 Чат» с кнопкой «очистить», окно становится шире на ~340px;
- повторный клик — колонка исчезает, окно возвращается к прежней ширине.

- [ ] **Step 7: Commit**

```bash
cd ~/projects/health-widget
git add src/main.rs
git commit -m "feat(chat): вёрстка — переключатель 💬, SidePanel-колонка, ресайз окна"
```

---

### Task 5: Ввод, отправка, картинки (📎 + drag-drop)

**Files:**
- Modify: `~/projects/health-widget/src/main.rs` (метод `draw_chat`)

**Interfaces:**
- Consumes: `ChatState::{send, is_sending}`, `chat::ocr_clipboard`, `chat::ocr_file`, `compose_message` через `send` (Task 3); `self.cfg.autopilot_dir`, `self.cfg.autopilot_bin` (уже в `App`).

- [ ] **Step 1: Добавить строку ввода в `draw_chat`**

In `src/main.rs`, в конце метода `draw_chat` (после `ScrollArea…show(...)`), добавить перед закрытием метода. Метод должен принимать `ctx`, поэтому поменять сигнатуру и вызов:

(a) сигнатуру `fn draw_chat(&mut self, ui: &mut egui::Ui)` → `fn draw_chat(&mut self, ui: &mut egui::Ui, ctx: &egui::Context)`.

(b) вызов в Task 4 Step 5 `self.draw_chat(ui);` → `self.draw_chat(ui, ctx);`.

(c) добавить в конец тела метода:

```rust
        // Приём картинки перетаскиванием в область чата → OCR в attachment.
        let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        for path in dropped {
            match chat::ocr_file(&path) {
                Ok(t) => self.chat.attachment = Some(t),
                Err(e) => self.chat.messages.push(chat::ChatMessage {
                    role: chat::Role::Error,
                    text: e,
                }),
            }
        }

        ui.separator();
        // Чип с прикреплённым OCR-текстом (видно, что уйдёт вместе с сообщением).
        if let Some(att) = self.chat.attachment.clone() {
            ui.horizontal(|ui| {
                let short: String = att.chars().take(40).collect();
                ui.label(
                    egui::RichText::new(format!("📷 {short}…"))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(150, 180, 150)),
                );
                if ui.small_button("✕").on_hover_text("убрать картинку").clicked() {
                    self.chat.attachment = None;
                }
            });
        }
        let mut do_send = false;
        ui.horizontal(|ui| {
            if ui.button("📎").on_hover_text("вставить картинку из буфера (OCR)").clicked() {
                match chat::ocr_clipboard() {
                    Ok(t) => self.chat.attachment = Some(t),
                    Err(e) => self.chat.messages.push(chat::ChatMessage {
                        role: chat::Role::Error,
                        text: e,
                    }),
                }
            }
            let send_clicked = ui.button("▶").on_hover_text("отправить (Enter)").clicked();
            let resp = ui.add(
                egui::TextEdit::multiline(&mut self.chat.input)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text("сообщение…"),
            );
            // Enter отправляет, Shift+Enter — перенос строки.
            let enter = resp.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
            do_send = send_clicked || enter;
        });
        if do_send && !self.chat.is_sending() {
            self.chat.send(
                ctx,
                self.cfg.autopilot_dir.clone(),
                self.cfg.autopilot_bin.clone(),
            );
        }
```

- [ ] **Step 2: Собрать**

Run: `cd ~/projects/health-widget && cargo build --release`
Expected: `Finished`. (Если ошибка про лишний перевод строки от Enter в TextEdit — это ожидаемо решается тем, что при `do_send` мы сразу вызываем `send`, который очищает `input`.)

- [ ] **Step 3: Прогнать все тесты виджета**

Run: `cd ~/projects/health-widget && cargo test --release`
Expected: PASS (маркер-тесты + `chat::tests` — всё зелёное).

- [ ] **Step 4: Ручная проверка end-to-end**

Перезапустить виджет. Проверить (нужен рабочий ключ DeepSeek в `.env` автопилота):
- открыть чат 💬, набрать сообщение, **Enter** — появляется «ты: …», затем «…думает», затем «ассистент: …»;
- **Shift+Enter** — перенос строки, не отправляет;
- скопировать скриншот с текстом (Spectacle → буфер), нажать 📎 — появляется чип «📷 …»; отправить — в ответе виден учёт текста с картинки;
- перетащить файл-картинку в область чата — тоже появляется чип;
- «очистить» — лента пустеет; скрыть чат 💬 — окно сужается до одной колонки.

- [ ] **Step 5: Commit**

```bash
cd ~/projects/health-widget
git add src/main.rs
git commit -m "feat(chat): ввод/отправка, картинки через 📎 буфер и drag-drop (OCR)"
```

---

## Проверка плана против спеки (self-review)

- **Общий ассистент, свой промпт** → Task 2 (`CHAT_SYSTEM`, `chat_reply`). ✓
- **Переиспользование LLM автопилота через шелл-аут** → Task 2 (CLI) + Task 3 (`run_chat`). ✓
- **DeepSeek текстовый; картинки через OCR (tesseract rus+eng)** → Task 3 (`ocr_*`), Task 5 (📎/drag-drop). ✓
- **Эфемерная история в памяти; Error не уходит в LLM** → Task 3 (`ChatState`, `history_json` фильтрует Error). ✓
- **Без долгоживущего процесса (шелл на сообщение)** → Task 3 (`send`→`run_chat`). ✓
- **Вёрстка: SidePanel::right, текущий контент = левая колонка, скрытие → одна колонка** → Task 4. ✓
- **Кнопка-переключатель, ресайз окна на CHAT_W** → Task 4 (Steps 3–5). ✓
- **Индикатор «…думает», кнопка «очистить»** → Task 4 (Step 2). ✓
- **Enter отправляет, Shift+Enter перенос** → Task 5 (Step 1). ✓
- **Ошибки: LLM/сеть/OCR → красная строка, история не рвётся** → Task 3 (`run_chat` Err→Role::Error), Task 5 (OCR Err→Role::Error). ✓
- **Предпосылка tesseract-ocr-rus** → Global Constraints + отражено в ручных проверках. ✓

Типы согласованы между задачами: `ChatState`, `ChatMessage`, `Role` (Task 3) используются в Task 4/5 под теми же именами; `chat_reply`/`QwenBrain.chat` — между Task 1 и Task 2.
