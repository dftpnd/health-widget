# Локальный просмотр колов в браузере — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Кнопка «📺 Колы» в виджете поднимает HTTP-сервер на `127.0.0.1:8788` и открывает вкладку со списком склеенных колов, где видео играет родным `<video>` с рабочей перемоткой.

**Architecture:** Новый модуль `src/calls_web.rs` на vendored `tiny_http` (тот же крейт, что у `webmic.rs`, но отдельный сервер на loopback без TLS и без токена). Три маршрута: страница (`include_str!` одного самодостаточного HTML), `GET /api/calls` (JSON из скана папок колов + имена из SQLite), `GET /video/<id>` (отдача `combined.mp4` с собственной обработкой заголовка `Range`, которой в tiny_http нет). Метаданные колов даёт новая `transcript_log::call_meta()`.

**Tech Stack:** Rust, `tiny_http` (vendor/), `rusqlite`, `serde_json`, `egui`; фронт — голые HTML/CSS/JS без билд-шага и без CDN.

**Spec:** `docs/superpowers/specs/2026-07-24-calls-web-viewer-design.md`

## Global Constraints

- **Никаких комментариев в исходниках** — ни `//`, ни `///`, ни `/* */` в Rust. Не возвращать удалённые комментарии при правках. В HTML/JS страницы — тоже без комментариев.
- **Строки UI — по-русски.**
- **Ветка одна — `master`.** Не создавать git-веток.
- **Никаких новых зависимостей** в `Cargo.toml`. Внешние действия — через готовые CLI (`xdg-open`).
- **Порт `127.0.0.1:8788`**, чистый HTTP, без токена. Сервер живёт до конца процесса виджета.
- Тесты — юнит-тесты в `#[cfg(test)] mod tests` внутри модуля (идиома проекта: см. `src/mux.rs`, `src/webmic.rs`), запуск `cargo test`.

## File Structure

| Файл | Ответственность |
|---|---|
| `src/calls_web.rs` (создать) | Сервер, маршруты, Range, скан колов, JSON |
| `src/calls_web.html` (создать) | Страница целиком: разметка, стили, скрипт |
| `src/transcript_log.rs` (изменить) | Новая `pub fn call_meta()` + `pub struct CallMeta` |
| `src/main.rs` (изменить) | `mod calls_web;`, поле статуса, кнопка и строка статуса в `draw_call` |

---

### Task 1: Разбор заголовка Range

**Files:**
- Create: `src/calls_web.rs`
- Modify: `src/main.rs:32` (список `mod`)

**Interfaces:**
- Consumes: ничего
- Produces: `fn parse_range(header: &str, len: u64) -> Option<(u64, u64)>` — возвращает **включительные** границы `(start, end)`; `None` означает «ответить 416». Используется в Task 3.

- [ ] **Step 1: Создать модуль с падающим тестом**

Создать `src/calls_web.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_forms_parse_into_inclusive_bounds() {
        assert_eq!(parse_range("bytes=0-499", 1000), Some((0, 499)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-500", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=0-99999", 1000), Some((0, 999)));
    }

    #[test]
    fn broken_or_unsatisfiable_ranges_are_rejected() {
        assert_eq!(parse_range("байты пожалуйста", 1000), None);
        assert_eq!(parse_range("bytes=abc-def", 1000), None);
        assert_eq!(parse_range("bytes=999999-", 1000), None);
        assert_eq!(parse_range("bytes=0-10,20-30", 1000), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }
}
```

Добавить в `src/main.rs` строкой после `mod mux;` (строка 32):

```rust
mod calls_web;
```

- [ ] **Step 2: Убедиться, что тест падает**

Run: `cargo test calls_web 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'parse_range' in this scope`

- [ ] **Step 3: Реализовать parse_range**

В начало `src/calls_web.rs` (перед блоком тестов):

