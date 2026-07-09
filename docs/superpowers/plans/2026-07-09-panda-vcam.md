# Виртуальная камера с аватаром-пандой — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Виртуальная веб-камера (`/dev/videoN` через v4l2loopback), показывающая статичного SVG-аватара-панду с живым осциллографом на месте рта из микрофона; вкл/выкл из виджета.

**Architecture:** Новый модуль `src/avatar.rs` инкапсулирует всё: разовая растеризация SVG через `resvg`, поток-компоновщик на 30 fps (кэш панды → осциллограф в коробку рта → RGBA→YUYV → `write()` в устройство), вывод в v4l2 через `libc::ioctl(VIDIOC_S_FMT)`. Виджет держит хэндл `Avatar`, тумблер и превью. Аудио берётся из уже существующего `AudioMonitor` через общий `Arc<Mutex<VecDeque<f32>>>`.

**Tech Stack:** Rust, egui/eframe 0.31, `resvg` (новая зависимость), `libc` (есть), v4l2loopback (модуль ядра, ставится вручную).

## Global Constraints

- **Никаких комментариев в коде** — ни `//`, ни `///`, ни `//!`, ни `/* */`. Ни в коде, ни в тестах. Имена и маленькие функции объясняют себя сами. (CLAUDE.md)
- Ветка одна — `master`. Git-ветки не создавать.
- Идиома: шеллить готовые CLI / не тянуть зависимости, где дёшево. Здесь единственная новая зависимость — `resvg` (одобрено на брейншторме).
- UI-строки — на русском.
- Формат пикселей вирткамеры — **YUYV (YUV 4:2:2)**, тип буфера producer — `V4L2_BUF_TYPE_VIDEO_OUTPUT`.
- Виджет **не** делает `modprobe` (root); загрузка модуля — ручной шаг.
- Каждая задача заканчивается коммитом.

**Спек:** `docs/superpowers/specs/2026-07-09-panda-vcam-design.md`

---

## Структура файлов

- **Создать `src/avatar.rs`** — типы (`AvatarCfg`, `MouthBox`, `AvatarError`), чистые функции (`rasterize`, `rgba_to_yuyv`, `draw_scope`), обёртка устройства (`Vcam`), контроллер (`Avatar`).
- **Изменить `src/config.rs`** — добавить `AvatarCfg` и поле `avatar: AvatarCfg` в `Config` + env-оверрайды.
- **Изменить `src/audio.rs`** — метод `AudioMonitor::samples_handle()`.
- **Изменить `src/main.rs`** — `mod avatar;`, под-структура `AvatarState` в `App`, тумблер, превью, проводка.
- **Изменить `Cargo.toml`** — зависимость `resvg`.

## Общие определения типов (появляются в задачах ниже, приведены здесь для сверки)

```rust
pub struct MouthBox { pub x: u32, pub y: u32, pub w: u32, pub h: u32 }

pub struct AvatarCfg {
    pub svg_path: std::path::PathBuf,
    pub device: std::path::PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub mouth: MouthBox,
    pub scope_color: [u8; 3],
    pub scope_gain: f32,
}

pub enum AvatarError {
    NoDevice(std::path::PathBuf),
    Svg(String),
    Format(String),
    Io(std::io::Error),
}
```

---

### Task 1: Зависимость `resvg` и заглушка модуля

**Files:**
- Modify: `Cargo.toml`
- Create: `src/avatar.rs`
- Modify: `src/main.rs` (список `mod`)

**Interfaces:**
- Produces: пустой компилируемый модуль `avatar`, доступный крейт `resvg`.

- [ ] **Step 1: Добавить зависимость**

В `Cargo.toml` в секцию `[dependencies]` добавить строку (версия из crates.io на момент реализации; ниже — актуальная линия):

```toml
resvg = "0.44"
```

- [ ] **Step 2: Создать пустой модуль**

Создать `src/avatar.rs` с одной строкой (без комментариев):

```rust
pub struct Avatar;
```

- [ ] **Step 3: Подключить модуль**

В `src/main.rs` в блок объявлений `mod …` (рядом с `mod audio;`) добавить:

```rust
mod avatar;
```

- [ ] **Step 4: Проверить сборку**

