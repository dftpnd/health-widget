use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::pilot_summary::Summary;

const HEARTBEAT: Duration = Duration::from_secs(20 * 60);
const TICK: Duration = Duration::from_secs(5 * 60);

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
    crate::telemetry::now_local()[..10].to_string()
}

pub fn push(cfg: &Cfg, payload: &str) -> Result<(), String> {
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "-m",
            "5",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            &cfg.url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("нет stdin")?
        .write_all(payload.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() && out.stderr.is_empty() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn spawn(dir: PathBuf, bin: PathBuf, profiles: Vec<String>) -> Option<JoinHandle<()>> {
    spawn_with(load_cfg(), dir, bin, profiles)
}

pub fn spawn_with(
    cfg: Option<Cfg>,
    dir: PathBuf,
    bin: PathBuf,
    profiles: Vec<String>,
) -> Option<JoinHandle<()>> {
    let cfg = cfg?;
    Some(std::thread::spawn(move || {
        let mut prev: Option<i64> = None;
        let mut last = Instant::now();
        loop {
            let sums: Vec<Summary> = profiles
                .iter()
                .filter_map(|p| crate::pilot_summary::fetch(&dir, &bin, p, None))
                .collect();
            if let Some((applied, limit)) = totals(&sums) {
                if should_push(prev, applied, last.elapsed()) {
                    match push(&cfg, &body(&cfg, applied, limit, &today())) {
                        Ok(()) => {
                            crate::telemetry::event(
                                "alice.push",
                                serde_json::json!({ "applied": applied, "limit": limit }),
                            );
                            prev = Some(applied);
                            last = Instant::now();
                        }
                        Err(e) => crate::telemetry::error("alice.push", &e),
                    }
                }
            }
            std::thread::sleep(TICK);
        }
    }))
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

    #[test]
    fn push_reports_failure_for_unreachable_url() {
        let cfg = Cfg { url: "http://127.0.0.1:1/none".to_string(), token: "t".to_string() };
        let err = push(&cfg, &body(&cfg, 1, 2, "2026-08-04")).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn spawn_without_config_is_a_noop() {
        let handle = spawn_with(
            None,
            PathBuf::from("/nonexistent"),
            PathBuf::from("/nonexistent"),
            vec![],
        );
        assert!(handle.is_none());
    }
}