```rust
fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (from, to) = spec.split_once('-')?;
    let (from, to) = (from.trim(), to.trim());
    let (start, end) = if from.is_empty() {
        let want: u64 = to.parse().ok()?;
        if want == 0 {
            return None;
        }
        (len.saturating_sub(want), len - 1)
    } else {
        let start: u64 = from.parse().ok()?;
        let end = if to.is_empty() {
            len - 1
        } else {
            to.parse::<u64>().ok()?.min(len - 1)
        };
        (start, end)
    };
    if start > end || start >= len {
        return None;
    }
    Some((start, end))
}
```

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `cargo test calls_web 2>&1 | tail -20`
Expected: PASS — `2 passed`. Предупреждение `function 'parse_range' is never used` в обычной сборке ожидаемо и уйдёт в Task 3.

- [ ] **Step 5: Коммит**

```bash
git add src/calls_web.rs src/main.rs
git commit -m "feat(calls_web): разбор HTTP-заголовка Range"
```

---

### Task 2: Список склеенных колов

**Files:**
- Modify: `src/transcript_log.rs` (добавить `CallMeta` и `call_meta()` рядом с `calls_dir()`, ~строка 260)
- Modify: `src/calls_web.rs`

**Interfaces:**
- Consumes: `crate::transcript_log::calls_dir() -> Option<PathBuf>` (уже есть)
- Produces:
  - `pub struct CallMeta { pub name: String, pub started: String, pub ended: String }` в `transcript_log`
  - `pub fn call_meta() -> HashMap<i64, CallMeta>` в `transcript_log`
  - `type MetaSource = fn() -> HashMap<i64, CallMeta>` в `calls_web`
  - `struct Row { id: i64, name: String, started: String, ended: String, size: u64 }` в `calls_web`
  - `fn scan_calls(root: &Path, meta: &HashMap<i64, CallMeta>) -> Vec<Row>`
  - `fn rows_json(rows: &[Row]) -> String`

  Всё это использует Task 3.

- [ ] **Step 1: Написать падающие тесты**

В `src/calls_web.rs`, внутрь `mod tests`, добавить:

```rust
    fn mkcall(root: &Path, id: &str, size: usize) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("combined.mp4"), vec![b'x'; size]).unwrap();
    }

    #[test]
    fn scan_keeps_only_calls_with_nonempty_video() {
        let tmp = std::env::temp_dir().join(format!("hw-web-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        mkcall(&tmp, "1", 10);
        mkcall(&tmp, "3", 0);
        std::fs::create_dir_all(tmp.join("4")).unwrap();
        std::fs::write(tmp.join("4").join("mic.wav"), b"x").unwrap();
        mkcall(&tmp, "9", 20);
        std::fs::create_dir_all(tmp.join("notanumber")).unwrap();

        let mut meta = HashMap::new();
        meta.insert(
            9,
            CallMeta {
                name: "Скрининг Ozon".to_string(),
                started: "2026-07-23 14:05:11".to_string(),
                ended: "2026-07-23 14:51:02".to_string(),
            },
        );

        let rows = scan_calls(&tmp, &meta);
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![9, 1]);
        assert_eq!(rows[0].name, "Скрининг Ozon");
        assert_eq!(rows[0].size, 20);
        assert_eq!(rows[1].name, "#1");
        assert_eq!(rows[1].started, "");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rows_serialize_to_json_array() {
        let rows = vec![Row {
            id: 12,
            name: "Кол".to_string(),
            started: "2026-07-23 14:05:11".to_string(),
            ended: String::new(),
            size: 50331648,
        }];
        let v: serde_json::Value = serde_json::from_str(&rows_json(&rows)).unwrap();
        assert_eq!(v[0]["id"], 12);
        assert_eq!(v[0]["name"], "Кол");
        assert_eq!(v[0]["size"], 50331648u64);
        assert_eq!(v[0]["ended"], "");
    }
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test calls_web 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'scan_calls'`, `cannot find type 'CallMeta'`

- [ ] **Step 3: Добавить call_meta в transcript_log**

В `src/transcript_log.rs`: в начало файла, к существующим `use`, добавить

```rust
use std::collections::HashMap;
```

и рядом с `pub fn calls_dir()` (строка ~260) добавить:

