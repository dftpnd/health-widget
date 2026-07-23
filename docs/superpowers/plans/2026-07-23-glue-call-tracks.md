# Склейка дорожек кола — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Кнопка «🎬 Склеить» в виджете, которая одним нажатием склеивает все ещё не склеенные колы в `combined.mp4` (свод двух аудиодорожек + видео экрана).

**Architecture:** Новый модуль `src/mux.rs` по образцу `src/screenshot.rs`: enum-статус в `Arc<Mutex<GlueStatus>>`, чистая функция `pending()` для выбора кандидатов, `glue_dir()` для одного кола через `ffmpeg`, `glue_all()` спавнит фоновый поток. `src/main.rs` добавляет поле статуса, кнопку и строку статуса в секцию «🎤 Кол».

**Tech Stack:** Rust, egui, внешний `ffmpeg` (шеллится через `std::process::Command`).

## Global Constraints

- Никаких комментариев в коде (ни `//`, ни `///`, ни `/* */`). Не возвращать удалённые комментарии.
- Строки UI — по-русски.
- Ветка одна — `master`, новых веток не создавать.
- Идиома: шеллить готовый CLI (`ffmpeg`), не тянуть Rust-зависимости.
- Колы лежат в `~/.local/share/health-widget/calls/<id>/` (числовые папки). Файлы: `mic.wav`, `zoom.wav`, `screen.mkv`. Маркер «склеено» = наличие `combined.mp4`.
- `ffmpeg` установлен в системе (`/usr/bin/ffmpeg`, 7:8.0.1).

---

## File Structure

- Create: `src/mux.rs` — вся логика склейки (статус, выбор кандидатов, вызов ffmpeg, фоновый поток).
- Modify: `src/main.rs` — `mod mux;`, поле статуса в `App`, кнопка и строка статуса в `draw_call`.

---

### Task 1: Модуль `mux` — выбор кандидатов (`pending`)

**Files:**
- Create: `src/mux.rs`
- Modify: `src/main.rs:31` (добавить `mod mux;` в список объявлений модулей)
- Test: `src/mux.rs` (`#[cfg(test)]` внизу файла)

**Interfaces:**
- Consumes: `crate::transcript_log::calls_dir() -> Option<PathBuf>` (уже есть).
- Produces:
  - `pub enum GlueStatus { Idle, Working { done: usize, total: usize }, Done(usize), Failed(String) }` (выведи `#[derive(Clone)]`).
  - `pub fn pending(active_id: Option<i64>) -> Vec<std::path::PathBuf>` — папки колов, которым нужна склейка, отсортированные по id (числовому имени папки) по возрастанию.

Правила отбора в `pending`:
- Папка внутри `calls_dir()`, имя которой парсится в `i64` (id).
- id не равен `active_id`.
- В папке есть хотя бы один из `mic.wav` / `zoom.wav`.
- В папке НЕТ `combined.mp4`.

- [ ] **Step 1: Написать падающий тест**

В конец `src/mux.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn mkcall(root: &std::path::Path, id: &str, files: &[&str]) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        for f in files {
            fs::write(dir.join(f), b"x").unwrap();
        }
    }

    #[test]
    fn pending_filters_active_glued_and_empty() {
        let tmp = std::env::temp_dir().join(format!("hw-mux-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        mkcall(&tmp, "1", &["mic.wav", "zoom.wav"]);
        mkcall(&tmp, "2", &["mic.wav", "zoom.wav", "combined.mp4"]);
        mkcall(&tmp, "3", &["screen.mkv"]);
        mkcall(&tmp, "4", &["zoom.wav"]);
        mkcall(&tmp, "5", &["mic.wav"]);
        fs::write(tmp.join("notanumber"), b"x").unwrap();

        let got = select_pending(&tmp, Some(5));
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["1", "4"]);

        let _ = fs::remove_dir_all(&tmp);
    }
}
```

- [ ] **Step 2: Запустить тест — убедиться, что не компилируется/падает**

Run: `cargo test --bin health-widget mux::tests::pending_filters_active_glued_and_empty 2>&1 | tail -20`
Expected: ошибка компиляции — `select_pending` и `GlueStatus` не определены.

- [ ] **Step 3: Написать минимальную реализацию**

В начало `src/mux.rs`:

```rust
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub enum GlueStatus {
    Idle,
    Working { done: usize, total: usize },
    Done(usize),
    Failed(String),
}

pub fn pending(active_id: Option<i64>) -> Vec<PathBuf> {
    match crate::transcript_log::calls_dir() {
        Some(root) => select_pending(&root, active_id),
        None => Vec::new(),
    }
}

fn select_pending(root: &Path, active_id: Option<i64>) -> Vec<PathBuf> {
    let mut out: Vec<(i64, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        if Some(id) == active_id {
            continue;
        }
        if path.join("combined.mp4").exists() {
            continue;
        }
        let has_audio =
            path.join("mic.wav").exists() || path.join("zoom.wav").exists();
        if !has_audio {
            continue;
        }
        out.push((id, path));
    }
    out.sort_by_key(|(id, _)| *id);
    out.into_iter().map(|(_, p)| p).collect()
}
```

В `src/main.rs` после строки `mod transcript_log;` (строка 31) добавить:

```rust
mod mux;
```

- [ ] **Step 4: Запустить тест — убедиться, что проходит**

Run: `cargo test --bin health-widget mux::tests::pending_filters_active_glued_and_empty 2>&1 | tail -20`
Expected: `test mux::tests::pending_filters_active_glued_and_empty ... ok`

- [ ] **Step 5: Коммит**

```bash
git add src/mux.rs src/main.rs
git commit -m "feat(mux): выбор колов-кандидатов для склейки"
```

---

### Task 2: Склейка одного кола через ffmpeg (`glue_dir`)

**Files:**
- Modify: `src/mux.rs`

**Interfaces:**
- Consumes: `select_pending`/`pending` из Task 1.
- Produces: `fn glue_dir(dir: &Path) -> Result<(), String>` — собирает `combined.mp4` в папке кола. Приватная (используется из `glue_all`).

Логика `glue_dir`:
- Пути: `mic.wav`, `zoom.wav`, `screen.mkv`, выход `combined.mp4`, временный `combined.mp4.part`.
- Наличие: `has_video` = `screen.mkv` существует и размер > 0; `has_mic`, `has_zoom` = файлы существуют.
- Собрать аргументы ffmpeg по матрице:
  - видео + оба аудио: `-i screen.mkv -i mic.wav -i zoom.wav -filter_complex "[1:a][2:a]amix=inputs=2:normalize=0[a]" -map 0:v -map [a] -c:v copy -c:a aac`
  - видео + один аудио: `-i screen.mkv -i <wav> -map 0:v -map 1:a -c:v copy -c:a aac`
  - аудио-only, оба: `-i mic.wav -i zoom.wav -filter_complex "[0:a][1:a]amix=inputs=2:normalize=0[a]" -map [a] -c:a aac`
  - аудио-only, один: `-i <wav> -c:a aac`
  - нет аудио вовсе: `Err`.
- Запуск: `ffmpeg -y -hide_banner -loglevel error <args> <part>`; статус выхода не 0 → `Err(stderr)`. Spawn-ошибка (нет бинаря) → `Err("нет ffmpeg")`.
- При успехе: `std::fs::rename(part, combined.mp4)`.

- [ ] **Step 1: Написать реализацию `glue_dir`**

В `src/mux.rs` (после `select_pending`):

```rust
fn glue_dir(dir: &Path) -> Result<(), String> {
    let mic = dir.join("mic.wav");
    let zoom = dir.join("zoom.wav");
    let screen = dir.join("screen.mkv");
    let out = dir.join("combined.mp4");
    let part = dir.join("combined.mp4.part");

    let has_mic = mic.exists();
    let has_zoom = zoom.exists();
    let has_video = std::fs::metadata(&screen).map(|m| m.len() > 0).unwrap_or(false);

    if !has_mic && !has_zoom {
        return Err("нет аудиодорожек".into());
    }

    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"]);

    let both = has_mic && has_zoom;
    let single = if has_mic { &mic } else { &zoom };

    if has_video {
        cmd.arg("-i").arg(&screen);
    }
    if both {
        cmd.arg("-i").arg(&mic).arg("-i").arg(&zoom);
    } else {
        cmd.arg("-i").arg(single);
    }

    if both {
        let a = if has_video { "[1:a][2:a]" } else { "[0:a][1:a]" };
        cmd.arg("-filter_complex")
            .arg(format!("{a}amix=inputs=2:normalize=0[a]"));
    }

    if has_video {
        cmd.arg("-map").arg("0:v");
        cmd.arg("-map").arg(if both { "[a]" } else { "1:a" });
        cmd.args(["-c:v", "copy"]);
    } else {
        cmd.arg("-map").arg(if both { "[a]" } else { "0:a" });
    }
    cmd.args(["-c:a", "aac"]);
    cmd.arg(&part);

    let output = cmd
        .output()
        .map_err(|_| "нет ffmpeg".to_string())?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&part);
        let err = String::from_utf8_lossy(&output.stderr);
        let tail = err.lines().last().unwrap_or("ffmpeg упал").to_string();
        return Err(tail);
    }

    std::fs::rename(&part, &out).map_err(|e| format!("rename: {e}"))
}
```

