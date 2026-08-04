use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::pilot_summary::Summary;

const HEARTBEAT: Duration = Duration::from_secs(20 * 60);

#[derive(Clone)]
pub struct Cfg {
    pub url: String,
    pub token: String,
}

pub fn cfg_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("health-widget").join("alice.json"))
}

pub fn parse_cfg(raw: &str) -> Option<Cfg> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let url = v.get("url")?.as_str()?.to_string();
    let token = v.get("token")?.as_str()?.to_string();
    if url.is_empty() || token.is_empty() {
        return None;
    }
    Some(Cfg { url, token })
}

pub fn load_cfg() -> Option<Cfg> {
    let raw = std::fs::read_to_string(cfg_path()?).ok()?;
    parse_cfg(&raw)
}

pub fn totals(sums: &[Summary]) -> Option<(i64, i64)> {
    if sums.is_empty() {
        return None;
    }
    let applied = sums.iter().map(|s| s.applied_today).sum();
    let limit = sums.iter().map(|s| s.daily_limit).sum();
    Some((applied, limit))
}

pub fn should_push(prev: Option<i64>, cur: i64, since_last: Duration) -> bool {
    match prev {
        None => true,
        Some(p) => p != cur || since_last >= HEARTBEAT,
    }
}

pub fn body(cfg: &Cfg, applied: i64, limit: i64, date: &str) -> String {
    serde_json::json!({
        "token": cfg.token,
        "applied_today": applied,
        "daily_limit": limit,
        "date": date,
    })
    .to_string()
}

pub fn today() -> String {
    Command::new("date")
        .arg("+%F")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pilot_summary::Summary;

    fn sum(applied: i64, limit: i64) -> Summary {
        Summary { applied_today: applied, daily_limit: limit, ..Default::default() }
    }

    #[test]
    fn totals_sum_across_profiles() {
        let got = totals(&[sum(9, 100), sum(5, 100)]);
        assert_eq!(got, Some((14, 200)));
    }

    #[test]
    fn totals_of_nothing_is_none() {
        assert_eq!(totals(&[]), None);
    }

    #[test]
    fn first_push_always_happens() {
        assert!(should_push(None, 0, Duration::from_secs(0)));
    }

    #[test]
    fn changed_value_pushes_immediately() {
        assert!(should_push(Some(13), 14, Duration::from_secs(60)));
    }

    #[test]
    fn same_value_stays_quiet_for_a_while() {
        assert!(!should_push(Some(14), 14, Duration::from_secs(5 * 60)));
    }

    #[test]
    fn same_value_pushes_as_heartbeat_after_twenty_minutes() {
        assert!(should_push(Some(14), 14, Duration::from_secs(21 * 60)));
    }

    #[test]
    fn body_carries_token_and_counts() {
        let cfg = Cfg { url: "u".to_string(), token: "s3cret".to_string() };
        let js: serde_json::Value =
            serde_json::from_str(&body(&cfg, 14, 200, "2026-08-04")).unwrap();
        assert_eq!(js["token"], "s3cret");
        assert_eq!(js["applied_today"], 14);
        assert_eq!(js["daily_limit"], 200);
        assert_eq!(js["date"], "2026-08-04");
    }

    #[test]
    fn today_is_iso_date() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(d.matches('-').count(), 2);
    }

    #[test]
    fn cfg_parses_url_and_token() {
        let cfg = parse_cfg(r#"{"url":"https://x/y","token":"t"}"#).unwrap();
        assert_eq!(cfg.url, "https://x/y");
        assert_eq!(cfg.token, "t");
    }

    #[test]
    fn cfg_without_url_is_rejected() {
        assert!(parse_cfg(r#"{"token":"t"}"#).is_none());
    }
}