Run: `cargo build`
Expected: успешная сборка (скачается `resvg` и транзитивные `usvg`/`tiny-skia`); предупреждение о неиспользуемом `Avatar` допустимо.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/avatar.rs src/main.rs
git commit -m "chore(avatar): каркас модуля вирткамеры + зависимость resvg"
```

---

### Task 2: Конфиг `AvatarCfg` с дефолтами и env-оверрайдами

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `config::MouthBox`, `config::AvatarCfg` (`Default`), поле `Config::avatar: AvatarCfg`. Именно эти типы переэкспортируются/используются в `avatar.rs` (Task 3+). Чтобы не дублировать типы, `AvatarCfg`/`MouthBox` объявляются здесь, в `config.rs`, и `avatar.rs` использует `crate::config::{AvatarCfg, MouthBox}`.

- [ ] **Step 1: Написать падающий тест**

В конец `src/config.rs` добавить:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_defaults_are_sane() {
        let c = AvatarCfg::default();
        assert_eq!(c.device, std::path::PathBuf::from("/dev/video10"));
        assert_eq!(c.width, 640);
        assert_eq!(c.height, 480);
        assert_eq!(c.fps, 30);
        assert_eq!(c.scope_color, [220, 30, 20]);
        assert!(c.mouth.w > 0 && c.mouth.h > 0);
        assert!(c.mouth.x + c.mouth.w <= c.width);
        assert!(c.mouth.y + c.mouth.h <= c.height);
    }
}
```

- [ ] **Step 2: Запустить тест — убедиться, что не компилируется/падает**

Run: `cargo test --lib config`
Expected: FAIL — `AvatarCfg` не найден.

- [ ] **Step 3: Реализовать типы и дефолты**

В `src/config.rs` добавить (над `impl Config`):

```rust
#[derive(Clone)]
pub struct MouthBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone)]
pub struct AvatarCfg {
    pub svg_path: PathBuf,
    pub device: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub mouth: MouthBox,
    pub scope_color: [u8; 3],
    pub scope_gain: f32,
}

impl Default for AvatarCfg {
    fn default() -> Self {
        let svg_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Pictures")
            .join("panda.svg");
        Self {
            svg_path,
            device: PathBuf::from("/dev/video10"),
            width: 640,
            height: 480,
            fps: 30,
            mouth: MouthBox { x: 150, y: 330, w: 340, h: 90 },
            scope_color: [220, 30, 20],
            scope_gain: 6.0,
        }
    }
}
```

Добавить поле в `Config`:

```rust
    pub autopilot_bin: PathBuf,
    pub avatar: AvatarCfg,
}
```

И в `Config::default()` в конце конструктора `Self { … }` добавить:

```rust
            autopilot_bin,
            avatar: AvatarCfg::default(),
        }
```

- [ ] **Step 4: Добавить env-оверрайды**

В `Config::load()` перед `c` (перед `c` в конце функции) добавить:

```rust
        if let Ok(v) = std::env::var("HEALTH_AVATAR_SVG") {
            c.avatar.svg_path = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("HEALTH_AVATAR_DEVICE") {
            c.avatar.device = PathBuf::from(v);
        }
        if let Some(v) = env_f32("HEALTH_AVATAR_GAIN") {
            c.avatar.scope_gain = v;
        }
```

- [ ] **Step 5: Запустить тест**

Run: `cargo test --lib config`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(avatar): конфиг AvatarCfg с дефолтами и env-оверрайдами"
```

---

### Task 3: RGBA → YUYV конвертация

**Files:**
- Modify: `src/avatar.rs`
- Test: `src/avatar.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub fn rgba_to_yuyv(rgba: &[u8], width: u32, height: u32) -> Vec<u8>` — вход RGBA (len `w*h*4`), выход YUYV (len `w*h*2`). Формула BT.601 full-range, упаковка макропикселя `[Y0, U, Y1, V]` (U/V усредняются по паре).

- [ ] **Step 1: Написать падающий тест**

В `src/avatar.rs` добавить:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_pixels_map_to_full_luma_neutral_chroma() {
        let rgba = vec![255u8; 2 * 4];
        let yuyv = rgba_to_yuyv(&rgba, 2, 1);
        assert_eq!(yuyv.len(), 2 * 2);
        assert_eq!(yuyv[0], 255);
        assert_eq!(yuyv[2], 255);
        assert_eq!(yuyv[1], 128);
        assert_eq!(yuyv[3], 128);
    }

    #[test]
    fn black_pixels_map_to_zero_luma() {
        let rgba = vec![0u8, 0, 0, 255, 0, 0, 0, 255];
        let yuyv = rgba_to_yuyv(&rgba, 2, 1);
        assert_eq!(yuyv[0], 0);
        assert_eq!(yuyv[2], 0);
        assert_eq!(yuyv[1], 128);
        assert_eq!(yuyv[3], 128);
    }
}
```

