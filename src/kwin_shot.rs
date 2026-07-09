//! Прямой захват области экрана через `org.kde.KWin.ScreenShot2.CaptureArea`
//! (DBus) — без spectacle и без всякого UI. KWin отдаёт пиксели только процессам,
//! чей бинарь совпадает с установленным приложением (`.desktop` → Exec); поэтому
//! виджет ставит себе `.desktop` (health-widget.desktop) и авторизуется после
//! пересбора allowlist (перезапуск KWin / следующий вход).
//!
//! CaptureArea пишет сырые пиксели в переданный пайп и возвращает метаданные
//! (width/height/stride/format QImage). Читаем пайп в отдельном потоке (чтобы
//! запись KWin не заблокировалась на переполнении буфера), собираем RGBA.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;

use zbus::blocking::Connection;
use zbus::zvariant::{Fd, OwnedValue, Value};

/// Захватить прямоугольник (координаты — в логической системе KWin, как геометрия
/// окон) напрямую у KWin. Возвращает RGBA-изображение в физических пикселях.
pub fn capture_area(x: i32, y: i32, w: u32, h: u32) -> Result<image::RgbaImage, String> {
    if w == 0 || h == 0 {
        return Err("нулевая область".into());
    }
    let (mut ours, theirs) = UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;

    // Читаем пиксели в фоне — иначе KWin заблокируется на write(), когда буфер полон.
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = ours.read_to_end(&mut buf);
        buf
    });

    let conn = Connection::session().map_err(|e| format!("session bus: {e}"))?;
    let opts: HashMap<String, Value> = HashMap::new();
    let reply = conn
        .call_method(
            Some("org.kde.KWin.ScreenShot2"),
            "/org/kde/KWin/ScreenShot2",
            Some("org.kde.KWin.ScreenShot2"),
            "CaptureArea",
            &(x, y, w, h, opts, Fd::from(theirs.as_fd())),
        )
        .map_err(|e| format!("CaptureArea: {e}"))?;

    // Закрываем наш конец записи, чтобы reader увидел EOF после KWin.
    drop(theirs);

    let meta: HashMap<String, OwnedValue> =
        reply.body().deserialize().map_err(|e| format!("meta decode: {e}"))?;
    let bytes = reader.join().map_err(|_| "reader thread panic".to_string())?;

    let width = get_u32(&meta, "width").ok_or("нет width в ответе")?;
    let height = get_u32(&meta, "height").ok_or("нет height в ответе")?;
    let stride = get_u32(&meta, "stride").unwrap_or(width * 4);
    let format = get_u32(&meta, "format").unwrap_or(0);

    let need = stride as usize * height as usize;
    if bytes.len() < need {
        return Err(format!(
            "мало данных: {} из {} (w={width} h={height} stride={stride} fmt={format})",
            bytes.len(),
            need
        ));
    }
    build_rgba(&bytes, width, height, stride, format)
}

/// QImage-форматы у KWin screenshot: обычно ARGB32(_Premultiplied) (5/6) или
/// RGBA8888 (17). В памяти little-endian ARGB32 лежит как B,G,R,A; RGBA8888 —
/// как R,G,B,A. Приводим к RGBA.
fn build_rgba(
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
) -> Result<image::RgbaImage, String> {
    let mut img = image::RgbaImage::new(width, height);
    let bgra = !matches!(format, 17..=19); // не RGBA8888* → считаем ARGB32 (BGRA в памяти)
    for row in 0..height {
        let base = row as usize * stride as usize;
        for col in 0..width {
            let p = base + col as usize * 4;
            let (r, g, b, a) = if bgra {
                (bytes[p + 2], bytes[p + 1], bytes[p], bytes[p + 3])
            } else {
                (bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3])
            };
            img.put_pixel(col, row, image::Rgba([r, g, b, a]));
        }
    }
    Ok(img)
}

fn get_u32(meta: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    let v = meta.get(key)?;
    u32::try_from(v)
        .ok()
        .or_else(|| i32::try_from(v).ok().map(|n| n as u32))
}

/// Отладка: `health-widget --grab-test` — тянет маленькую область и печатает,
/// авторизованы ли мы и что вернул KWin.
pub fn grab_test() {
    match capture_area(100, 100, 300, 200) {
        Ok(img) => {
            let out = std::env::temp_dir().join("hw-grabtest.png");
            let _ = img.save(&out);
            println!(
                "OK: {}x{} сохранено в {}",
                img.width(),
                img.height(),
                out.display()
            );
        }
        Err(e) => println!("FAIL: {e}"),
    }
}
