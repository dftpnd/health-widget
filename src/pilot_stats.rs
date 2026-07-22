
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize, Clone, Default)]
pub struct PilotStats {
    #[serde(default)]
    pub applied_total: i64,
    #[serde(default)]
    pub applied_today: i64,
    #[serde(default)]
    pub daily_limit: i64,
    #[serde(default)]
    pub chats_acted: i64,
}

pub fn load(path: &Path) -> Option<PilotStats> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