- [ ] **Step 2: Запустить — убедиться, что падает**

Run: `cargo test --lib avatar`
Expected: FAIL — `rgba_to_yuyv` не найдена.

- [ ] **Step 3: Реализовать**

В `src/avatar.rs` (над тестами) добавить:

```rust
fn clamp_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.299 * r + 0.587 * g + 0.114 * b
}

fn chroma_u(r: f32, g: f32, b: f32) -> f32 {
    128.0 - 0.168736 * r - 0.331264 * g + 0.5 * b
}

fn chroma_v(r: f32, g: f32, b: f32) -> f32 {
    128.0 + 0.5 * r - 0.418688 * g - 0.081312 * b
}

pub fn rgba_to_yuyv(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut out = vec![0u8; w * h * 2];
    let mut oi = 0;
    for row in 0..h {
        let mut col = 0;
        while col + 1 < w {
            let i0 = (row * w + col) * 4;
            let i1 = (row * w + col + 1) * 4;
            let (r0, g0, b0) = (rgba[i0] as f32, rgba[i0 + 1] as f32, rgba[i0 + 2] as f32);
            let (r1, g1, b1) = (rgba[i1] as f32, rgba[i1 + 1] as f32, rgba[i1 + 2] as f32);
            let y0 = clamp_u8(luma(r0, g0, b0));
            let y1 = clamp_u8(luma(r1, g1, b1));
            let u = clamp_u8((chroma_u(r0, g0, b0) + chroma_u(r1, g1, b1)) * 0.5);
            let v = clamp_u8((chroma_v(r0, g0, b0) + chroma_v(r1, g1, b1)) * 0.5);
            out[oi] = y0;
            out[oi + 1] = u;
            out[oi + 2] = y1;
            out[oi + 3] = v;
            oi += 4;
            col += 2;
        }
    }
    out
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test --lib avatar`
Expected: PASS (оба теста).

- [ ] **Step 5: Commit**

```bash
git add src/avatar.rs
git commit -m "feat(avatar): RGBA→YUYV конвертация кадра"
```

---

### Task 4: Растеризация SVG через resvg

**Files:**
- Modify: `src/avatar.rs`
- Test: `src/avatar.rs` (tests)

**Interfaces:**
- Produces: `pub fn rasterize(svg: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String>` — растеризует SVG на холст `width×height` (панда вписывается по аспекту, фон тёмный `#2b2b2b`), возвращает RGBA длиной `w*h*4`.

- [ ] **Step 1: Написать падающий тест**

Добавить в `mod tests`:

```rust
    #[test]
    fn rasterize_fills_canvas_with_dark_background() {
        let svg = br#"<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10' viewBox='0 0 10 10'></svg>"#;
        let rgba = rasterize(svg, 8, 6).expect("rasterize");
        assert_eq!(rgba.len(), 8 * 6 * 4);
        assert_eq!(rgba[0], 0x2b);
        assert_eq!(rgba[1], 0x2b);
        assert_eq!(rgba[2], 0x2b);
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn rasterize_rejects_garbage() {
        assert!(rasterize(b"not an svg", 8, 6).is_err());
    }
```

- [ ] **Step 2: Запустить — падает**

Run: `cargo test --lib avatar`
Expected: FAIL — `rasterize` не найдена.

- [ ] **Step 3: Реализовать**

В начало `src/avatar.rs` добавить импорты и функцию:

```rust
use resvg::tiny_skia;
use resvg::usvg;

pub fn rasterize(svg: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg, &opts).map_err(|e| e.to_string())?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or("pixmap")?;
    pixmap.fill(tiny_skia::Color::from_rgba8(0x2b, 0x2b, 0x2b, 255));

    let size = tree.size();
    let sx = width as f32 / size.width();
    let sy = height as f32 / size.height();
    let scale = sx.min(sy);
    let tx = (width as f32 - size.width() * scale) * 0.5;
    let ty = (height as f32 - size.height() * scale) * 0.5;
    let transform = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, tx, ty);

    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(pixmap.take())
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test --lib avatar`
Expected: PASS. Если API `usvg`/`resvg` в поставленной версии отличается (напр. `Options`/`render` сигнатуры), поправить по докам крейта — семантика та же: parse → pixmap с фоном → render с трансформом вписывания → `take()`.

- [ ] **Step 5: Commit**

```bash
git add src/avatar.rs
git commit -m "feat(avatar): растеризация SVG в RGBA через resvg"
```

