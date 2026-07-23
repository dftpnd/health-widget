# Запись экрана кола покадровыми скриншотами — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Заменить сломанную gst-запись экрана на покадровые скриншоты KWin ScreenShot2 (раз в секунду, центральная полоса), которые кнопка «🎬 Склеить» собирает в видео 1 fps и мукает с аудио в `combined.mp4`.

**Architecture:** Новый модуль `src/frames.rs` (геометрия центральной полосы + фоновый поток захвата в JPEG). `src/mux.rs` вместо `screen.mkv` берёт папку `frames/` и собирает её через `ffmpeg -framerate 1`. `src/screencast.rs` (portal + gst pipewiresrc) удаляется целиком.

**Tech Stack:** Rust, egui, `image` (JPEG), внешние CLI `ffmpeg` и `kscreen-doctor`, D-Bus `org.kde.KWin.ScreenShot2` через существующий `kwin_shot`.

## Global Constraints

- Никаких комментариев в коде (`//`, `///`, `/* */`, doc-комментарии). Не возвращать удалённые комментарии.
- Строки UI — по-русски.
- Ветка одна — `master`, новых веток не создавать.
- Идиома: шеллить готовые CLI (`ffmpeg`, `kscreen-doctor`), не тянуть новые Rust-зависимости. `image` уже с фичей `jpeg`, `serde_json` уже есть.
- Бинарный крейт: тесты гонять `cargo test --bin health-widget <фильтр>` (НЕ `--lib`).
- Кол лежит в `~/.local/share/health-widget/calls/<id>/`: `mic.wav`, `zoom.wav`, новая папка `frames/`. Маркер «склеено» = наличие `combined.mp4`.
- ScreenShot2 требует авторизации зарегистрированного `.desktop` — захват работает только из процесса виджета, standalone-CLI получает `NoAuthorized`. Реальный захват проверяется вручную из живого виджета.
- Монитор: логическая геометрия 4096×1728, режим 5120×2160, scale 1.25. Целевая ширина полосы на выходе ≈2500 px ⇒ логическая ширина `2500 / scale`.

---

## File Structure

- Create: `src/frames.rs` — геометрия центральной полосы (чистая функция), опрос геометрии монитора, `FrameCapture` (поток захвата в JPEG).
- Modify: `src/mux.rs` — `select_pending` учитывает `frames/`; `glue_dir` собирает кадры вместо `screen.mkv`, удаляет `frames/` после успеха.
- Modify: `src/main.rs` — `mod frames;` вместо `mod screencast;`, `ActiveCall.frames`, `start_frame_capture`, `end_call`, удаление `--screencast-test`.
- Delete: `src/screencast.rs`.

---

### Task 1: Геометрия центральной полосы

**Files:**
- Create: `src/frames.rs`
- Modify: `src/main.rs` (объявление `mod frames;`)
- Test: `src/frames.rs` (`#[cfg(test)]` внизу файла)

**Interfaces:**
- Produces:
  - `fn band(w_lg: u32, h_lg: u32, target_lg: u32) -> (i32, i32, u32, u32)` — возвращает `(x, y, w, h)` центрированной полосы; `y = 0`, `h` = полная высота; `w` = `min(target_lg, w_lg)`; `x`, `w`, `h` округляются вниз до чётного.
  - `fn geometry() -> (u32, u32, f64)` — `(w_lg, h_lg, scale)` из `kscreen-doctor -j`, фолбэк `(4096, 1728, 1.25)`.
  - `const TARGET_OUT_WIDTH: u32 = 2500;`

- [ ] **Step 1: Написать падающий тест**

Создать `src/frames.rs` с тестом в конце файла:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_is_centered_even_and_clamped() {
        assert_eq!(band(4096, 1728, 2000), (1048, 0, 2000, 1728));
        assert_eq!(band(4096, 1728, 2001), (1048, 0, 2000, 1728));
        assert_eq!(band(4096, 1728, 9000), (0, 0, 4096, 1728));
        assert_eq!(band(1001, 999, 2000), (0, 0, 1000, 998));
    }
}
```

- [ ] **Step 2: Запустить тест — убедиться, что не компилируется**

Run: `cargo test --bin health-widget frames::tests::band_is_centered_even_and_clamped 2>&1 | tail -20`
Expected: ошибка компиляции — `band` не определена (и/или модуль `frames` не объявлен).

- [ ] **Step 3: Написать минимальную реализацию**

В начало `src/frames.rs`:

```rust
pub const TARGET_OUT_WIDTH: u32 = 2500;