```rust
pub struct CallMeta {
    pub name: String,
    pub started: String,
    pub ended: String,
}

pub fn call_meta() -> HashMap<i64, CallMeta> {
    let mut out = HashMap::new();
    let Some(path) = db_path() else {
        return out;
    };
    let Ok(conn) = Connection::open(path) else {
        return out;
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT id, name, started_at, COALESCE(ended_at, '') FROM calls")
    else {
        return out;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    }) else {
        return out;
    };
    for (id, name, started, ended) in rows.flatten() {
        out.insert(id, CallMeta { name, started, ended });
    }
    out
}
```

- [ ] **Step 4: Реализовать scan_calls и rows_json**

В `src/calls_web.rs`, над блоком тестов (и над `parse_range`), добавить шапку модуля:

```rust
use std::collections::HashMap;
use std::path::Path;

use crate::transcript_log::CallMeta;

type MetaSource = fn() -> HashMap<i64, CallMeta>;

struct Row {
    id: i64,
    name: String,
    started: String,
    ended: String,
    size: u64,
}

fn scan_calls(root: &Path, meta: &HashMap<i64, CallMeta>) -> Vec<Row> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        let size = match std::fs::metadata(path.join("combined.mp4")) {
            Ok(m) if m.len() > 0 => m.len(),
            _ => continue,
        };
        let row = match meta.get(&id) {
            Some(m) => Row {
                id,
                name: m.name.clone(),
                started: m.started.clone(),
                ended: m.ended.clone(),
                size,
            },
            None => Row {
                id,
                name: format!("#{id}"),
                started: String::new(),
                ended: String::new(),
                size,
            },
        };
        out.push(row);
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

fn rows_json(rows: &[Row]) -> String {
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "started": r.started,
                "ended": r.ended,
                "size": r.size,
            })
        })
        .collect();
    serde_json::Value::Array(items).to_string()
}
```

- [ ] **Step 5: Убедиться, что тесты проходят**

Run: `cargo test calls_web 2>&1 | tail -20`
Expected: PASS — `4 passed`

- [ ] **Step 6: Коммит**

```bash
git add src/calls_web.rs src/transcript_log.rs
git commit -m "feat(calls_web): список склеенных колов с именами из БД"
```

---

### Task 3: Сервер, маршруты и страница

**Files:**
- Create: `src/calls_web.html`
- Modify: `src/calls_web.rs`

**Interfaces:**
- Consumes: `parse_range`, `scan_calls`, `rows_json`, `Row`, `MetaSource` (Tasks 1–2); `crate::transcript_log::{calls_dir, call_meta}`
- Produces:
  - `pub const PORT: u16 = 8788;`
  - `pub enum WebStatus { Idle, Running, Failed(String) }` (с `#[derive(Clone)]`)
  - `pub fn open(status: Arc<Mutex<WebStatus>>, ctx: egui::Context)` — поднимает сервер при необходимости и открывает браузер. Использует Task 4.

- [ ] **Step 1: Написать страницу**

Создать `src/calls_web.html`:

```html
<!doctype html>
<html lang="ru">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Колы</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; height: 100vh; display: flex; background: #14161a; color: #d8dbe0;
         font: 14px/1.4 system-ui, sans-serif; }
  aside { width: 300px; flex: none; overflow-y: auto; border-right: 1px solid #262a31; }
  h1 { margin: 0; padding: 14px 16px; font-size: 12px; letter-spacing: .08em;
       text-transform: uppercase; color: #7d8492; }
  ul { list-style: none; margin: 0; padding: 0; }
  li { padding: 10px 16px; cursor: pointer; border-left: 3px solid transparent; }
  li:hover { background: #1b1e24; }
  li.active { background: #1f2530; border-left-color: #6aa7ff; }
  .meta { display: block; margin-top: 2px; font-size: 12px; color: #7d8492; }
  .empty { padding: 0 16px; font-size: 13px; color: #7d8492; }
  main { flex: 1; display: flex; align-items: center; justify-content: center; padding: 16px; }
  video { width: 100%; max-height: 100%; background: #000; border-radius: 6px; }
</style>
</head>
<body>
<aside><h1>Колы</h1><ul id="list"></ul><p id="empty" class="empty" hidden></p></aside>
<main><video id="player" controls preload="metadata"></video></main>
<script>
const list = document.getElementById('list');
const empty = document.getElementById('empty');
const player = document.getElementById('player');
const mb = b => (b / 1048576).toFixed(1).replace('.', ',') + ' МБ';

fetch('/api/calls').then(r => r.json()).then(rows => {
  if (!rows.length) {
    empty.textContent = 'нечего смотреть — склей колы кнопкой 🎬 в виджете';
    empty.hidden = false;
    return;
  }
  for (const row of rows) {
    const li = document.createElement('li');
    li.textContent = '#' + row.id + ' · ' + row.name;
    const meta = document.createElement('span');
    meta.className = 'meta';
    meta.textContent = [row.started, mb(row.size)].filter(Boolean).join(' · ');
    li.append(meta);
    li.onclick = () => {
      for (const el of list.children) el.classList.remove('active');
      li.classList.add('active');
      player.src = '/video/' + row.id;
      player.play();
    };
    list.append(li);
  }
});
</script>
</body>
</html>
```

- [ ] **Step 2: Написать падающие тесты сервера**

В `src/calls_web.rs`, внутрь `mod tests`, добавить:

```rust
    fn no_meta() -> HashMap<i64, CallMeta> {
        HashMap::new()
    }

    fn curl(args: &[&str]) -> String {
        let out = std::process::Command::new("curl")
            .args(args)
            .output()
            .expect("curl");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn server_serves_page_list_and_ranged_video() {
        let tmp = std::env::temp_dir().join(format!("hw-web-srv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("7")).unwrap();
        let body: Vec<u8> = (0..1000u32).map(|i| b'a' + (i % 26) as u8).collect();
        std::fs::write(tmp.join("7").join("combined.mp4"), &body).unwrap();

        let server = tiny_http::Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let root = tmp.clone();
        std::thread::spawn(move || serve_loop(server, root, no_meta));

        let base = format!("http://127.0.0.1:{port}");

        let page = curl(&["-s", &base]);
        assert!(page.contains("<video"), "страница: {page}");

        let json = curl(&["-s", &format!("{base}/api/calls")]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["id"], 7);
        assert_eq!(v[0]["size"], 1000);
        assert_eq!(v[0]["name"], "#7");

        let code = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}",
            "-H", "Range: bytes=0-9", &format!("{base}/video/7"),
        ]);
        assert_eq!(code, "206");

        let chunk = curl(&["-s", "-H", "Range: bytes=0-9", &format!("{base}/video/7")]);
        assert_eq!(chunk.as_bytes(), &body[..10]);

        let whole = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}:%{size_download}",
            &format!("{base}/video/7"),
        ]);
        assert_eq!(whole, "200:1000");

        let unsat = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}",
            "-H", "Range: bytes=99999-", &format!("{base}/video/7"),
        ]);
        assert_eq!(unsat, "416");

        let missing = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}", &format!("{base}/video/999"),
        ]);
        assert_eq!(missing, "404");

        let junk = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}", &format!("{base}/video/../etc/passwd"),
        ]);
        assert_eq!(junk, "404");

        let _ = std::fs::remove_dir_all(&tmp);
    }
```

- [ ] **Step 3: Убедиться, что тест падает**

Run: `cargo test calls_web 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'serve_loop' in this scope`

- [ ] **Step 4: Реализовать сервер**

В `src/calls_web.rs` дополнить шапку модуля (`use`-блок в начале файла) до:

```rust
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::transcript_log::CallMeta;

pub const PORT: u16 = 8788;

const PAGE: &str = include_str!("calls_web.html");

#[derive(Clone)]
pub enum WebStatus {
    Idle,
    Running,
    Failed(String),
}
```

и добавить функции (после `rows_json`, до `mod tests`):