---

### Task 5: Осциллограф в коробку рта

**Files:**
- Modify: `src/avatar.rs`
- Test: `src/avatar.rs` (tests)

**Interfaces:**
- Consumes: `crate::config::MouthBox`.
- Produces: `pub fn draw_scope(rgba: &mut [u8], width: u32, height: u32, mouth: &MouthBox, samples: &[f32], color: [u8; 3], gain: f32)` — рисует полилинию сигнала внутри коробки. Пустой/нулевой сигнал → плоская линия по вертикальному центру коробки. Значения клипуются коробкой.

- [ ] **Step 1: Написать падающий тест**

Добавить в `mod tests`:

```rust
    fn px(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    }

    #[test]
    fn silence_draws_flat_line_at_center() {
        let (w, h) = (40u32, 40u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let mouth = MouthBox { x: 10, y: 10, w: 20, h: 20 };
        draw_scope(&mut rgba, w, h, &mouth, &[0.0; 64], [200, 0, 0], 6.0);
        assert_eq!(px(&rgba, w, 20, 20), [200, 0, 0]);
        assert_eq!(px(&rgba, w, 2, 2), [0, 0, 0]);
    }

    #[test]
    fn loud_sample_leaves_center_row_for_some_column() {
        let (w, h) = (40u32, 40u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let mouth = MouthBox { x: 10, y: 10, w: 20, h: 20 };
        let mut samples = vec![0.0f32; 20];
        samples[10] = 1.0;
        draw_scope(&mut rgba, w, h, &mouth, &samples, [200, 0, 0], 6.0);
        let mut off_center_hit = false;
        for y in 10..30 {
            if y != 20 && px(&rgba, w, 20, y) == [200, 0, 0] {
                off_center_hit = true;
            }
        }
        assert!(off_center_hit);
    }
```

- [ ] **Step 2: Запустить — падает**

Run: `cargo test --lib avatar`
Expected: FAIL — `draw_scope` не найдена.

- [ ] **Step 3: Реализовать**

Добавить (и импорт `MouthBox`):

```rust
use crate::config::MouthBox;

fn put_px(rgba: &mut [u8], width: u32, x: i32, y: i32, color: [u8; 3]) {
    if x < 0 || y < 0 || x as u32 >= width {
        return;
    }
    let i = ((y as u32 * width + x as u32) * 4) as usize;
    if i + 3 >= rgba.len() {
        return;
    }
    rgba[i] = color[0];
    rgba[i + 1] = color[1];
    rgba[i + 2] = color[2];
    rgba[i + 3] = 255;
}

fn draw_line(rgba: &mut [u8], width: u32, a: (i32, i32), b: (i32, i32), color: [u8; 3]) {
    let (mut x0, mut y0) = a;
    let (x1, y1) = b;
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    loop {
        put_px(rgba, width, x0, y0, color);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

pub fn draw_scope(
    rgba: &mut [u8],
    width: u32,
    height: u32,
    mouth: &MouthBox,
    samples: &[f32],
    color: [u8; 3],
    gain: f32,
) {
    let _ = height;
    let cy = mouth.y as i32 + mouth.h as i32 / 2;
    let half = mouth.h as f32 * 0.5;
    let n = mouth.w.max(1) as usize;
    let sample_at = |col: usize| -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let idx = col * samples.len() / n;
        samples[idx.min(samples.len() - 1)]
    };
    let y_of = |col: usize| -> i32 {
        let v = (sample_at(col) * gain).clamp(-1.0, 1.0);
        cy - (v * half) as i32
    };
    let mut prev = (mouth.x as i32, y_of(0));
    for col in 1..n {
        let cur = (mouth.x as i32 + col as i32, y_of(col));
        draw_line(rgba, width, prev, cur, color);
        prev = cur;
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test --lib avatar`
Expected: PASS (все тесты avatar).

- [ ] **Step 5: Commit**

```bash
git add src/avatar.rs
git commit -m "feat(avatar): осциллограф сигнала в коробку рта"
```

---

### Task 6: Обёртка устройства v4l2 (`Vcam`)

**Files:**
- Modify: `src/avatar.rs`
- Test: `src/avatar.rs` (tests)

**Interfaces:**
- Produces:
  - `struct Vcam { fd: std::os::fd::OwnedFd }`
  - `impl Vcam { fn open(path: &std::path::Path) -> std::io::Result<Vcam>; fn set_format(&self, width: u32, height: u32) -> std::io::Result<()>; fn write_frame(&self, frame: &[u8]) -> std::io::Result<()> }`
  - Открытие несуществующего пути → `Err`. `set_format` задаёт YUYV / `V4L2_BUF_TYPE_VIDEO_OUTPUT` через `ioctl(VIDIOC_S_FMT)`.

