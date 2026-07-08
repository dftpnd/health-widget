//! Тумблер TG-уведомлений автопилота: общий флаг `data/notify.json`
//! (`{"enabled": bool}`). Автопилот читает его перед отправкой; виджет — пишет.
//! Нет файла/битый JSON → включено (fail-open), как в автопилоте.

use std::path::{Path, PathBuf};

fn flag_path(data_dir: &Path) -> PathBuf {
    data_dir.join("notify.json")
}

/// Включены ли уведомления. Нет файла/битый JSON → true.
pub fn read_enabled(data_dir: &Path) -> bool {
    match std::fs::read_to_string(flag_path(data_dir)) {
        Ok(s) => serde_json::from_str::<serde_json::Value>(&s)
            .ok()
            .and_then(|v| v.get("enabled").and_then(|e| e.as_bool()))
            .unwrap_or(true),
        Err(_) => true,
    }
}

/// Записать состояние тумблера в `data/notify.json`.
pub fn set_enabled(data_dir: &Path, value: bool) -> std::io::Result<()> {
    if let Some(parent) = flag_path(data_dir).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(flag_path(data_dir), format!("{{\"enabled\":{value}}}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_means_enabled() {
        let dir = std::env::temp_dir().join("hw_notify_missing");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(read_enabled(&dir));
    }

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join("hw_notify_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        set_enabled(&dir, false).unwrap();
        assert!(!read_enabled(&dir));
        set_enabled(&dir, true).unwrap();
        assert!(read_enabled(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
