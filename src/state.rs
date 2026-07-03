//! Сохранение/восстановление состояния виджета между запусками.
//!
//! Пишем в `state.json` рядом с конфигом (config_dir/health-widget/). Храним размер, позицию,
//! выбранный источник звука и флаг «поверх всех». Позиция на Wayland может быть недоступна
//! клиенту (тогда x/y = null и не восстанавливается) — размер/источник/закрепление сохраняются.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct State {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// Включён ли канал микрофона (слушатель + осциллограф).
    #[serde(default)]
    pub mic_on: bool,
    /// Выбранный микрофон (имя источника; None — по умолчанию).
    #[serde(default)]
    pub mic_target: Option<String>,
    /// Включён ли канал звука программы/вывода (Zoom/Телемост/Discord…).
    #[serde(default)]
    pub zoom_on: bool,
    /// Закреплено ли окно «поверх всех».
    #[serde(default)]
    pub pinned: bool,
}

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("health-widget")
        .join("state.json")
}

pub fn load() -> State {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(st: &State) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(s) = serde_json::to_string_pretty(st) {
        let _ = std::fs::write(&p, s);
    }
}