- [ ] **Step 1: Написать падающий тест**

Добавить в `mod tests`:

```rust
    #[test]
    fn open_missing_device_errors() {
        let r = Vcam::open(std::path::Path::new("/dev/does-not-exist-999"));
        assert!(r.is_err());
    }
```

- [ ] **Step 2: Запустить — падает**

Run: `cargo test --lib avatar`
Expected: FAIL — `Vcam` не найден.

- [ ] **Step 3: Реализовать FFI-обёртку**

Добавить в `src/avatar.rs`:

```rust
use std::os::fd::{AsRawFd, OwnedFd};

const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_FIELD_NONE: u32 = 1;
const V4L2_COLORSPACE_SRGB: u32 = 8;
const V4L2_PIX_FMT_YUYV: u32 = 0x56595559;

#[repr(C)]
struct V4l2PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    priv_: u32,
    flags: u32,
    enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
struct V4l2Format {
    type_: u32,
    raw: [u8; 200],
}

const fn iowr(ty: u8, nr: u8, size: usize) -> libc::c_ulong {
    ((3u64 << 30) | ((size as u64) << 16) | ((ty as u64) << 8) | nr as u64) as libc::c_ulong
}

const VIDIOC_S_FMT: libc::c_ulong = iowr(b'V', 5, std::mem::size_of::<V4l2Format>());

pub struct Vcam {
    fd: OwnedFd,
    frame_len: usize,
}

impl Vcam {
    pub fn open(path: &std::path::Path) -> std::io::Result<Vcam> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_RDWR)
            .open(path)?;
        Ok(Vcam { fd: OwnedFd::from(file), frame_len: 0 })
    }

    pub fn set_format(&mut self, width: u32, height: u32) -> std::io::Result<()> {
        let pix = V4l2PixFormat {
            width,
            height,
            pixelformat: V4L2_PIX_FMT_YUYV,
            field: V4L2_FIELD_NONE,
            bytesperline: width * 2,
            sizeimage: width * height * 2,
            colorspace: V4L2_COLORSPACE_SRGB,
            priv_: 0,
            flags: 0,
            enc: 0,
            quantization: 0,
            xfer_func: 0,
        };
        let mut fmt = V4l2Format { type_: V4L2_BUF_TYPE_VIDEO_OUTPUT, raw: [0u8; 200] };
        let pix_bytes = unsafe {
            std::slice::from_raw_parts(
                (&pix as *const V4l2PixFormat) as *const u8,
                std::mem::size_of::<V4l2PixFormat>(),
            )
        };
        fmt.raw[..pix_bytes.len()].copy_from_slice(pix_bytes);
        let rc = unsafe {
            libc::ioctl(self.fd.as_raw_fd(), VIDIOC_S_FMT, &mut fmt as *mut V4l2Format)
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error());
        }
        self.frame_len = (width * height * 2) as usize;
        Ok(())
    }

    pub fn write_frame(&self, frame: &[u8]) -> std::io::Result<()> {
        let n = unsafe {
            libc::write(
                self.fd.as_raw_fd(),
                frame.as_ptr() as *const libc::c_void,
                frame.len(),
            )
        };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}
```

Примечание: `open` берёт сигнатуру `&mut self` для `set_format` — поле `Vcam { fd, frame_len }` заведено выше; тест из шага 1 использует только `open`, компилируется.

- [ ] **Step 4: Запустить тест**

Run: `cargo test --lib avatar`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/avatar.rs
git commit -m "feat(avatar): обёртка v4l2-устройства (S_FMT YUYV + write)"
```

---

### Task 7: Общий доступ к сэмплам микрофона

**Files:**
- Modify: `src/audio.rs`

**Interfaces:**
- Produces: `AudioMonitor::samples_handle(&self) -> std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>` — клон `Arc` на кольцо сэмплов; используется потоком аватара.

- [ ] **Step 1: Реализовать метод**

В `impl AudioMonitor` (рядом с `snapshot`) добавить:

```rust
    pub fn samples_handle(&self) -> Arc<Mutex<VecDeque<f32>>> {
        self.samples.clone()
    }
