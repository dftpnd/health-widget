//! Конфиг виджета. Значения берутся из переменных окружения (удобно для .desktop/скриптов),
//! всё имеет разумные дефолты.

use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone)]
pub struct Config {
    pub json_path: PathBuf,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Прозрачность подложки виджета (0..=255).
    pub bg_alpha: u8,
    /// Пропускать ли клики мыши насквозь.
    pub click_through: bool,
    /// Прятать ли виджет при обнаружении захвата экрана.
    pub auto_hide_on_share: bool,
    /// Период опроса детектора.
    pub detect_poll: Duration,
}

impl Default for Config {
    fn default() -> Self {
        let json_path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("health-widget")
            .join("metrics.json");
        Self {
            json_path,
            x: 40.0,
            y: 60.0,
            width: 260.0,
            height: 200.0,
            bg_alpha: 220,
            click_through: false,
            auto_hide_on_share: true,
            detect_poll: Duration::from_millis(1000),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config::default();
        if let Ok(v) = std::env::var("HEALTH_JSON") {
            c.json_path = PathBuf::from(v);
        }
        if let Some(v) = env_f32("HEALTH_X") {
            c.x = v;
        }
        if let Some(v) = env_f32("HEALTH_Y") {
            c.y = v;
        }
        if let Some(v) = env_f32("HEALTH_W") {
            c.width = v;
        }
        if let Some(v) = env_f32("HEALTH_H") {
            c.height = v;
        }
        if let Some(v) = env_f32("HEALTH_BG_ALPHA") {
            c.bg_alpha = v.clamp(0.0, 255.0) as u8;
        }
        if let Some(v) = env_bool("HEALTH_CLICK_THROUGH") {
            c.click_through = v;
        }
        if let Some(v) = env_bool("HEALTH_AUTO_HIDE") {
            c.auto_hide_on_share = v;
        }
        c
    }
}

fn env_f32(k: &str) -> Option<f32> {
    std::env::var(k).ok()?.trim().parse().ok()
}

fn env_bool(k: &str) -> Option<bool> {
    let v = std::env::var(k).ok()?;
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
