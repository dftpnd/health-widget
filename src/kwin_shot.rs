
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;

use zbus::blocking::Connection;
use zbus::zvariant::{Fd, OwnedValue, Value};

pub fn capture_area(x: i32, y: i32, w: u32, h: u32) -> Result<image::RgbaImage, String> {
    if w == 0 || h == 0 {
        return Err("нулевая область".into());
    }
    let (mut ours, theirs) = UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;

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

fn build_rgba(
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
) -> Result<image::RgbaImage, String> {
    let mut img = image::RgbaImage::new(width, height);
    let bgra = !matches!(format, 17..=19);
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