```

- [ ] **Step 2: Проверить сборку**

Run: `cargo build`
Expected: успешно.

- [ ] **Step 3: Commit**

```bash
git add src/audio.rs
git commit -m "feat(audio): samples_handle() для шаринга кольца сэмплов"
```

---

### Task 8: Контроллер `Avatar` (поток-компоновщик)

**Files:**
- Modify: `src/avatar.rs`
- Test: `src/avatar.rs` (tests)

**Interfaces:**
- Consumes: `rasterize`, `draw_scope`, `rgba_to_yuyv`, `Vcam`, `crate::config::AvatarCfg`.
- Produces:
  - `pub enum AvatarError { NoDevice(PathBuf), Svg(String), Format(String), Io(std::io::Error) }` + `impl std::fmt::Display`.
  - `pub struct Avatar { … }`
  - `impl Avatar { pub fn start(cfg: &AvatarCfg, samples: Arc<Mutex<VecDeque<f32>>>) -> Result<Avatar, AvatarError>; pub fn stop(self); pub fn is_running(&self) -> bool; pub fn last_frame(&self) -> Option<(u32, u32, Vec<u8>)> }`
  - `impl Drop for Avatar` → `stop`-семантика.
  - `last_frame` возвращает `(width, height, rgba)` последнего кадра для превью.

- [ ] **Step 1: Написать падающий тест**

Заменить строку-заглушку `pub struct Avatar;` (из Task 1) на реальную реализацию (шаг 3) — а тест на ошибку добавить в `mod tests`:

```rust
    #[test]
    fn start_without_device_returns_no_device() {
        let mut cfg = crate::config::AvatarCfg::default();
        cfg.device = std::path::PathBuf::from("/dev/does-not-exist-999");
        cfg.svg_path = std::path::PathBuf::from("/dev/does-not-exist-999.svg");
        let samples = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let r = Avatar::start(&cfg, samples);
        assert!(matches!(r, Err(AvatarError::NoDevice(_))));
    }
```

- [ ] **Step 2: Запустить — падает**

Run: `cargo test --lib avatar`
Expected: FAIL (нет `Avatar::start`).

- [ ] **Step 3: Реализовать контроллер**

В `src/avatar.rs` добавить импорты и заменить заглушку `pub struct Avatar;`:

```rust
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::config::AvatarCfg;

pub enum AvatarError {
    NoDevice(PathBuf),
    Svg(String),
    Format(String),
    Io(std::io::Error),
}

impl std::fmt::Display for AvatarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AvatarError::NoDevice(p) => write!(f, "нет устройства {}", p.display()),
            AvatarError::Svg(e) => write!(f, "SVG: {e}"),
            AvatarError::Format(e) => write!(f, "формат: {e}"),
            AvatarError::Io(e) => write!(f, " io: {e}"),
        }
    }
}

pub struct Avatar {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    last: Arc<Mutex<Option<(u32, u32, Vec<u8>)>>>,
}

impl Avatar {
    pub fn start(cfg: &AvatarCfg, samples: Arc<Mutex<VecDeque<f32>>>) -> Result<Avatar, AvatarError> {
        if !cfg.device.exists() {
            return Err(AvatarError::NoDevice(cfg.device.clone()));
        }
        let svg = std::fs::read(&cfg.svg_path).map_err(AvatarError::Io)?;
        let base = rasterize(&svg, cfg.width, cfg.height).map_err(AvatarError::Svg)?;

        let mut cam = Vcam::open(&cfg.device).map_err(AvatarError::Io)?;
        cam.set_format(cfg.width, cfg.height).map_err(|e| AvatarError::Format(e.to_string()))?;

        let running = Arc::new(AtomicBool::new(true));
        let last: Arc<Mutex<Option<(u32, u32, Vec<u8>)>>> = Arc::new(Mutex::new(None));

        let run = running.clone();
        let last_w = last.clone();
        let cfg = cfg.clone();
        let frame_dt = Duration::from_secs_f32(1.0 / cfg.fps.max(1) as f32);

        let handle = std::thread::spawn(move || {
            let mut buf: Vec<f32> = Vec::with_capacity(4096);
            let mut fail_streak = 0u32;
            while run.load(Ordering::Relaxed) {
                let tick = Instant::now();
                buf.clear();
                if let Ok(g) = samples.lock() {
                    buf.extend(g.iter().copied());
                }
                let mut frame = base.clone();
                draw_scope(
                    &mut frame,
                    cfg.width,
                    cfg.height,
                    &cfg.mouth,
                    &buf,
                    cfg.scope_color,
                    cfg.scope_gain,
                );
                let yuyv = rgba_to_yuyv(&frame, cfg.width, cfg.height);
                match cam.write_frame(&yuyv) {
                    Ok(_) => fail_streak = 0,
                    Err(_) => {
                        fail_streak += 1;
                        if fail_streak == 30 {
                            crate::telemetry::error("avatar.write", "серия ошибок записи кадра");
                        }
                    }
                }
                if let Ok(mut g) = last_w.lock() {
                    *g = Some((cfg.width, cfg.height, frame));
                }
                if let Some(rem) = frame_dt.checked_sub(tick.elapsed()) {
                    std::thread::sleep(rem);
                }
            }
        });

        crate::telemetry::event("avatar.start", serde_json::json!({ "device": cfg.device }));
        Ok(Avatar { running, handle: Some(handle), last })
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn last_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        self.last.lock().ok().and_then(|g| g.clone())
    }
}

