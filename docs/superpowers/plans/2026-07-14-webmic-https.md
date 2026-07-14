# Webmic HTTPS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Веб-микрофон доступен с внешнего устройства: `https://<ip>:8787/?t=<токен>` вместо localhost+ssh-туннеля.

**Architecture:** `tiny_http` переводится в https-режим (фича `ssl-openssl`, слушает `0.0.0.0`); самоподписанный сертификат и секретный токен генерятся шеллом `openssl` в `~/.local/share/health-widget/` и переживают рестарты; страница и `/api/*` требуют `?t=<токен>` (иначе 403), прочая статика открыта; кнопка 🌐 копирует готовую ссылку с публичным IP.

**Tech Stack:** Rust (tiny_http 0.12 + ssl-openssl), openssl CLI, curl CLI, React/Vite (web/).

## Global Constraints

- Спека: `docs/superpowers/specs/2026-07-14-webmic-https-design.md`.
- Никаких комментариев в коде (CLAUDE.md).
- Ветка одна — `master`, коммиты прямо в неё.
- Идиома: шеллить CLI (`openssl`, `curl`), не тянуть новые крейты; единственное изменение зависимостей — фича `ssl-openssl` у уже используемого `tiny_http`.
- Порт `8787`, файлы: `webmic-cert.pem`, `webmic-key.pem`, `webmic-token` в `~/.local/share/health-widget/`.
- Коммиты кончаются строкой `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.

---

### Task 1: Токен — генерация, проверка путей, 403

**Files:**
- Modify: `src/webmic.rs` (импорты, `WebMic`, `serve_loop`, `handle_request`, новые fn, тесты в `mod tests`)

**Interfaces:**
- Consumes: существующие `query_param(url, key)`, `WebMic::start`, `serve_loop`, `handle_request`.
- Produces: `fn needs_token(path: &str) -> bool`, `fn token_ok(url: &str, token: &str) -> bool`, `fn ensure_token() -> Result<String, String>`, `fn data_dir() -> Option<PathBuf>`; поле `token: String` в `WebMic` (нужно Task 3 для `hint()`); `serve_loop(..., token: String)`; `handle_request(..., token: &str)`.

- [ ] **Step 1: Написать падающие тесты** — в `mod tests` в `src/webmic.rs`:

```rust
    #[test]
    fn token_required_paths() {
        assert!(needs_token("/"));
        assert!(needs_token("/index.html"));
        assert!(needs_token("/api/audio"));
        assert!(!needs_token("/worklet.js"));
        assert!(!needs_token("/assets/index-abc.js"));
    }

    #[test]
    fn token_check() {
        assert!(token_ok("/?t=secret", "secret"));
        assert!(token_ok("/api/audio?rate=1&t=secret", "secret"));
        assert!(!token_ok("/?t=wrong", "secret"));
        assert!(!token_ok("/", "secret"));
    }
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test webmic 2>&1 | tail -5`
Expected: ошибка компиляции `cannot find function needs_token` (это и есть красная фаза).

- [ ] **Step 3: Минимальная реализация чистых функций** — рядом с `query_param`:

```rust
fn needs_token(path: &str) -> bool {
    path == "/" || path == "/index.html" || path.starts_with("/api/")
}

fn token_ok(url: &str, token: &str) -> bool {
    query_param(url, "t").is_some_and(|t| t == token)
}
```

- [ ] **Step 4: Тесты зелёные**

Run: `cargo test webmic 2>&1 | tail -5`
Expected: `token_required_paths ... ok`, `token_check ... ok`.

- [ ] **Step 5: Генерация токена и врезка в сервер.**

Рядом с `web_root()`:

```rust
fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget"))
}