const FALLBACK_W_LG: u32 = 4096;
const FALLBACK_H_LG: u32 = 1728;
const FALLBACK_SCALE: f64 = 1.25;

fn even(v: u32) -> u32 {
    v & !1
}

pub fn band(w_lg: u32, h_lg: u32, target_lg: u32) -> (i32, i32, u32, u32) {
    let w = even(target_lg.min(w_lg));
    let h = even(h_lg);
    let x = even(w_lg.saturating_sub(w) / 2);
    (x as i32, 0, w, h)
}

pub fn geometry() -> (u32, u32, f64) {
    kscreen_logical().unwrap_or((FALLBACK_W_LG, FALLBACK_H_LG, FALLBACK_SCALE))
}

fn kscreen_logical() -> Option<(u32, u32, f64)> {
    let out = std::process::Command::new("kscreen-doctor").arg("-j").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    for o in v.get("outputs")?.as_array()? {
        if o.get("enabled").and_then(|e| e.as_bool()) != Some(true) {
            continue;
        }
        let scale = o.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0);
        if scale <= 0.0 {
            continue;
        }
        let mode_id = o.get("currentModeId").and_then(|m| m.as_str())?;
        for m in o.get("modes")?.as_array()? {
            if m.get("id").and_then(|i| i.as_str()) != Some(mode_id) {
                continue;
            }
            let size = m.get("size")?;
            let w = size.get("width").and_then(|x| x.as_f64())?;
            let h = size.get("height").and_then(|x| x.as_f64())?;
            return Some(((w / scale) as u32, (h / scale) as u32, scale));
        }
    }
    None
}
```

В `src/main.rs` после строки `mod detect;` добавить:

```rust
mod frames;
```

- [ ] **Step 4: Запустить тест — убедиться, что проходит**

Run: `cargo test --bin health-widget frames::tests::band_is_centered_even_and_clamped 2>&1 | tail -20`
Expected: `test frames::tests::band_is_centered_even_and_clamped ... ok`

Предупреждения о неиспользуемых `geometry`/`TARGET_OUT_WIDTH` ожидаемы (используются в Task 2).

- [ ] **Step 5: Коммит**

```bash
git add src/frames.rs src/main.rs
git commit -m "feat(frames): геометрия центральной полосы для скриншотов кола"
```

---

### Task 2: Поток покадрового захвата

**Files:**
- Modify: `src/frames.rs`

**Interfaces:**
- Consumes: `band`, `geometry`, `TARGET_OUT_WIDTH` из Task 1; существующий `crate::kwin_shot::capture_area(x: i32, y: i32, w: u32, h: u32) -> Result<image::RgbaImage, String>`; `crate::telemetry::error(ev: &str, err: &str)`.
- Produces:
  - `pub struct FrameCapture`
  - `pub fn FrameCapture::start(dir: &Path) -> FrameCapture` — создаёт `dir/frames/`, спавнит поток.
  - `pub fn FrameCapture::stop(self)` — останавливает поток и ждёт его.

Поведение потока: раз в 1 секунду снимает полосу, сохраняет `frames/{n:06}.jpg` (`n` с 1, растёт только при успешном сохранении). Ошибка захвата → `telemetry::error("frames.fail", …)`, кадр пропускается, поток продолжает. Интервал: спим `1s − время_захвата` (если захват дольше секунды — не спим).

JPEG: `image` не умеет сохранять RGBA в JPEG, поэтому кадр конвертируется в RGB8 перед сохранением.

- [ ] **Step 1: Написать реализацию**

В `src/frames.rs` добавить в начало файла импорты:

```rust
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
```

И после `kscreen_logical` добавить:

```rust
pub struct FrameCapture {
    stopping: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FrameCapture {
    pub fn start(dir: &Path) -> FrameCapture {
        let frames_dir = dir.join("frames");
        let _ = std::fs::create_dir_all(&frames_dir);
        let (w_lg, h_lg, scale) = geometry();
        let target_lg = (TARGET_OUT_WIDTH as f64 / scale) as u32;
        let (x, y, w, h) = band(w_lg, h_lg, target_lg);
        let stopping = Arc::new(AtomicBool::new(false));
        let flag = stopping.clone();
        let handle = std::thread::spawn(move || {
            let period = std::time::Duration::from_secs(1);
            let mut n: u64 = 1;
            while !flag.load(Ordering::Relaxed) {
                let started = std::time::Instant::now();
                match crate::kwin_shot::capture_area(x, y, w, h) {
                    Ok(img) => {
                        let path = frames_dir.join(format!("{n:06}.jpg"));
                        let rgb = image::DynamicImage::ImageRgba8(img).into_rgb8();
                        if rgb.save(&path).is_ok() {
                            n += 1;
                        }
                    }
                    Err(e) => crate::telemetry::error("frames.fail", &e),
                }
                let spent = started.elapsed();
                if spent < period {
                    std::thread::sleep(period - spent);
                }
            }
        });
        FrameCapture { stopping, handle: Some(handle) }
    }

    pub fn stop(mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
```

- [ ] **Step 2: Проверить сборку и что тест Task 1 не сломан**

Run: `cargo build --bin health-widget 2>&1 | grep -iE "^error" | head; cargo test --bin health-widget frames:: 2>&1 | tail -6`
Expected: сборка без ошибок (предупреждения о неиспользуемом `FrameCapture` ожидаемы — подключается в Task 4); тест `band_is_centered_even_and_clamped ... ok`.

- [ ] **Step 3: Коммит**

```bash
git add src/frames.rs
git commit -m "feat(frames): фоновый поток скриншотов кола раз в секунду"
```

---

### Task 3: Сборка кадров в `mux.rs`

**Files:**
- Modify: `src/mux.rs`

**Interfaces:**
- Consumes: ничего из Task 1–2 (работает по файлам на диске).
- Produces: `glue_dir` собирает `frames/*.jpg` в видео 1 fps и мукает с аудио; `select_pending` считает кандидатом папку с кадрами.

Правила:
- `has_any_frame(dir)` — в `dir` есть хотя бы один файл с расширением `jpg`.
- Кандидат в `select_pending`: числовая папка, не активный кол, без `combined.mp4`, и (есть аудио ИЛИ есть кадры).
- `glue_dir`: нет ни аудио, ни кадров → `Err`. Есть кадры → видео `-framerate 1 -i frames/%06d.jpg`, кодек `libx264 -pix_fmt yuv420p`. Аудио — как раньше (`amix` для двух, один трек как есть). После успешного `rename` удалить `frames/`.

- [ ] **Step 1: Обновить тест (падающий)**

В `src/mux.rs` в `mod tests` добавить хелпер и переписать тест:

```rust
    fn mkframes(root: &Path, id: &str) {
        let d = root.join(id).join("frames");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("000001.jpg"), b"x").unwrap();
    }

    #[test]
    fn pending_includes_frames_and_filters_rest() {
        let tmp = std::env::temp_dir().join(format!("hw-mux-f-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        mkcall(&tmp, "1", &["mic.wav", "zoom.wav"]);
        mkcall(&tmp, "2", &["mic.wav", "zoom.wav", "combined.mp4"]);
        mkcall(&tmp, "3", &["screen.mkv"]);
        mkcall(&tmp, "4", &["zoom.wav"]);
        mkcall(&tmp, "5", &["mic.wav"]);
        mkframes(&tmp, "6");
        mkframes(&tmp, "7");
        fs::write(tmp.join("7").join("combined.mp4"), b"x").unwrap();

        let got = select_pending(&tmp, Some(5));
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["1", "4", "6"]);

        let _ = fs::remove_dir_all(&tmp);
    }
```

- [ ] **Step 2: Запустить тест — убедиться, что падает**

Run: `cargo test --bin health-widget mux::tests::pending_includes_frames_and_filters_rest 2>&1 | tail -20`
Expected: FAIL — папка `6` (только кадры, без аудио) не попала в результат: `assertion ... left: ["1", "4"] right: ["1", "4", "6"]`.

- [ ] **Step 3: Реализация — `has_any_frame` и `select_pending`**

В `src/mux.rs` добавить функцию после `select_pending`:

```rust
fn has_any_frame(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path().extension().and_then(|x| x.to_str()) == Some("jpg")
    })
}
```

В `select_pending` заменить блок проверки аудио:

```rust
        let has_audio =
            path.join("mic.wav").exists() || path.join("zoom.wav").exists();
        if !has_audio {
            continue;
        }
```

на:

```rust
        let has_audio =
            path.join("mic.wav").exists() || path.join("zoom.wav").exists();
        if !has_audio && !has_any_frame(&path.join("frames")) {
            continue;
        }
```

- [ ] **Step 4: Запустить тест — убедиться, что проходит**

Run: `cargo test --bin health-widget mux::tests::pending_includes_frames_and_filters_rest 2>&1 | tail -6`
Expected: `test mux::tests::pending_includes_frames_and_filters_rest ... ok`

- [ ] **Step 5: Переписать `glue_dir` под кадры**

Заменить тело `glue_dir` в `src/mux.rs` целиком на:

```rust
fn glue_dir(dir: &Path) -> Result<(), String> {
    let mic = dir.join("mic.wav");
    let zoom = dir.join("zoom.wav");
    let frames = dir.join("frames");
    let out = dir.join("combined.mp4");
    let part = dir.join("combined.mp4.part");

    let has_mic = mic.exists();
    let has_zoom = zoom.exists();
    let has_video = has_any_frame(&frames);
    let has_audio = has_mic || has_zoom;

    if !has_audio && !has_video {
        return Err("нет ни кадров, ни аудиодорожек".into());
    }

    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"]);

    let both = has_mic && has_zoom;
    let single = if has_mic { &mic } else { &zoom };

    if has_video {
        cmd.args(["-framerate", "1"]).arg("-i").arg(frames.join("%06d.jpg"));
    }
    if has_audio {
        if both {
            cmd.arg("-i").arg(&mic).arg("-i").arg(&zoom);
        } else {
            cmd.arg("-i").arg(single);
        }
    }

    if both {
        let a = if has_video { "[1:a][2:a]" } else { "[0:a][1:a]" };
        cmd.arg("-filter_complex")
            .arg(format!("{a}amix=inputs=2:normalize=0[a]"));
    }

    if has_video {
        cmd.arg("-map").arg("0:v");
    }
    if has_audio {
        let m = if both {
            "[a]".to_string()
        } else if has_video {
            "1:a".to_string()
        } else {
            "0:a".to_string()
        };
        cmd.arg("-map").arg(m);
    }
    if has_video {
        cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    }
    if has_audio {
        cmd.args(["-c:a", "aac"]);
    }
    cmd.arg(&part);

    let output = cmd.output().map_err(|_| "нет ffmpeg".to_string())?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&part);
        let err = String::from_utf8_lossy(&output.stderr);
        let tail = err.lines().last().unwrap_or("ffmpeg упал").to_string();
        return Err(tail);
    }

    std::fs::rename(&part, &out).map_err(|e| format!("rename: {e}"))?;
    let _ = std::fs::remove_dir_all(&frames);
    Ok(())
}
```

- [ ] **Step 6: Проверить сборку и все тесты mux**

Run: `cargo build --bin health-widget 2>&1 | grep -iE "^error" | head; cargo test --bin health-widget mux:: 2>&1 | tail -6`
Expected: сборка без ошибок; тесты `mux::` проходят.

- [ ] **Step 7: Ручная проверка команды ffmpeg на синтетических кадрах**

Run:
```bash
D=$(mktemp -d); mkdir -p "$D/frames"
for i in $(seq -w 1 5); do ffmpeg -y -hide_banner -loglevel error -f lavfi -i color=c=blue:s=640x480 -frames:v 1 "$D/frames/00000$i.jpg"; done
ffmpeg -y -hide_banner -loglevel error -framerate 1 -i "$D/frames/%06d.jpg" -map 0:v -c:v libx264 -pix_fmt yuv420p "$D/out.mp4"
ffprobe -v error -show_entries stream=codec_name,width,height -show_entries format=duration -of default=noprint_wrappers=1 "$D/out.mp4"; rm -rf "$D"
```
Expected: `codec_name=h264`, `width=640`, `height=480`, `duration≈5` (5 кадров на 1 fps = 5 секунд).

- [ ] **Step 8: Коммит**

```bash
git add src/mux.rs
git commit -m "feat(mux): склейка видео кола из покадровых скриншотов"
```

---

### Task 4: Подключение к колу и удаление gst-пути

**Files:**
- Modify: `src/main.rs`
- Delete: `src/screencast.rs`

**Interfaces:**
- Consumes: `frames::FrameCapture::{start, stop}` из Task 2.
- Produces: `ActiveCall.frames: Option<frames::FrameCapture>`; метод `App::start_frame_capture`.

- [ ] **Step 1: Заменить поле в `ActiveCall`**

В `src/main.rs` заменить:

```rust
struct ActiveCall {
    id: i64,
    name: String,
    screen: Option<screencast::ScreenRecorder>,
}
```

на:

```rust
struct ActiveCall {
    id: i64,
    name: String,
    frames: Option<frames::FrameCapture>,
}
```

- [ ] **Step 2: Переключить старт кола на захват кадров**

В `src/main.rs` в `start_call` заменить:

```rust
        self.active_call = Some(ActiveCall { id, name, screen: None });
        self.reconcile_call_recording();
        self.start_screen_recording();
```

на:

```rust
        self.active_call = Some(ActiveCall { id, name, frames: None });
        self.reconcile_call_recording();
        self.start_frame_capture();
```

И заменить метод `start_screen_recording` целиком:

```rust
    fn start_screen_recording(&mut self) {
        let Some(call) = &self.active_call else {
            return;
        };
        let Some(dir) = transcript_log::call_dir(call.id) else {
            return;
        };
        match screencast::ScreenRecorder::start(&dir) {
            Ok(rec) => {
                if let Some(call) = &mut self.active_call {
                    call.screen = Some(rec);
                }
            }
            Err(e) => telemetry::error("screencast.fail", &e),
        }
    }
```

на:

```rust
    fn start_frame_capture(&mut self) {
        let Some(call) = &self.active_call else {
            return;
        };
        let Some(dir) = transcript_log::call_dir(call.id) else {
            return;
        };
        let capture = frames::FrameCapture::start(&dir);
        if let Some(call) = &mut self.active_call {
            call.frames = Some(capture);
        }
    }
```

- [ ] **Step 3: Переключить завершение кола**

В `src/main.rs` в `end_call` заменить:

```rust
        if let Some(rec) = call.screen {
            rec.stop();
        }
```

на:

```rust
        if let Some(capture) = call.frames {
            capture.stop();
        }
```

- [ ] **Step 4: Удалить gst-путь**

Удалить строку объявления модуля в `src/main.rs`:

```rust
mod screencast;
```

Удалить целиком блок подкоманды `--screencast-test` в `src/main.rs` (начинается со строки `if std::env::args().nth(1).as_deref() == Some("--screencast-test") {` и заканчивается закрывающей `}` перед блоком `--telemetry`):

```rust
    if std::env::args().nth(1).as_deref() == Some("--screencast-test") {
        let secs: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(4);
        let dir = std::env::temp_dir().join("hw-screencast-test");
        let _ = std::fs::remove_dir_all(&dir);
        match screencast::ScreenRecorder::start(&dir) {
            Ok(rec) => {
                eprintln!("cast стартовал, пишу {secs}s в {}", dir.display());
                std::thread::sleep(std::time::Duration::from_secs(secs));
                rec.stop();
                let mkv = dir.join("screen.mkv");
                let sz = std::fs::metadata(&mkv).map(|m| m.len()).unwrap_or(0);
                eprintln!("screen.mkv = {sz} байт");
                eprintln!("--- gst.log ---\n{}", std::fs::read_to_string(dir.join("screen.gst.log")).unwrap_or_default());
                std::process::exit(if sz > 0 { 0 } else { 1 });
            }
            Err(e) => {
                eprintln!("старт не удался: {e}");
                std::process::exit(2);
            }
        }
    }
```

Удалить файл:

```bash
git rm src/screencast.rs
```

- [ ] **Step 5: Проверить, что ссылок на `screencast::` не осталось, и собрать**

Run: `grep -rn "screencast::" src/ | grep -v "detect::screencast_active"; cargo build --bin health-widget 2>&1 | grep -iE "^error|warning: unused" | grep -v tiny_http | head`
Expected: grep ничего не находит; сборка без ошибок и без предупреждений о неиспользуемых элементах `frames`/`mux`.

Примечание: `detect::screencast_active()` — другой модуль (`src/detect.rs`), его НЕ трогаем.

- [ ] **Step 6: Прогнать весь набор тестов**

Run: `cargo test --bin health-widget 2>&1 | tail -6`
Expected: все тесты проходят (0 failed).

- [ ] **Step 7: Коммит**

```bash
git add -A src/
git commit -m "feat: запись экрана кола покадрово вместо сломанного gst-скринкаста"
```

- [ ] **Step 8: Ручная e2e-проверка из живого виджета (за пользователем)**

ScreenShot2 авторизован только для зарегистрированного виджета, поэтому проверка — вживую:

```bash
pkill -x health-widget; cargo build --release --bin health-widget && setsid ./target/release/health-widget >/tmp/hw.log 2>&1 < /dev/null &
```
Затем в виджете: «⏺ Кол» → подождать ~10 с → «⏹ Завершить» → «🎬 Склеить».

Проверить:
```bash
ls ~/.local/share/health-widget/calls/*/combined.mp4
ffprobe -v error -show_entries stream=codec_type,codec_name,width,height -show_entries format=duration -of default=noprint_wrappers=1 ~/.local/share/health-widget/calls/<id>/combined.mp4
```
Expected: есть `combined.mp4` с дорожками `video/h264` (ширина ≈2500) и `audio/aac`, длительность ≈ длине кола; папка `frames/` удалена.

---

## Self-Review

**Spec coverage:**
- Модуль `frames.rs`, поток раз в секунду, JPEG, счётчик кадров → Task 1–2. ✓
- Центральная полоса ≈2500 выходных px, логическая ширина `2500/scale`, чётные стороны, клампинг → Task 1 (`band`) + Task 2 (применение). ✓
- Геометрия через `kscreen-doctor -j`, фолбэк 4096×1728 → Task 1 (`geometry`). ✓
- Ошибка захвата → `telemetry::error`, поток продолжает → Task 2. ✓
- `ActiveCall.frames`, старт/стоп в `start_call`/`end_call` → Task 4. ✓
- `pending` учитывает `frames/` → Task 3 (Step 3) + тест. ✓
- Сборка `-framerate 1`, `libx264 -pix_fmt yuv420p`, матрица аудио, `.part`→rename, удаление `frames/` → Task 3 (Step 5). ✓
- Видео без аудио / аудио без кадров / ни того ни другого → Task 3 (Step 5). ✓
- Удаление `screencast.rs`, `--screencast-test`, `mod screencast` → Task 4. ✓
- Ручная e2e из авторизованного виджета → Task 4 Step 8. ✓

**Placeholder scan:** плейсхолдеров нет — код приведён целиком в каждом шаге.

**Type consistency:** `band(u32,u32,u32)->(i32,i32,u32,u32)`, `geometry()->(u32,u32,f64)`, `TARGET_OUT_WIDTH: u32`, `FrameCapture::start(&Path)->FrameCapture`, `FrameCapture::stop(self)`, `has_any_frame(&Path)->bool`, `ActiveCall.frames: Option<frames::FrameCapture>` — согласованы между задачами. `capture_area(i32,i32,u32,u32)` совпадает с сигнатурой в `src/kwin_shot.rs`; `telemetry::error(&str,&str)` — с `src/telemetry.rs`.
