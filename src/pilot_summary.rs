use std::path::Path;
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize, Clone, Default)]
pub struct Counter {
    #[serde(default)]
    pub turn: i64,
    #[serde(default)]
    pub total: i64,
}

#[derive(Deserialize, Clone, Default)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub new: i64,
}

#[derive(Deserialize, Clone, Default)]
pub struct Summary {
    #[serde(default)]
    pub applied: Counter,
    #[serde(default)]
    pub replies: Counter,
    #[serde(default)]
    pub captcha: Counter,
    #[serde(default)]
    pub forms: Counter,
    #[serde(default)]
    pub http_403: Counter,
    #[serde(default)]
    pub expired: Counter,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub unenriched: i64,
    #[serde(default)]
    pub pool_total: i64,
    #[serde(default)]
    pub found_today: i64,
    #[serde(default)]
    pub enriched_today: i64,
    #[serde(default)]
    pub last_scan_date: Option<String>,
    #[serde(default)]
    pub top: Vec<String>,
    #[serde(default)]
    pub ranked: i64,
    #[serde(default)]
    pub applied_today: i64,
    #[serde(default)]
    pub daily_limit: i64,
}

pub fn fetch(dir: &Path, bin: &Path, profile: &str, since: Option<&str>) -> Option<Summary> {
    let mut cmd = Command::new(bin);
    cmd.arg("summary").args(["--profile", profile]);
    if let Some(s) = since {
        cmd.args(["--since", s]);
    }
    let out = cmd.current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text.lines().rev().find(|l| l.trim_start().starts_with('{'))?;
    serde_json::from_str(last).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_summary_json() {
        let js = r#"{
          "applied":{"turn":12,"total":340},"replies":{"turn":3,"total":88},
          "captcha":{"turn":1,"total":20},"forms":{"turn":0,"total":7},
          "http_403":{"turn":5,"total":210},"expired":{"turn":4,"total":190},
          "groups":[{"name":"Backend Go","new":8},{"name":"Rust","new":2}],
          "unenriched":10,"pool_total":530,"found_today":42,"enriched_today":18,
          "last_scan_date":"2026-07-24","top":["Go dev"],"ranked":17,
          "applied_today":12,"daily_limit":200,"updated_at":"x"
        }"#;
        let s: Summary = serde_json::from_str(js).unwrap();
        assert_eq!(s.applied.turn, 12);
        assert_eq!(s.groups.len(), 2);
        assert_eq!(s.top, vec!["Go dev".to_string()]);
        assert_eq!(s.last_scan_date.as_deref(), Some("2026-07-24"));
        assert_eq!(s.pool_total, 530);
        assert_eq!(s.found_today, 42);
        assert_eq!(s.enriched_today, 18);
        assert_eq!(s.ranked, 17);
    }
}
