
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use std::path::Path;

use crate::recorder::WavRecorder;
use crate::transcribe::Transcriber;
use crate::transcript_log::TranscriptLog;

const CAP: usize = 2048;
const RATE: &str = "44100";
const RATE_HZ: u32 = 44100;
const SIGNAL_FLOOR: f32 = 1e-4;

fn record_args(target: Option<&str>, capture_sink: bool) -> Vec<String> {
    let mut args: Vec<String> = ["--rate", RATE, "--channels", "1", "--format", "f32"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if capture_sink {
        args.push("-P".to_string());
        args.push("{ stream.capture.sink = true }".to_string());
    }
    if let Some(t) = target {
        args.push("--target".to_string());
        args.push(t.to_string());
    }
    args.push("-".to_string());
    args
}

fn has_signal(batch: &[f32]) -> bool {
    batch.iter().any(|v| v.abs() > SIGNAL_FLOOR)
}

pub struct Device {
    pub target: String,
    pub label: String,
}

fn run_pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn list_mics() -> Vec<Device> {
    let text = match run_pactl(&["list", "sources"]) {
        Some(t) => t,
        None => return Vec::new(),
    };
    text.split("Source #").skip(1).filter_map(parse_mic).collect()
}

fn parse_mic(block: &str) -> Option<Device> {
    let mut name = None;
    let mut desc = None;
    let mut is_monitor = false;
    for line in block.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Name: ") {
            name = Some(v.to_string());
        } else if let Some(v) = t.strip_prefix("Description: ") {
            desc = Some(v.to_string());
        } else if let Some(v) = t.strip_prefix("Monitor of Sink: ") {
            if v != "n/a" {
                is_monitor = true;
            }
        }
    }
    if is_monitor {
        return None;
    }
    let name = name?;
    let label = format!("🎤 {}", desc.unwrap_or_else(|| name.clone()));
    Some(Device { target: name, label })
}

pub fn list_programs() -> Vec<Device> {
    let out = match Command::new("pw-dump").output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let objs: Vec<serde_json::Value> = match serde_json::from_slice(&out) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut res = Vec::new();
    for o in &objs {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let info = match o.get("info") {
            Some(i) => i,
            None => continue,
        };
        let props = match info.get("props") {
            Some(p) => p,
            None => continue,
        };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Stream/Output/Audio") {
            continue;
        }
        let target = match props.get("object.serial") {
            Some(v) => match v.as_u64() {
                Some(n) => n.to_string(),
                None => match v.as_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                },
            },
            None => continue,
        };
        if info.get("state").and_then(|s| s.as_str()) != Some("running") {
            continue;
        }
        let app = props
            .get("application.name")
            .and_then(|s| s.as_str())
            .or_else(|| props.get("node.name").and_then(|s| s.as_str()))
            .unwrap_or("?");
        let media = props
            .get("media.name")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let label = if media.is_empty() {
            format!("🔊 {app}")
        } else {
            format!("🔊 {app} — {media}")
        };
        res.push(Device { target, label });
    }

    let all_labels: Vec<String> = res.iter().map(|d| d.label.clone()).collect();
    let mut used: Vec<String> = Vec::new();
    for d in res.iter_mut() {
        if all_labels.iter().filter(|l| **l == d.label).count() > 1 {
            let n = used.iter().filter(|l| **l == d.label).count() + 1;
            used.push(d.label.clone());
            d.label = format!("{} #{n}", d.label);
        }
    }
    res
}

pub struct AudioMonitor {
    samples: Arc<Mutex<VecDeque<f32>>>,
    child: Child,
    transcriber: Option<Transcriber>,
    recorder: Arc<Mutex<Option<WavRecorder>>>,
    channel: &'static str,
    last_signal: Arc<Mutex<std::time::Instant>>,
}

impl AudioMonitor {
    pub fn start(
        target: Option<&str>,
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
    ) -> Option<Self> {
        Self::start_with(target, false, channel, log)
    }

    pub fn start_sink_monitor(
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
    ) -> Option<Self> {
        Self::start_with(None, true, channel, log)
    }

