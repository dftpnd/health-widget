use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

static SINK: OnceLock<Sender<String>> = OnceLock::new();
const MAX_BYTES: u64 = 5 * 1024 * 1024;

pub fn path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget").join("telemetry.jsonl"))
}

pub fn init() {
    if std::env::var("HEALTH_TELEMETRY").as_deref() == Ok("0") {
        return;
    }
    let Some(p) = path() else {
        return;
    };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    rotate_if_big(&p);
    let (tx, rx) = mpsc::channel::<String>();
    if SINK.set(tx).is_err() {
        return;
    }
    std::thread::spawn(move || {
        let mut file = match OpenOptions::new().create(true).append(true).open(&p) {
            Ok(f) => f,
            Err(_) => return,
        };
        for line in rx {
            let _ = file.write_all(line.as_bytes());
            let _ = file.write_all(b"\n");
            let _ = file.flush();
        }
    });
    event(
        "app.start",
        serde_json::json!({ "version": env!("CARGO_PKG_VERSION"), "pid": std::process::id() }),
    );
}

pub fn event(ev: &str, fields: Value) {
    let Some(tx) = SINK.get() else {
        return;
    };
    let _ = tx.send(build_record(now_ms(), ev, fields));
}

pub fn error(ev: &str, err: &str) {
    event(ev, serde_json::json!({ "err": err }));
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn build_record(ts: i64, ev: &str, fields: Value) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("ts".to_string(), Value::from(ts));
    obj.insert("ev".to_string(), Value::from(ev));
    if let Value::Object(m) = fields {
        for (k, v) in m {
            obj.insert(k, v);
        }
    }
    Value::Object(obj).to_string()
}

fn should_rotate(size: u64) -> bool {
    size >= MAX_BYTES
}

fn rotate_if_big(p: &Path) {
    if let Ok(meta) = std::fs::metadata(p) {
        if should_rotate(meta.len()) {
            let _ = std::fs::rename(p, p.with_extension("jsonl.1"));
        }
    }
}

fn fmt_local(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn today_prefix() -> String {
    fmt_local(now_ms())[..10].to_string()
}

fn render(v: &Value) -> String {
    let ts = v.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
    let ev = v.get("ev").and_then(|e| e.as_str()).unwrap_or("?");
    let mut extra = String::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            if k == "ts" || k == "ev" {
                continue;
            }
            let s = match val {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            extra.push_str(&format!(" {k}={s}"));
        }
    }
    format!("{}  {:<22}{}", fmt_local(ts), ev, extra)
}

pub fn dump(limit: usize, today_only: bool) -> Option<String> {
    let text = std::fs::read_to_string(path()?).ok()?;
    let today = today_only.then(today_prefix);
    let mut rendered: Vec<String> = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let s = render(&v);
        if let Some(t) = &today {
            if !s.starts_with(t.as_str()) {
                continue;
            }
        }
        rendered.push(s);
    }
    let start = rendered.len().saturating_sub(limit);
    Some(rendered[start..].join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_has_ts_ev_and_merges_fields() {
        let r = build_record(1720526400123, "audio.mic.start", serde_json::json!({ "target": "mic0" }));
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["ts"], 1720526400123i64);
        assert_eq!(v["ev"], "audio.mic.start");
        assert_eq!(v["target"], "mic0");
    }

    #[test]
    fn record_without_fields_ok() {
        let r = build_record(1, "app.shutdown", serde_json::json!({}));
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["ev"], "app.shutdown");
        assert!(v.get("target").is_none());
    }

    #[test]
    fn non_object_fields_ignored() {
        let r = build_record(1, "x", serde_json::json!([1, 2, 3]));
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["ev"], "x");
    }

    #[test]
    fn rotate_threshold() {
        assert!(!should_rotate(0));
        assert!(!should_rotate(MAX_BYTES - 1));
        assert!(should_rotate(MAX_BYTES));
    }

    #[test]
    fn render_puts_time_event_and_fields() {
        let v = serde_json::json!({ "ts": 0, "ev": "pilot.spawn", "profile": "back" });
        let s = render(&v);
        assert!(s.contains("pilot.spawn"));
        assert!(s.contains("profile=back"));
    }
}
