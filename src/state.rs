
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct State {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    #[serde(default)]
    pub mic_on: bool,
    #[serde(default)]
    pub mic_target: Option<String>,
    #[serde(default)]
    pub zoom_on: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub pilot_profile: Option<String>,
    #[serde(default)]
    pub llm_provider: Option<String>,
    #[serde(default, alias = "chat_width")]
    pub terminal_width: Option<f32>,
    #[serde(default)]
    pub autopilot_collapsed: bool,
    #[serde(default, alias = "metrics_collapsed")]
    pub scopes_collapsed: bool,
    #[serde(default)]
    pub chat_collapsed: bool,
    #[serde(default)]
    pub terminal_open: bool,
    #[serde(default)]
    pub clip_open: bool,
    #[serde(default)]
    pub clip_x: Option<f32>,
    #[serde(default)]
    pub clip_y: Option<f32>,
    #[serde(default)]
    pub chat_open: bool,
    #[serde(default)]
    pub chat_x: Option<f32>,
    #[serde(default)]
    pub chat_y: Option<f32>,
    #[serde(default)]
    pub chat_w: Option<f32>,
    #[serde(default)]
    pub chat_h: Option<f32>,
    #[serde(default)]
    pub web_x: Option<f32>,
    #[serde(default)]
    pub web_y: Option<f32>,
    #[serde(default)]
    pub web_w: Option<f32>,
    #[serde(default)]
    pub web_h: Option<f32>,
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
