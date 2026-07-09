//! Копирование в системный буфер обмена Wayland через `wl-copy`.
//!
//! Почему не `egui`/`ctx.copy_text`: на Wayland eframe ставит буфер через smithay-clipboard,
//! а тот работает только когда окно в фокусе (нужен input-serial). Копия по кнопке Tartarus
//! прилетает, когда виджет НЕ в фокусе, поэтому egui-копия молча не проходит. `wl-copy` создаёт
//! свой data-source и не зависит от фокуса; он демонизируется (форкает держатель) и хранит
//! содержимое сам, так что достаточно передать текст в его stdin.

use std::io::Write;
use std::process::{Command, Stdio};

/// Записать текст в буфер, дождавшись завершения `wl-copy`. Блокирует вызывающего до форка
/// демона-держателя (быстро) — звать НЕ из UI-потока; для кадра используй [`set_async`].
/// Ошибку возвращаем строкой, чтобы вызывающий мог показать её пользователю.
pub fn set(text: &str) -> Result<(), String> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("wl-copy не запустился: {e}"))?;
    {
        let mut si = child
            .stdin
            .take()
            .ok_or_else(|| "нет stdin у wl-copy".to_string())?;
        si.write_all(text.as_bytes())
            .map_err(|e| format!("wl-copy stdin: {e}"))?;
    }
    child.wait().map_err(|e| format!("wl-copy: {e}"))?;
    Ok(())
}

/// Записать текст в буфер в фоновом потоке (не блокирует вызывающего). Ошибки молча
/// игнорируются — для копирования прямо из UI-кадра, где блокировать нельзя.
pub fn set_async(text: String) {
    std::thread::spawn(move || {
        let _ = set(&text);
    });
}