- [ ] **Step 2: Проверить компиляцию**

Run: `cargo build --bin health-widget 2>&1 | tail -20`
Expected: сборка проходит (возможен warning `glue_dir` не используется — это ок, снимется в Task 3).

- [ ] **Step 3: Ручная проверка на живом коле**

Run:
```bash
D=$(cargo run --quiet --bin health-widget -- --calls 2>/dev/null | head -0; ls -d ~/.local/share/health-widget/calls/*/ | head -1)
echo "проверяю на: $D"
ls -la "$D"
```
Затем на любой папке кола с `mic.wav`+`zoom.wav` вручную выполнить эквивалент (для быстрой проверки команды):
```bash
cd "$D" && ffmpeg -y -hide_banner -loglevel error -i mic.wav -i zoom.wav -filter_complex "[0:a][1:a]amix=inputs=2:normalize=0[a]" -map "[a]" -c:a aac /tmp/glue-check.mp4 && ffprobe -v error -show_entries stream=codec_type,codec_name -of default=noprint_wrappers=1 /tmp/glue-check.mp4
```
Expected: без ошибок, `ffprobe` показывает `codec_type=audio`, `codec_name=aac`. Удалить: `rm -f /tmp/glue-check.mp4`.

- [ ] **Step 4: Коммит**

```bash
git add src/mux.rs
git commit -m "feat(mux): склейка одного кола в combined.mp4 через ffmpeg"
```

---

### Task 3: Фоновый проход `glue_all`

**Files:**
- Modify: `src/mux.rs`

**Interfaces:**
- Consumes: `pending`, `glue_dir`, `GlueStatus` из Task 1–2.
- Produces: `pub fn glue_all(active_id: Option<i64>, status: Arc<Mutex<GlueStatus>>, ctx: egui::Context)` — спавнит поток, перебирает `pending`, обновляет `status` (`Working { done, total }` перед каждым колом, в конце `Done(успешно_склеено)` или `Failed`), дёргает `ctx.request_repaint()` после каждого обновления.

Поведение:
- `total = pending.len()`. Если `total == 0` → сразу `Done(0)`.
- Идём по списку; перед обработкой i-го: `Working { done: i, total }`.
- `glue_dir` вернул `Err("нет ffmpeg")` → выставить `Failed("нет ffmpeg")` и прервать проход (нет смысла продолжать).
- Прочие `Err(e)` → залогировать через `crate::telemetry::error("mux.fail", &e)`, продолжить (кол пропущен, счётчик успешных не растёт).
- В конце — `Done(done)` где `done` = число успешно склеенных.

- [ ] **Step 1: Написать реализацию `glue_all`**

В начало `src/mux.rs` заменить строку `use std::path::{Path, PathBuf};` на:

```rust
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
```

Добавить функцию (после `pending`):

```rust
pub fn glue_all(active_id: Option<i64>, status: Arc<Mutex<GlueStatus>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let dirs = pending(active_id);
        let total = dirs.len();
        let set = |s: GlueStatus| {
            *status.lock().unwrap() = s;
            ctx.request_repaint();
        };
        if total == 0 {
            return set(GlueStatus::Done(0));
        }
        let mut done = 0usize;
        for (i, dir) in dirs.iter().enumerate() {
            set(GlueStatus::Working { done: i, total });
            match glue_dir(dir) {
                Ok(()) => done += 1,
                Err(e) if e == "нет ffmpeg" => return set(GlueStatus::Failed(e)),
                Err(e) => crate::telemetry::error("mux.fail", &e),
            }
        }
        set(GlueStatus::Done(done));
    });
}
```

- [ ] **Step 2: Проверить компиляцию**

Run: `cargo build --bin health-widget 2>&1 | tail -20`
Expected: сборка проходит (возможен warning `glue_all` не используется — снимется в Task 4).

- [ ] **Step 3: Коммит**

```bash
git add src/mux.rs
git commit -m "feat(mux): фоновый проход склейки всех несклеенных колов"
```

---

### Task 4: Кнопка и статус в UI

**Files:**
- Modify: `src/main.rs` — поле `glue` в `App` (строка ~230, рядом с `shot`), инициализация в конструкторе (строка ~538), кнопка + строка статуса в `draw_call` (строки 2073–2124).

**Interfaces:**
- Consumes: `mux::GlueStatus`, `mux::glue_all` из Task 3; `self.active_call`.
- Produces: UI-побочный эффект, без нового публичного API.

- [ ] **Step 1: Добавить поле статуса в `App`**

В `src/main.rs`, в `struct App` сразу после строки `    shot: ShotState,` (строка 230) добавить:

```rust
    glue: Arc<std::sync::Mutex<mux::GlueStatus>>,
```

