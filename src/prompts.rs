use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Prompts {
    #[serde(default)]
    pub prompt_1: String,
    #[serde(default)]
    pub prompt_2: String,
    #[serde(default = "default_active")]
    pub active: u8,
}

impl Default for Prompts {
    fn default() -> Self {
        Self {
            prompt_1: String::new(),
            prompt_2: String::new(),
            active: 1,
        }
    }
}

fn default_active() -> u8 {
    1
}

impl Prompts {
    pub fn active_text(&self) -> &str {
        if self.active == 2 {
            &self.prompt_2
        } else {
            &self.prompt_1
        }
    }
}

fn path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".health-widget-prompts.json")
}

pub fn load() -> Prompts {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(p: &Prompts) {
    if let Ok(s) = serde_json::to_string_pretty(p) {
        let _ = std::fs::write(path(), s);
    }
}