```rust
pub fn open(status: Arc<Mutex<WebStatus>>, ctx: egui::Context) {
    let running = matches!(&*status.lock().unwrap(), WebStatus::Running);
    if !running {
        let Some(root) = crate::transcript_log::calls_dir() else {
            *status.lock().unwrap() = WebStatus::Failed("нет папки колов".to_string());
            ctx.request_repaint();
            return;
        };
        match tiny_http::Server::http(("127.0.0.1", PORT)) {
            Ok(server) => {
                std::thread::spawn(move || {
                    serve_loop(server, root, crate::transcript_log::call_meta)
                });
                *status.lock().unwrap() = WebStatus::Running;
                crate::telemetry::event("calls_web.start", serde_json::json!({ "port": PORT }));
            }
            Err(e) => {
                crate::telemetry::error("calls_web.bind", &e.to_string());
                *status.lock().unwrap() = WebStatus::Failed(format!("порт {PORT} занят"));
                ctx.request_repaint();
                return;
            }
        }
        ctx.request_repaint();
    }
    let _ = std::process::Command::new("xdg-open")
        .arg(format!("http://127.0.0.1:{PORT}/"))
        .spawn();
}

fn serve_loop(server: tiny_http::Server, root: PathBuf, meta: MetaSource) {
    for req in server.incoming_requests() {
        handle(req, &root, meta);
    }
}

fn handle(req: tiny_http::Request, root: &Path, meta: MetaSource) {
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    if path == "/" {
        let resp = tiny_http::Response::from_string(PAGE).with_header(
            tiny_http::Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
        );
        let _ = req.respond(resp);
        return;
    }
    if path == "/api/calls" {
        let body = rows_json(&scan_calls(root, &meta()));
        let resp = tiny_http::Response::from_string(body).with_header(
            tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
        );
        let _ = req.respond(resp);
        return;
    }
    if let Some(rest) = path.strip_prefix("/video/") {
        if let Ok(id) = rest.parse::<i64>() {
            respond_video(req, &root.join(id.to_string()).join("combined.mp4"));
            return;
        }
    }
    let _ = req.respond(tiny_http::Response::empty(404));
}

fn respond_video(req: tiny_http::Request, path: &Path) {
    let Ok(mut file) = std::fs::File::open(path) else {
        let _ = req.respond(tiny_http::Response::empty(404));
        return;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let ctype = tiny_http::Header::from_bytes("Content-Type", "video/mp4").unwrap();
    let accept = tiny_http::Header::from_bytes("Accept-Ranges", "bytes").unwrap();
    let spec = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .map(|h| h.value.as_str().to_string());
    let Some(spec) = spec else {
        let resp = tiny_http::Response::from_file(file)
            .with_header(ctype)
            .with_header(accept);
        let _ = req.respond(resp);
        return;
    };
    let Some((start, end)) = parse_range(&spec, len) else {
        let resp = tiny_http::Response::empty(416).with_header(
            tiny_http::Header::from_bytes("Content-Range", format!("bytes */{len}")).unwrap(),
        );
        let _ = req.respond(resp);
        return;
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        let _ = req.respond(tiny_http::Response::empty(500));
        return;
    }
    let count = end - start + 1;
    let crange = tiny_http::Header::from_bytes(
        "Content-Range",
        format!("bytes {start}-{end}/{len}"),
    )
    .unwrap();
    let resp = tiny_http::Response::new(
        tiny_http::StatusCode(206),
        vec![ctype, accept, crange],
        file.take(count),
        Some(count as usize),
        None,
    );
    let _ = req.respond(resp);
}
```

- [ ] **Step 5: Убедиться, что тесты проходят**

Run: `cargo test calls_web 2>&1 | tail -20`
Expected: PASS — `5 passed`

Если ругается на неиспользуемый импорт (`Read`, `Seek`) — значит какой-то `use` лишний, убрать по подсказке компилятора.

- [ ] **Step 6: Коммит**

```bash
git add src/calls_web.rs src/calls_web.html
git commit -m "feat(calls_web): сервер на 127.0.0.1:8788 со страницей и Range-отдачей mp4"
```

---

### Task 4: Кнопка в виджете

**Files:**
- Modify: `src/main.rs:232` (поле структуры), `src/main.rs:546` (инициализация), `src/main.rs:2072-2168` (`draw_call`)