- [ ] **Step 2: Инициализировать поле в конструкторе**

В `src/main.rs`, в блоке инициализации `App { … }` сразу после закрывающей `},` блока `shot: ShotState { … }` (строка 543) добавить:

```rust
            glue: Arc::new(std::sync::Mutex::new(mux::GlueStatus::Idle)),
```

- [ ] **Step 3: Добавить кнопку и строку статуса в `draw_call`**

В `src/main.rs`, в `draw_call`, внутри `section_sized(ui, "🎤 Кол", …)`, после закрывающей `});` первого `ui.horizontal(|ui| { … })` (строка 2108, где рисуется кнопка Кол) и до блока `if let Some(m) = zoom_silent_min`, вставить:

```rust
            let glue_line = {
                use mux::GlueStatus::*;
                match &*self.glue.lock().unwrap() {
                    Idle => None,
                    Working { done, total } => Some((
                        format!("⧗ склеиваю {}/{}…", done + 1, total),
                        egui::Color32::from_rgb(210, 200, 120),
                    )),
                    Done(0) => Some((
                        "нечего склеивать".to_string(),
                        egui::Color32::GRAY,
                    )),
                    Done(n) => Some((
                        format!("✔ склеено {n}"),
                        egui::Color32::from_rgb(120, 200, 120),
                    )),
                    Failed(e) => Some((
                        format!("✖ {e}"),
                        egui::Color32::from_rgb(230, 120, 120),
                    )),
                }
            };
            let glue_busy =
                matches!(&*self.glue.lock().unwrap(), mux::GlueStatus::Working { .. });
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
```

В начало `draw_call`, рядом со строкой `let mut call_toggle = false;` (строка 2075), добавить:

```rust
        let mut glue_go = false;
```

В конце `draw_call`, после блока `if call_toggle { … }` (строки 2117–2123), добавить:

```rust
        if glue_go {
            let active_id = self.active_call.as_ref().map(|c| c.id);
            mux::glue_all(active_id, self.glue.clone(), ui.ctx().clone());
        }
```

- [ ] **Step 4: Проверить сборку и тесты**

Run: `cargo build --bin health-widget 2>&1 | tail -20 && cargo test --bin health-widget mux:: 2>&1 | tail -10`
Expected: сборка без ошибок и warning'ов о неиспользуемых функциях; тест `mux::tests::pending_filters_active_glued_and_empty ... ok`.

- [ ] **Step 5: Ручная проверка в живом виджете**

Run: `setsid cargo run --bin health-widget >/tmp/hw-glue.log 2>&1 &` — открыть виджет, в секции «🎤 Кол» нажать «🎬 Склеить». Убедиться: появляется `⧗ склеиваю …`, затем `✔ склеено N` или `нечего склеивать`; в папках колов появляются `combined.mp4`. Проверить:
```bash
ls ~/.local/share/health-widget/calls/*/combined.mp4 2>/dev/null
```
Expected: файлы `combined.mp4` появились у колов с аудио.

- [ ] **Step 6: Коммит**

```bash
git add src/main.rs
git commit -m "feat: кнопка «🎬 Склеить» — склейка дорожек колов в combined.mp4"
```

---

## Self-Review

**Spec coverage:**
- Кнопка «🎬 Склеить» в секции «🎤 Кол» → Task 4. ✓
- Батч по всем несклеенным, пропуск активного и уже склеенных → Task 1 (`pending`) + Task 3. ✓
- Свод mic+zoom в одну дорожку (`amix normalize=0`), видео copy → Task 2. ✓
- Аудио-only при пустом `screen.mkv` → Task 2 (`has_video` по размеру > 0). ✓
- Один трек / нет аудио → Task 2. ✓
- `.part` + rename → Task 2. ✓
- Инструмент ffmpeg, «нет ffmpeg» при отсутствии → Task 2 (spawn err) + Task 3 (прерывание) + Task 4 (строка статуса). ✓
- Статус `Working/Done/Failed`, строки UI по-русски, фон-поток, кнопка disabled при работе → Task 3–4. ✓
- Юнит-тест на `pending` → Task 1. ✓

**Placeholder scan:** плейсхолдеров нет — весь код приведён целиком.

**Type consistency:** `GlueStatus` (варианты `Idle/Working{done,total}/Done(usize)/Failed(String)`), `select_pending(&Path, Option<i64>)`, `pending(Option<i64>)`, `glue_dir(&Path)->Result<(),String>`, `glue_all(Option<i64>, Arc<Mutex<GlueStatus>>, egui::Context)` — согласованы между задачами. Строка-сентинел `"нет ffmpeg"` одинакова в `glue_dir` и `glue_all`.
