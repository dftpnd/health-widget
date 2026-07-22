
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::transcript_log::TranscriptLog;

const STT_RATE: f64 = 16000.0;
const MAX_FINALS: usize = 50_000;
const FRESH_CAP: usize = 32;

#[derive(Default)]
pub struct Transcript {
    pub finals: String,
    pub partial: String,
}

pub struct Transcriber {
    state: Arc<Mutex<Transcript>>,
    fresh: Arc<Mutex<VecDeque<String>>>,
    child: Child,
    channel: &'static str,
}

pub struct Feeder {
    stdin: ChildStdin,
    ratio: f64,
    pos: f64,
    prev: f32,
    out: Vec<u8>,
    dead: bool,
    channel: &'static str,
}

impl Feeder {
    pub fn feed(&mut self, s: &[f32]) {
        if self.dead || s.is_empty() {
            return;
        }
        self.out.clear();
        let len = s.len() as f64;
        while self.pos < len - 1.0 {
            let i = self.pos.floor();
            let frac = (self.pos - i) as f32;
            let ii = i as isize;
            let a = if ii < 0 { self.prev } else { s[ii as usize] };
            let b = s[(ii + 1) as usize];
            let v = (a + (b - a) * frac).clamp(-1.0, 1.0);
            let q = (v * 32767.0) as i16;
            self.out.extend_from_slice(&q.to_le_bytes());
            self.pos += self.ratio;
        }
        self.pos -= len;
        self.prev = s[s.len() - 1];

        if !self.out.is_empty() {
            if let Err(e) = self.stdin.write_all(&self.out) {
                self.dead = true;
                crate::telemetry::event(
                    "stt.feed.dead",
                    serde_json::json!({ "channel": self.channel, "err": e.to_string() }),
                );
            }
        }
    }
}

impl Transcriber {
    pub fn start(
        src_rate: u32,
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
    ) -> Option<(Transcriber, Feeder)> {
        if std::env::var("HEALTH_TRANSCRIBE").as_deref() == Ok("0") {
            return None;
        }
        if let Some(free) = free_vram_mib() {
            let need = min_free_mib();
            if free < need {
                crate::telemetry::error(
                    "stt.low_vram",
                    &format!("{channel}: свободно {free} MiB < порога {need} MiB"),
                );
                return None;
            }
        }
        let python = python_path()?;
        let model = model_spec();
        let script = ensure_script()?;

        let mut child = Command::new(&python)
            .arg(&script)
            .arg(&model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;

        let pid = child.id();
        if let Some(errout) = child.stderr.take() {
            std::thread::spawn(move || {
                let reader = BufReader::new(errout);
                for line in reader.lines() {
                    match line {
                        Ok(l) if !l.trim().is_empty() => crate::telemetry::event(
                            "stt.stderr",
                            serde_json::json!({ "channel": channel, "pid": pid, "line": l }),
                        ),
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            });
        }

        let stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let state = Arc::new(Mutex::new(Transcript::default()));
        let fresh: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));

        {
            let state = state.clone();
            let fresh = fresh.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                let mut reason = "eof";
                for line in reader.lines() {
                    let line = match line {
                        Ok(l) => l,
                        Err(_) => {
                            reason = "read_err";
                            break;
                        }
                    };
                    let v: serde_json::Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let Ok(mut g) = state.lock() else { break };
                    if let Some(p) = v.get("partial").and_then(|p| p.as_str()) {
                        g.partial = p.to_string();
                    } else if let Some(t) = v.get("final").and_then(|t| t.as_str()) {
                        if !g.finals.is_empty() {
                            g.finals.push(' ');
                        }
                        g.finals.push_str(t);
                        trim_head(&mut g.finals, MAX_FINALS);
                        g.partial.clear();
                        if let Ok(mut q) = fresh.lock() {
                            if q.len() >= FRESH_CAP {
                                q.pop_front();
                            }
                            q.push_back(t.to_string());
                        }
                        if let Some(log) = &log {
                            log.append(channel, t);
                        }
                    }
                }
                crate::telemetry::event(
                    "stt.reader.end",
                    serde_json::json!({ "channel": channel, "reason": reason }),
                );
            });
        }