    fn start_with(
        target: Option<&str>,
        capture_sink: bool,
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
    ) -> Option<Self> {
        let mut cmd = Command::new("pw-record");
        cmd.args(record_args(target, capture_sink));
        let mut child = match cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                crate::telemetry::error("audio.fail", &format!("{channel}: {e}"));
                return None;
            }
        };

        let mut stdout = child.stdout.take()?;
        let samples = Arc::new(Mutex::new(VecDeque::with_capacity(CAP)));
        let buf = samples.clone();

        let recorder: Arc<Mutex<Option<WavRecorder>>> = Arc::new(Mutex::new(None));
        let rec = recorder.clone();

        let last_signal = Arc::new(Mutex::new(std::time::Instant::now()));
        let sig = last_signal.clone();

        let (transcriber, mut feeder) = match Transcriber::start(RATE_HZ, channel, log) {
            Some((t, f)) => (Some(t), Some(f)),
            None => (None, None),
        };

        std::thread::spawn(move || {
            let mut acc: Vec<u8> = Vec::with_capacity(8192);
            let mut raw = [0u8; 4096];
            let mut batch: Vec<f32> = Vec::with_capacity(2048);
            loop {
                match stdout.read(&mut raw) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        acc.extend_from_slice(&raw[..n]);
                        let full = acc.len() / 4 * 4;
                        if full == 0 {
                            continue;
                        }
                        batch.clear();
                        let mut i = 0;
                        while i < full {
                            batch.push(f32::from_le_bytes([acc[i], acc[i + 1], acc[i + 2], acc[i + 3]]));
                            i += 4;
                        }
                        if has_signal(&batch) {
                            if let Ok(mut t) = sig.lock() {
                                *t = std::time::Instant::now();
                            }
                        }
                        if let Ok(mut g) = buf.lock() {
                            for &v in &batch {
                                if g.len() >= CAP {
                                    g.pop_front();
                                }
                                g.push_back(v);
                            }
                        }
                        if let Some(f) = feeder.as_mut() {
                            f.feed(&batch);
                        }
                        if let Ok(mut r) = rec.lock() {
                            if let Some(w) = r.as_mut() {
                                w.write(&batch);
                            }
                        }
                        acc.drain(..full);
                    }
                }
            }
        });

        crate::telemetry::event(
            "audio.start",
            serde_json::json!({ "channel": channel, "target": target, "sink_monitor": capture_sink }),
        );
        Some(Self { samples, child, transcriber, recorder, channel, last_signal })
    }

    pub fn silent_for(&self) -> std::time::Duration {
        self.last_signal
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    pub fn transcript(&self) -> Option<(String, String)> {
        self.transcriber.as_ref().map(|t| t.text())
    }

    pub fn clear_transcript(&self) {
        if let Some(t) = &self.transcriber {
            t.clear();
        }
    }

    pub fn start_recording(&self, path: &Path) -> std::io::Result<()> {
        let w = WavRecorder::create(path, RATE_HZ)?;
        if let Ok(mut g) = self.recorder.lock() {
            *g = Some(w);
        }
        Ok(())
    }

    pub fn stop_recording(&self) {
        if let Ok(mut g) = self.recorder.lock() {
            g.take();
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    pub fn snapshot(&self, out: &mut Vec<f32>) {
        out.clear();
        if let Ok(g) = self.samples.lock() {
            out.extend(g.iter().copied());
        }
    }

    pub fn samples_handle(&self) -> Arc<Mutex<VecDeque<f32>>> {
        self.samples.clone()
    }

    pub fn fresh_finals(&self) -> Option<Arc<Mutex<VecDeque<String>>>> {
        self.transcriber.as_ref().map(|t| t.fresh_handle())
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        crate::telemetry::event("audio.stop", serde_json::json!({ "channel": self.channel }));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_args_program_target() {
        let a = record_args(Some("244"), false);
        assert_eq!(
            a,
            ["--rate", "44100", "--channels", "1", "--format", "f32", "--target", "244", "-"]
        );
    }

    #[test]
    fn record_args_default_sink_monitor() {
        let a = record_args(None, true);
        assert_eq!(
            a,
            [
                "--rate",
                "44100",
                "--channels",
                "1",
                "--format",
                "f32",
                "-P",
                "{ stream.capture.sink = true }",
                "-"
            ]
        );
    }

    #[test]
    fn signal_detection_ignores_digital_silence() {
        assert!(!has_signal(&[0.0; 512]));
        assert!(!has_signal(&[5e-5, -5e-5]));
        assert!(has_signal(&[0.0, 0.002, 0.0]));
    }
}
