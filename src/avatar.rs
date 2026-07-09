use resvg::tiny_skia;
use resvg::usvg;
use crate::config::{AvatarCfg, MouthBox};
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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
    pad: u32,
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
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
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
        let mut fmt = V4l2Format { type_: V4L2_BUF_TYPE_VIDEO_OUTPUT, pad: 0, raw: [0u8; 200] };
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
        if n as usize != frame.len() {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "short frame write"));
        }
        Ok(())
    }
}

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

    #[test]
    fn open_missing_device_errors() {
        let r = Vcam::open(std::path::Path::new("/dev/does-not-exist-999"));
        assert!(r.is_err());
    }

    #[test]
    fn v4l2_format_matches_kernel_abi_size() {
        assert_eq!(std::mem::size_of::<V4l2Format>(), 208);
    }

    #[test]
    fn start_without_device_returns_no_device() {
        let mut cfg = crate::config::AvatarCfg::default();
        cfg.device = std::path::PathBuf::from("/dev/does-not-exist-999");
        cfg.svg_path = std::path::PathBuf::from("/dev/does-not-exist-999.svg");
        let samples = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let r = Avatar::start(&cfg, samples);
        assert!(matches!(r, Err(AvatarError::NoDevice(_))));
    }
}