        let feeder = Feeder {
            stdin,
            ratio: src_rate as f64 / STT_RATE,
            pos: 0.0,
            prev: 0.0,
            out: Vec::with_capacity(4096),
            dead: false,
            channel,
        };
        crate::telemetry::event(
            "stt.start",
            serde_json::json!({ "channel": channel, "model": model }),
        );
        Some((Transcriber { state, fresh, child, channel }, feeder))
    }

    pub fn fresh_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.fresh.clone()
    }

    pub fn state_handle(&self) -> Arc<Mutex<Transcript>> {
        self.state.clone()
    }

    pub fn text(&self) -> (String, String) {
        match self.state.lock() {
            Ok(g) => (g.finals.clone(), g.partial.clone()),
            Err(_) => (String::new(), String::new()),
        }
    }

    pub fn clear(&self) {
        if let Ok(mut g) = self.state.lock() {
            g.finals.clear();
            g.partial.clear();
        }
    }
}

impl Drop for Transcriber {
    fn drop(&mut self) {
        crate::telemetry::event("stt.stop", serde_json::json!({ "channel": self.channel }));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget"))
}

fn python_path() -> Option<PathBuf> {
    let p = match std::env::var_os("WHISPER_PYTHON") {
        Some(v) => PathBuf::from(v),
        None => data_dir()?.join("venv-whisper").join("bin").join("python"),
    };
    p.exists().then_some(p)
}

fn model_spec() -> String {
    std::env::var("WHISPER_MODEL").unwrap_or_else(|_| "large-v3".to_string())
}

pub fn script_path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("whisper_stream.py"))
}

fn free_vram_mib() -> Option<u64> {
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.free", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_free_mib(&String::from_utf8_lossy(&out.stdout))
}

fn parse_free_mib(s: &str) -> Option<u64> {
    s.lines().filter_map(|l| l.trim().parse().ok()).min()
}

fn min_free_mib() -> u64 {
    std::env::var("HEALTH_STT_MIN_FREE_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5000)
}

fn ensure_script() -> Option<PathBuf> {
    const SRC: &str = include_str!("../scripts/whisper_stream.py");
    let path = script_path()?;
    let need_write = std::fs::read_to_string(&path).map(|c| c != SRC).unwrap_or(true);
    if need_write {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, SRC).ok()?;
    }
    Some(path)
}

fn trim_head(s: &mut String, max: usize) {
    let n = s.chars().count();
    if n <= max {
        return;
    }
    let skip = n - max;
    let mut cut = s.char_indices().nth(skip).map(|(i, _)| i).unwrap_or(0);
    if let Some(rel) = s[cut..].find(' ') {
        cut += rel + 1;
    }
    s.replace_range(..cut, "");
}

#[cfg(test)]
mod tests {
    use super::{parse_free_mib, trim_head};

    #[test]
    fn parse_single_gpu() {
        assert_eq!(parse_free_mib("11650\n"), Some(11650));
    }

    #[test]
    fn parse_multi_gpu_takes_min() {
        assert_eq!(parse_free_mib("11650\n 512 \n"), Some(512));
    }

    #[test]
    fn parse_garbage_is_none() {
        assert_eq!(parse_free_mib("N/A\n"), None);
        assert_eq!(parse_free_mib(""), None);
    }

    #[test]
    fn short_string_unchanged() {
        let mut s = "коротко".to_string();
        trim_head(&mut s, 100);
        assert_eq!(s, "коротко");
    }

    #[test]
    fn trims_head_to_word_boundary() {
        let mut s = "one two three".to_string();
        trim_head(&mut s, 5);
        assert_eq!(s, "three");
    }

    #[test]
    fn unicode_boundary_no_panic() {
        let mut s = "аб вг де".to_string();
        trim_head(&mut s, 2);
        assert_eq!(s, "де");
        assert_eq!(s.chars().count(), 2);
    }
}