**Interfaces:**
- Consumes: `calls_web::{WebStatus, PORT, open}` (Task 3)
- Produces: ничего для следующих задач

- [ ] **Step 1: Добавить поле статуса**

В `src/main.rs` после строки `glue: Arc<std::sync::Mutex<mux::GlueStatus>>,` (строка 232):

```rust
    calls_web: Arc<std::sync::Mutex<calls_web::WebStatus>>,
```

и после строки `glue: Arc::new(std::sync::Mutex::new(mux::GlueStatus::Idle)),` (строка 546):

```rust
            calls_web: Arc::new(std::sync::Mutex::new(calls_web::WebStatus::Idle)),
```

- [ ] **Step 2: Добавить кнопку и строку статуса**

В `src/main.rs`, в `draw_call`: рядом с `let mut glue_go = false;` (строка 2075) добавить

```rust
        let mut web_go = false;
```

Затем в `ui.horizontal`, где живёт кнопка «🎬 Склеить» (строки 2137–2148), заменить блок на:

```rust
            let web_line = {
                use calls_web::WebStatus::*;
                match &*self.calls_web.lock().unwrap() {
                    Idle => None,
                    Running => Some((
                        format!("▶ 127.0.0.1:{}", calls_web::PORT),
                        egui::Color32::from_rgb(120, 200, 120),
                    )),
                    Failed(e) => Some((
                        format!("✖ {e}"),
                        egui::Color32::from_rgb(230, 120, 120),
                    )),
                }
            };
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!glue_busy, egui::Button::new("🎬 Склеить"))
                    .on_hover_text("Склеить все ещё не склеенные колы: свод аудио + видео в combined.mp4")
                    .clicked()
                {
                    glue_go = true;
                }
                if let Some((text, color)) = &glue_line {
                    ui.label(egui::RichText::new(text).size(11.0).color(*color));
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .button("📺 Колы")
                    .on_hover_text("Открыть склеенные колы в браузере")
                    .clicked()
                {
                    web_go = true;
                }
                if let Some((text, color)) = &web_line {
                    ui.label(egui::RichText::new(text).size(11.0).color(*color));
                }
            });
```

И после блока `if glue_go { … }` (строки 2164–2168) добавить:

```rust
        if web_go {
            calls_web::open(self.calls_web.clone(), ui.ctx().clone());
        }
```

- [ ] **Step 3: Собрать и прогнать все тесты**

Run: `cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20`
Expected: сборка без ошибок; все тесты PASS

- [ ] **Step 4: Ручная проверка в живом виджете**

Собрать релиз и перезапустить виджет **только через `setsid` из шелла** (не через `systemd-run --collect` — самоперезапуск умирает от cgroup-kill):

```bash
cargo build --release
pkill -f 'target/release/health-widget'; setsid ./target/release/health-widget >>widget.log 2>&1 &
```

Затем в виджете, секция «🎤 Кол»:
1. Нажать «📺 Колы» — открывается вкладка `http://127.0.0.1:8788/`, рядом с кнопкой зелёное `▶ 127.0.0.1:8788`.
2. В списке — склеенные колы, свежие сверху, с именем, датой и размером.
3. Клик по колу — видео играет.
4. **Перемотка**: тянуть ползунок в середину и в конец — картинка прыгает, воспроизведение продолжается (это и проверяет 206-ответы).
5. Полноэкранный режим и громкость — родные контролы работают.
6. Повторный клик по «📺 Колы» — открывается ещё вкладка, второй сервер не поднимается, ошибок в `widget.log` нет.

Если какой-то шаг провалился — чинить до коммита, а не после.

- [ ] **Step 5: Коммит**

```bash
git add src/main.rs
git commit -m "feat: кнопка «📺 Колы» — просмотр склеенных колов в браузере"
```

---

## Проверка результата

- `cargo test calls_web` — 5 тестов проходят.
- `cargo test` — весь набор проходит.
- Ручной сценарий из Task 4, шаг 4 (перемотка) — главный признак того, что Range-отдача сделана верно.
