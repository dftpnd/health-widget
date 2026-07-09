
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct ScanGroup {
    pub name: String,
    #[serde(default)]
    pub pending: i64,
}

#[derive(Deserialize, Clone, Default)]
pub struct ScanStatus {
    #[serde(default)]
    pub groups: Vec<ScanGroup>,
    #[serde(default)]
    pub unenriched: i64,
}

pub fn load(path: &Path) -> Option<ScanStatus> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