impl Drop for Avatar {
    fn drop(&mut self) {
        self.shutdown();
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test --lib avatar`
Expected: PASS (все тесты avatar, включая `start_without_device_returns_no_device`).

- [ ] **Step 5: Commit**

```bash
git add src/avatar.rs
git commit -m "feat(avatar): контроллер-компоновщик (поток 30fps, старт/стоп/превью)"
```

---

### Task 9: Проводка в виджет — состояние, тумблер, превью

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `avatar::{Avatar, AvatarError}`, `AudioMonitor::samples_handle`, `Config::avatar`.
- Produces: под-структура `AvatarState` в `App`; кнопка «🐼 Камера» и панель превью в `update`.

- [ ] **Step 1: Добавить под-структуру состояния**

Рядом с другими под-структурами (`AudioState`, `ShotState` и т.д.) добавить:

```rust
struct AvatarState {
    cam: Option<avatar::Avatar>,
    error: Option<String>,
    tex: Option<egui::TextureHandle>,
}
```

В `struct App { … }` добавить поле:

```rust
    avatar: AvatarState,
}
```

В конструкторе `App` (в `Self { … }`, где инициализируются `audio`/`shot`) добавить:

```rust
            avatar: AvatarState { cam: None, error: None, tex: None },
```

- [ ] **Step 2: Добавить метод переключения**

В `impl App` добавить:

```rust
    fn toggle_avatar(&mut self) {
        if let Some(cam) = self.avatar.cam.take() {
            cam.stop();
            self.avatar.error = None;
            return;
        }
        let samples = match &self.audio.mic {
            Some(m) => m.samples_handle(),
            None => std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
        };
        match avatar::Avatar::start(&self.cfg.avatar, samples) {
            Ok(cam) => {
                self.avatar.cam = Some(cam);
                self.avatar.error = None;
            }
            Err(e) => self.avatar.error = Some(e.to_string()),
        }
    }
```

Примечание: если поле конфига в `App` называется иначе, чем `self.cfg` — использовать фактическое имя (проверить объявление `App`).

- [ ] **Step 3: Добавить кнопку и превью в `update`**

В `eframe::App::update`, в подходящую панель с кнопками, добавить:

```rust
            let on = self.avatar.cam.is_some();
            let label = if on { "🐼 Камера: в эфире" } else { "🐼 Камера" };
            if ui.button(label).clicked() {
                self.toggle_avatar();
            }
            if let Some(err) = &self.avatar.error {
                ui.colored_label(egui::Color32::RED, err);
            }
            if let Some(cam) = &self.avatar.cam {
                if let Some((w, h, rgba)) = cam.last_frame() {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    match &mut self.avatar.tex {
                        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                        None => {
                            self.avatar.tex =
                                Some(ctx.load_texture("avatar_preview", img, egui::TextureOptions::LINEAR));
                        }
                    }
                }
                if let Some(t) = &self.avatar.tex {
                    ui.image((t.id(), egui::vec2(160.0, 120.0)));
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(66));
            }
```

Примечание: имя контекста (`ctx`) и `ui` — по фактическим переменным в `update`. Если превью загромождает основную панель — обернуть в `ui.collapsing("Превью камеры", |ui| { … })`.

- [ ] **Step 4: Проверить сборку**

Run: `cargo build`
Expected: успешно. Исправить возможные расхождения имён (`self.cfg`, `ctx`, панель кнопок) по фактическому коду `main.rs`.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(avatar): тумблер камеры и превью в виджете"
```

---

### Task 10: Ручная верификация end-to-end

**Files:** нет (проверка поведением).

- [ ] **Step 1: Загрузить модуль ядра**

```bash
sudo modprobe v4l2loopback video_nr=10 card_label="Panda" exclusive_caps=1
ls -l /dev/video10
```
Expected: `/dev/video10` существует. Если `modprobe` не находит модуль — поставить пакет (`v4l2loopback-dkms` или аналог дистрибутива) и повторить.

- [ ] **Step 2: Запустить виджет и включить камеру**

```bash
cargo run --release
```
В виджете: (при желании) включить микрофон, затем нажать «🐼 Камера». Ожидание: превью показывает панду; при речи в микрофон осциллограф во рту шевелится; статус «в эфире».

- [ ] **Step 3: Проверить внешним потребителем**

Открыть KDE-камеру (или в браузере тест `getUserMedia`, напр. webcamtests.com), выбрать источник **Panda**. Ожидание: тот же кадр панды с осциллографом-ртом.

- [ ] **Step 4: Подгонка**

Если рот-осциллограф не на месте / слабый — поправить в конфиге `avatar.mouth {x,y,w,h}` и `avatar.scope_gain` (или через env `HEALTH_AVATAR_GAIN`), перезапустить камеру, свериться по превью.

- [ ] **Step 5: Проверить выключение**

Нажать «🐼 Камера» ещё раз (выкл) и закрыть виджет. Ожидание: поток останавливается чисто, `/dev/video10` освобождается (потребитель теряет сигнал), процесс завершается без зависаний.

- [ ] **Step 6: (Опционально) персистентность модуля**

Для автозагрузки после ребута создать (root) `/etc/modules-load.d/v4l2loopback.conf` со строкой `v4l2loopback` и `/etc/modprobe.d/v4l2loopback.conf` с `options v4l2loopback video_nr=10 card_label="Panda" exclusive_caps=1`.

---

## Self-Review

**Spec coverage:**
- Вирткамера через v4l2loopback → Task 6, 10 ✓
- resvg-растеризация один раз → Task 4, Task 8 (`base` вне цикла) ✓
- Сырые кадры в /dev/videoN, без ffmpeg → Task 6 (`write_frame`) ✓
- YUYV → Task 3, 6 ✓
- Микрофон (default source) как источник → Task 7, 9 (шарим `self.audio.mic`) ✓
- Коробка рта + осциллограф → Task 5 ✓
- Конфиг с дефолтами → Task 2 ✓
- Тумблер + превью → Task 9 ✓
- Обработка ошибок (NoDevice/Format/Svg/write-streak/mic-off→flat) → Task 8 (`AvatarError`, `fail_streak`), Task 5 (пустой сигнал → плоская линия), Task 9 (mic None → пустое кольцо) ✓
- Жизненный цикл потока (AtomicBool + join + Drop) → Task 8 ✓
- Персистентность модуля (вне кода) → Task 10 Step 6 ✓
- Верификация по превью + внешний потребитель → Task 10 ✓

**Placeholder scan:** нет TBD/TODO; каждый шаг с кодом содержит код. Заметки про «имена по факту» (`self.cfg`, `ctx`, версия resvg/usvg API) — намеренные точки сверки с реальным кодом, не пропуски.

**Type consistency:** `AvatarCfg`/`MouthBox` объявлены в `config.rs` (Task 2), используются в `avatar.rs` через `crate::config::…` (Task 5, 8) и в `main.rs` (Task 9) — согласовано. `Vcam::open` → `Vcam { fd, frame_len }`, `set_format(&mut self, …)` — согласовано между Task 6 и Task 8. `Avatar::last_frame -> Option<(u32,u32,Vec<u8>)>` — совпадает с потреблением в Task 9. `samples_handle` возвращает `Arc<Mutex<VecDeque<f32>>>` — совпадает с параметром `Avatar::start`.

## Замечания/риски

- **API resvg/usvg**: сигнатуры `usvg::Tree::from_data`, `resvg::render`, `tiny_skia` могут отличаться между версиями. Семантика фиксирована (Task 4); при расхождении править по докам поставленной версии.
- **Размер `struct v4l2_format`**: используем каноничные 204 байта (`type` + `raw[200]`), overlay `V4l2PixFormat` в начало `raw`. Если конкретный v4l2loopback ругается на `S_FMT` — сверить размер через `v4l2-ctl` и константу `iowr`.
- **Смена микрофона после старта камеры**: аватар держит handle кольца, взятый на старте. Для v1 — включать/выбирать микрофон до включения камеры; смена на лету не отслеживается (осознанное упрощение, YAGNI).