fn ensure_token() -> Result<String, String> {
    let dir = data_dir().ok_or_else(|| "нет data_dir".to_string())?;
    let path = dir.join("webmic-token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = Command::new("openssl")
        .args(["rand", "-hex", "16"])
        .output()
        .map_err(|e| format!("openssl: {e}"))?;
    if !out.status.success() {
        return Err("openssl rand не отработал".to_string());
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if t.is_empty() {
        return Err("пустой токен".to_string());
    }
    std::fs::write(&path, &t).map_err(|e| e.to_string())?;
    Ok(t)
}
```

В `WebMic` — поле и генерация (сигнатура `start` не меняется):

```rust
pub struct WebMic {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    token: String,
    thread: Option<JoinHandle<()>>,
}
```

В `WebMic::start` перед созданием сервера:

```rust
        let token = ensure_token()?;
```

поток получает копию токена, структура сохраняет свою:

```rust
        let thread = {
            let stop = stop.clone();
            let shared = shared.clone();
            let token = token.clone();
            std::thread::spawn(move || {
                serve_loop(server, root, channel, log, stop, shared, token);
            })
        };
        crate::telemetry::event("webmic.start", serde_json::json!({ "port": PORT }));
        Ok(Self { stop, shared, token, thread: Some(thread) })
```

`serve_loop` принимает `token: String` последним параметром и передаёт в каждый вызов: `handle_request(req, &root, channel, &log, &shared, &mut stt, &mut last_audio, &token);`

`handle_request` принимает `token: &str` последним параметром; сразу после вычисления `path`:

```rust
    if needs_token(&path) && !token_ok(&url, token) {
        let _ = req.respond(tiny_http::Response::empty(403));
        return;
    }
```

- [ ] **Step 6: Всё компилируется и тесты зелёные**

Run: `cargo test 2>&1 | tail -3`
Expected: `test result: ok.` (все существующие + 2 новых).

- [ ] **Step 7: Commit**

```bash
git add src/webmic.rs
git commit -m "feat(webmic): токен доступа — генерация openssl rand, 403 для страницы и /api без него

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: TLS — самоподписанный сертификат, https на 0.0.0.0

**Files:**
- Modify: `Cargo.toml` (фича tiny_http)
- Modify: `src/webmic.rs` (импорт `Stdio`, `ensure_cert`, `Server::https`)

**Interfaces:**
- Consumes: `data_dir()` из Task 1.
- Produces: `fn ensure_cert() -> Result<(Vec<u8>, Vec<u8>), String>` (PEM cert, PEM key); сервер слушает `0.0.0.0:8787` по https.

- [ ] **Step 1: Включить фичу в Cargo.toml**

Заменить строку `tiny_http = "0.12"` на:

```toml
tiny_http = { version = "0.12", features = ["ssl-openssl"] }
```

- [ ] **Step 2: Генерация сертификата.** В `src/webmic.rs` импорт: `use std::process::{Command, Stdio};` (сейчас только `Command`). Рядом с `ensure_token`:

```rust
fn ensure_cert() -> Result<(Vec<u8>, Vec<u8>), String> {
    let dir = data_dir().ok_or_else(|| "нет data_dir".to_string())?;
    let cert = dir.join("webmic-cert.pem");
    let key = dir.join("webmic-key.pem");
    if !(cert.exists() && key.exists()) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let status = Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650", "-subj", "/CN=health-widget", "-keyout"])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("openssl: {e}"))?;
        if !status.success() {
            return Err("openssl req не отработал".to_string());
        }
    }
    let c = std::fs::read(&cert).map_err(|e| e.to_string())?;
    let k = std::fs::read(&key).map_err(|e| e.to_string())?;
    Ok((c, k))
}
```

- [ ] **Step 3: Переключить сервер на https.** В `WebMic::start` заменить создание сервера:

```rust
        let token = ensure_token()?;
        let (cert, key) = ensure_cert()?;
        let server = tiny_http::Server::https(
            ("0.0.0.0", PORT),
            tiny_http::SslConfig { certificate: cert, private_key: key },
        )
        .map_err(|e| format!("порт {PORT}: {e}"))?;
```

- [ ] **Step 4: Компиляция и тесты**

Run: `cargo test 2>&1 | tail -3`
Expected: `test result: ok.` (openssl-крейт подтянется при сборке).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/webmic.rs
git commit -m "feat(webmic): https на 0.0.0.0 — tiny_http ssl-openssl, самоподписанный серт через openssl req

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: Ссылка-подсказка — https с публичным IP и токеном

**Files:**
- Modify: `src/webmic.rs` (убрать `connect_hint`/`ssh_hint` и тест `ssh_hint_format`; добавить `hint_url`, `host`, `public_ip`, метод `WebMic::hint`; тест `hint_url_format`)
- Modify: `src/main.rs` (в `toggle_webmic` — `w.hint()` вместо `webmic::connect_hint()`)

**Interfaces:**
- Consumes: поле `token` из Task 1, существующий `lan_ip()`, константа `PORT`.
- Produces: `pub fn WebMic::hint(&self) -> String` → `https://<host>:8787/?t=<токен>`; `fn hint_url(host: &str, token: &str) -> String`. Функции `connect_hint`/`ssh_hint` удаляются — единственный внешний вызов в `main.rs::toggle_webmic`.

- [ ] **Step 1: Падающий тест** — в `mod tests` заменить `ssh_hint_format` на:

```rust
    #[test]
    fn hint_url_format() {
        assert_eq!(
            hint_url("203.0.113.7", "abc123"),
            "https://203.0.113.7:8787/?t=abc123"
        );
    }
```

- [ ] **Step 2: Убедиться, что падает**

Run: `cargo test webmic 2>&1 | tail -5`
Expected: ошибка компиляции `cannot find function hint_url`.

- [ ] **Step 3: Реализация.** Удалить `connect_hint` и `ssh_hint`, на их месте:

```rust
fn hint_url(host: &str, token: &str) -> String {
    format!("https://{host}:{PORT}/?t={token}")
}

fn host() -> String {
    std::env::var("HEALTH_WEBMIC_HOST")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(public_ip)
        .or_else(lan_ip)
        .unwrap_or_else(|| "<ip-компа>".to_string())
}

fn public_ip() -> Option<String> {
    let out = Command::new("curl")
        .args(["-s", "--max-time", "2", "https://api.ipify.org"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!ip.is_empty() && ip.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':'))
        .then_some(ip)
}
```

В `impl WebMic` рядом с `shared()`:

```rust
    pub fn hint(&self) -> String {
        hint_url(&host(), &self.token)
    }
```

В `src/main.rs`, в `toggle_webmic`, заменить `clip::set_async(webmic::connect_hint());` внутри ветки `Ok(w)` на (порядок: сначала ссылка, потом move `w`):

```rust
            Ok(w) => {
                clip::set_async(w.hint());
                self.webmic = Some(w);
```

Подсказка кнопки 🌐 в `draw_header` (`main.rs`, ~строка 821): заменить текст `"Веб-микрофон: страница на localhost:8787.\n При включении ssh-команда и ссылка копируются в буфер."` на:

```rust
                    .on_hover_text(
                        "Веб-микрофон: https-страница на порту 8787.\n\
                         При включении ссылка с токеном копируется в буфер.",
                    )
```

- [ ] **Step 4: Тесты зелёные**

Run: `cargo test 2>&1 | tail -3`
Expected: `test result: ok.`, `hint_url_format ... ok`, `ssh_hint_format` больше не существует.

- [ ] **Step 5: Commit**

```bash
git add src/webmic.rs src/main.rs
git commit -m "feat(webmic): ссылка-подсказка https://<публичный-ip>:8787/?t=… вместо ssh-туннеля

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: Фронт — токен из URL в POST-запросах

**Files:**
- Modify: `web/src/App.jsx`
- Build: `web/dist` (гитигнорится, но нужен рантайму)

**Interfaces:**
- Consumes: серверное правило Task 1 — `/api/*` требует `?t=`; страница открыта по ссылке с `?t=<токен>`.
- Produces: POST `api/audio?rate=…&seq=…&t=<токен>`.

- [ ] **Step 1: Правка App.jsx.** После строки `const CHUNK_MS = 250` добавить:

```jsx
const TOKEN = new URLSearchParams(location.search).get('t') || ''
```

Строку fetch заменить на:

```jsx
          const r = await fetch(`api/audio?rate=${ctx.sampleRate}&seq=${seq++}&t=${TOKEN}`, {
```

- [ ] **Step 2: Пересобрать статику**

Run: `npm --prefix web run build 2>&1 | tail -3`
Expected: `✓ built in …`, обновился `web/dist/`.

- [ ] **Step 3: Commit**

```bash
git add web/src/App.jsx
git commit -m "feat(webmic-web): токен из location.search в POST аудио-чанков

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: E2E, README, пуш

**Files:**
- Modify: `src/webmic.rs` (e2e-тест `e2e_speech_to_finals` и хелперы `curl_text`/`post_pcm` — https/-k/токен)
- Modify: `README.md` (раздел «Веб-микрофон (🌐)», таблица env)

**Interfaces:**
- Consumes: всё из Task 1–4; работающий бинарь.
- Produces: живая проверка curl'ом; актуальная дока.

- [ ] **Step 1: Обновить e2e-тест под https+токен.** В `mod tests` хелпер `post_pcm` получает токен параметром, «глухой» URL — https с `-k`:

```rust
    fn read_token() -> String {
        std::fs::read_to_string(dirs::data_dir().unwrap().join("health-widget").join("webmic-token"))
            .unwrap()
            .trim()
            .to_string()
    }

    fn post_pcm(bytes: &[u8], seq: usize, token: &str) -> String {
        let tmp = std::env::temp_dir().join(format!("hw-e2e-{seq}.f32"));
        std::fs::write(&tmp, bytes).unwrap();
        let resp = curl_text(&[
            "-sk",
            "-X",
            "POST",
            "--data-binary",
            &format!("@{}", tmp.display()),
            &format!("https://127.0.0.1:8787/api/audio?rate=44100&seq={seq}&t={token}"),
        ]);
        let _ = std::fs::remove_file(&tmp);
        resp
    }
```

В `e2e_speech_to_finals` после `WebMic::start`:

```rust
        let token = read_token();
        let index = curl_text(&["-sk", &format!("https://127.0.0.1:8787/?t={token}")]);
        assert!(index.contains("id=\"root\""), "статика не отдаётся: {index}");
        let denied = curl_text(&["-sk", "-o", "/dev/null", "-w", "%{http_code}", "https://127.0.0.1:8787/"]);
        assert_eq!(denied, "403");
        let miss = curl_text(&["-sk", &format!("https://127.0.0.1:8787/../etc/passwd?t={token}")]);
        assert_eq!(miss, "404");
```

и все вызовы `post_pcm(c, seq)` → `post_pcm(c, seq, &token)`.

- [ ] **Step 2: Полный прогон тестов**

Run: `cargo test 2>&1 | tail -3`
Expected: `test result: ok.` (e2e остаётся `#[ignore]`).

- [ ] **Step 3: Живая проверка.** Внимание: заменяет работающий виджет (single-instance); только setsid, не systemd-run (cgroup-ловушка).

```bash
cargo build --release
cd /home/mgu/projects/health-widget && setsid env HEALTH_AUTO_HIDE=0 HEALTH_WEBMIC=1 ./target/release/health-widget >/dev/null 2>&1 &
sleep 4
T=$(cat ~/.local/share/health-widget/webmic-token)
curl -sk -o /dev/null -w 'no-token:%{http_code}\n' https://localhost:8787/
curl -sk -o /dev/null -w 'token:%{http_code}\n' "https://localhost:8787/?t=$T"
curl -sk -o /dev/null -w 'worklet:%{http_code}\n' https://localhost:8787/worklet.js
curl -sk -o /dev/null -w 'api-no-token:%{http_code}\n' -X POST --data-binary x https://localhost:8787/api/audio
```

Expected: `no-token:403`, `token:200`, `worklet:200`, `api-no-token:403`. Плюс: в `~/.local/share/health-widget/` появились `webmic-cert.pem`, `webmic-key.pem`, `webmic-token`; в буфере ссылка `https://<ip>:8787/?t=…`.

- [ ] **Step 4: README.** В разделе «Веб-микрофон (🌐)» заменить описание подключения: страница теперь `https://<ip>:8787/?t=<токен>` (ссылка копируется кнопкой), самоподписанный сертификат принимается один раз, для доступа из интернета — проброс TCP 8787 на роутере; ssh-туннель больше не нужен. В таблицу env добавить строку:

```markdown
| `HEALTH_WEBMIC_HOST`    | хост для ссылки веб-микрофона        | публичный IP (ipify) |
```

- [ ] **Step 5: Финальный прогон и коммит с пушем**

```bash
cargo test 2>&1 | tail -3
git add src/webmic.rs README.md
git commit -m "test(webmic)+docs: e2e под https с токеном, README про внешний доступ

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
git push origin master
```
