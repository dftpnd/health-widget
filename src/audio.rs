
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use std::path::Path;

use crate::recorder::WavRecorder;
use crate::transcribe::{Health, Stt, Transcript};
use crate::transcript_log::TranscriptLog;

const CAP: usize = 2048;
const RATE: &str = "44100";
const RATE_HZ: u32 = 44100;
const SIGNAL_FLOOR: f32 = 1e-4;
const SPAWN_BACKOFF_MIN_MS: u64 = 1000;
const SPAWN_BACKOFF_MAX_MS: u64 = 30_000;
const FALLBACK_AFTER_FAILS: u32 = 3;

fn spawn_backoff_ms(fails: u32) -> u64 {
    SPAWN_BACKOFF_MIN_MS
        .saturating_mul(1u64 << fails.min(6))
        .min(SPAWN_BACKOFF_MAX_MS)
}

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

fn set_notice(slot: &Arc<Mutex<Option<String>>>, text: Option<String>) {
    if let Ok(mut g) = slot.lock() {
        *g = text;
    }
}

fn fell_back_to_sink(
    source: &mut Source,
    fails: &mut u32,
    note: &Arc<Mutex<Option<String>>>,
    channel: &'static str,
    after: &str,
) -> bool {
    if !source.falls_back_to_sink() || *fails < FALLBACK_AFTER_FAILS {
        return false;
    }
    *source = Source::Sink;
    *fails = 0;
    set_notice(note, Some("источник пропал — слушаю весь вывод".to_string()));
    crate::telemetry::event(
        "audio.fallback_sink",
        serde_json::json!({ "channel": channel, "after": after }),
    );
    true
}

pub struct Device {
    pub target: String,
    pub key: String,
    pub label: String,
}

enum Source {
    Device(Option<String>),
    Program(String),
    Sink,
}

impl Source {
    fn spawn(&self, channel: &'static str) -> Option<(Child, std::process::ChildStdout)> {
        match self {
            Source::Device(t) => spawn_pw_record(t.as_deref(), false, channel),
            Source::Sink => spawn_pw_record(None, true, channel),
            Source::Program(key) => match resolve_program(key) {
                Some(target) => spawn_pw_record(Some(&target), false, channel),
                None => {
                    crate::telemetry::event(
                        "audio.program.unresolved",
                        serde_json::json!({ "channel": channel, "key": key }),
                    );
                    None
                }
            },
        }
    }

    fn falls_back_to_sink(&self) -> bool {
        matches!(self, Source::Program(_))
    }

    fn keeps_monitor_on_spawn_failure(&self) -> bool {
        matches!(self, Source::Program(_))
    }

    fn spawn_failure_notice(&self) -> &'static str {
        match self {
            Source::Program(_) => "источник не найден, ищу",
            _ => "захват не запускается",
        }
    }

    fn describe(&self) -> (Option<&str>, bool) {
        match self {
            Source::Device(t) => (t.as_deref(), false),
            Source::Program(key) => (Some(key.as_str()), false),
            Source::Sink => (None, true),
        }
    }
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

fn jabra_target() -> Option<String> {
    list_mics()
        .into_iter()
        .find(|d| d.target.to_lowercase().contains("jabra") || d.label.to_lowercase().contains("jabra"))
        .map(|d| d.target)
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
    Some(Device { key: name.clone(), target: name, label })
}

fn resolve_program(key: &str) -> Option<String> {
    list_programs()
        .into_iter()
        .find(|d| d.key == key)
        .map(|d| d.target)
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
        let key = if media.is_empty() {
            format!("🔊 {app}")
        } else {
            format!("🔊 {app} — {media}")
        };
        res.push(Device { target, label: key.clone(), key });
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

fn spawn_pw_record(
    target: Option<&str>,
    capture_sink: bool,
    channel: &'static str,
) -> Option<(Child, std::process::ChildStdout)> {
    let mut cmd = Command::new("pw-record");
    cmd.args(record_args(target, capture_sink));
    let mut child = match cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => {
            crate::telemetry::error("audio.fail", &format!("{channel}: {e}"));
            return None;
        }
    };

    let pid = child.id();
    crate::telemetry::event(
        "audio.pw_record.spawn",
        serde_json::json!({ "channel": channel, "pid": pid, "target": target, "sink_monitor": capture_sink }),
    );

    if let Some(errout) = child.stderr.take() {
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(errout);
            for line in std::io::BufRead::lines(reader) {
                match line {
                    Ok(l) if !l.trim().is_empty() => crate::telemetry::event(
                        "audio.pw_record.stderr",
                        serde_json::json!({ "channel": channel, "pid": pid, "line": l }),
                    ),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
    }

    let stdout = child.stdout.take()?;
    Some((child, stdout))
}

pub struct AudioMonitor {
    samples: Arc<Mutex<VecDeque<f32>>>,
    shutdown: Arc<AtomicBool>,
    current_child: Arc<Mutex<Option<Child>>>,
    stt: Arc<Stt>,
    recorder: Arc<Mutex<Option<WavRecorder>>>,
    channel: &'static str,
    last_signal: Arc<Mutex<std::time::Instant>>,
    notice: Arc<Mutex<Option<String>>>,
}

impl AudioMonitor {
    pub fn start(
        target: Option<&str>,
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
        stt: bool,
    ) -> Option<Self> {
        let target = match target {
            Some(t) => Some(t.to_string()),
            None => jabra_target(),
        };
        Self::start_with(Source::Device(target), channel, log, stt)
    }

    pub fn start_program(
        key: Option<&str>,
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
        stt: bool,
    ) -> Option<Self> {
        let source = match key {
            Some(k) => Source::Program(k.to_string()),
            None => Source::Sink,
        };
        Self::start_with(source, channel, log, stt)
    }

    fn start_with(
        source: Source,
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
        want_stt: bool,
    ) -> Option<Self> {
        let (first_child, first_stdout) = match source.spawn(channel) {
            Some((child, out)) => (Some(child), Some(out)),
            None if source.keeps_monitor_on_spawn_failure() => (None, None),
            None => return None,
        };

        let samples = Arc::new(Mutex::new(VecDeque::with_capacity(CAP)));
        let buf = samples.clone();

        let recorder: Arc<Mutex<Option<WavRecorder>>> = Arc::new(Mutex::new(None));
        let rec = recorder.clone();

        let last_signal = Arc::new(Mutex::new(std::time::Instant::now()));
        let sig = last_signal.clone();

        let shutdown = Arc::new(AtomicBool::new(false));
        let current_child = Arc::new(Mutex::new(first_child));

        let stt = Stt::new(RATE_HZ, channel, log, want_stt);
        let feed = stt.clone();

        let notice: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let note = notice.clone();

        let (desc_target, desc_sink) = {
            let (t, sink) = source.describe();
            (t.map(|s| s.to_string()), sink)
        };

        let sd = shutdown.clone();
        let cc = current_child.clone();
        std::thread::spawn(move || {
            let mut source = source;
            let mut fails: u32 = 0;
            let mut acc: Vec<u8> = Vec::with_capacity(8192);
            let mut raw = [0u8; 4096];
            let mut batch: Vec<f32> = Vec::with_capacity(2048);
            let mut pending: Option<std::process::ChildStdout> = first_stdout;
            loop {
                if sd.load(Ordering::Relaxed) {
                    break;
                }
                let mut stdout = match pending.take() {
                    Some(s) => s,
                    None => match source.spawn(channel) {
                        Some((child, out)) => {
                            if let Ok(mut g) = cc.lock() {
                                if let Some(mut old) = g.replace(child) {
                                    let _ = old.wait();
                                }
                            }
                            if sd.load(Ordering::Relaxed) {
                                if let Ok(mut g) = cc.lock() {
                                    if let Some(mut c) = g.take() {
                                        let _ = c.kill();
                                        let _ = c.wait();
                                    }
                                }
                                break;
                            }
                            out
                        }
                        None => {
                            if sd.load(Ordering::Relaxed) {
                                break;
                            }
                            fails += 1;
                            let backoff = spawn_backoff_ms(fails);
                            if !fell_back_to_sink(
                                &mut source,
                                &mut fails,
                                &note,
                                channel,
                                "spawn_fail",
                            ) {
                                set_notice(&note, Some(source.spawn_failure_notice().to_string()));
                            }
                            std::thread::sleep(std::time::Duration::from_millis(backoff));
                            continue;
                        }
                    },
                };

                let mut total_bytes: u64 = 0;
                let mut got_first = false;
                let reason;
                loop {
                    match stdout.read(&mut raw) {
                        Ok(0) => {
                            reason = "eof".to_string();
                            break;
                        }
                        Err(e) => {
                            reason = format!("read_err: {e}");
                            break;
                        }
                        Ok(n) => {
                            total_bytes += n as u64;
                            if !got_first {
                                got_first = true;
                                fails = 0;
                                set_notice(&note, None);
                                crate::telemetry::event(
                                    "audio.capture.first_bytes",
                                    serde_json::json!({ "channel": channel, "bytes": n }),
                                );
                            }
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
                            feed.feed(&batch);
                            if let Ok(mut r) = rec.lock() {
                                if let Some(w) = r.as_mut() {
                                    w.write(&batch);
                                }
                            }
                            acc.drain(..full);
                        }
                    }
                }
                if let Ok(mut g) = cc.lock() {
                    if let Some(mut c) = g.take() {
                        let _ = c.wait();
                    }
                }
                crate::telemetry::event(
                    "audio.capture.end",
                    serde_json::json!({
                        "channel": channel,
                        "reason": reason,
                        "total_bytes": total_bytes,
                        "got_first": got_first,
                    }),
                );
                acc.clear();
                if sd.load(Ordering::Relaxed) {
                    break;
                }
                let backoff = if got_first {
                    fails = 0;
                    300
                } else {
                    fails += 1;
                    spawn_backoff_ms(fails)
                };
                let switched = fell_back_to_sink(&mut source, &mut fails, &note, channel, &reason);
                if !switched && !got_first {
                    set_notice(&note, Some("источник молчит, переподключаюсь".to_string()));
                }
                crate::telemetry::event(
                    "audio.capture.restart",
                    serde_json::json!({ "channel": channel, "after": reason, "fails": fails }),
                );
                std::thread::sleep(std::time::Duration::from_millis(backoff));
            }
        });

        crate::telemetry::event(
            "audio.start",
            serde_json::json!({ "channel": channel, "target": desc_target, "sink_monitor": desc_sink }),
        );
        Some(Self {
            samples,
            shutdown,
            current_child,
            stt,
            recorder,
            channel,
            last_signal,
            notice,
        })
    }

    pub fn notice(&self) -> Option<String> {
        self.notice.lock().ok().and_then(|g| g.clone())
    }

    pub fn silent_for(&self) -> std::time::Duration {
        self.last_signal
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    pub fn transcript(&self) -> (String, String) {
        self.stt.text()
    }

    pub fn clear_transcript(&self) {
        self.stt.clear();
    }

    pub fn stt_health(&self) -> Health {
        self.stt.health()
    }

    pub fn start_recording(&self, path: &Path) -> std::io::Result<()> {
        let w = WavRecorder::append(path, RATE_HZ)?;
        if let Ok(mut g) = self.recorder.lock() {
            *g = Some(w);
        }
        Ok(())
    }

    pub fn take_recorder(&self) -> Option<WavRecorder> {
        self.recorder.lock().ok().and_then(|mut g| g.take())
    }

    pub fn adopt_recorder(&self, w: WavRecorder) {
        if let Ok(mut g) = self.recorder.lock() {
            *g = Some(w);
        }
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

    pub fn fresh_finals(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.stt.fresh_handle()
    }

    pub fn transcript_handle(&self) -> Arc<Mutex<Transcript>> {
        self.stt.state_handle()
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        crate::telemetry::event("audio.stop", serde_json::json!({ "channel": self.channel }));
        self.shutdown.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.current_child.lock() {
            if let Some(mut c) = g.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
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
    fn only_program_source_falls_back_to_sink() {
        assert!(Source::Program("🔊 Google Chrome — Playback".to_string()).falls_back_to_sink());
        assert!(!Source::Device(Some("jabra".to_string())).falls_back_to_sink());
        assert!(!Source::Device(None).falls_back_to_sink());
        assert!(!Source::Sink.falls_back_to_sink());
    }

    #[test]
    fn fallback_needs_fail_streak_and_switches_once() {
        let note = Arc::new(Mutex::new(None));
        let mut source = Source::Program("🔊 Google Chrome".to_string());
        let mut fails = FALLBACK_AFTER_FAILS - 1;
        assert!(!fell_back_to_sink(&mut source, &mut fails, &note, "test", "eof"));
        fails += 1;
        assert!(fell_back_to_sink(&mut source, &mut fails, &note, "test", "eof"));
        assert_eq!(fails, 0);
        assert!(matches!(source, Source::Sink));
        fails = FALLBACK_AFTER_FAILS;
        assert!(!fell_back_to_sink(&mut source, &mut fails, &note, "test", "eof"));
    }

    #[test]
    fn spawn_backoff_grows_and_caps() {
        assert_eq!(spawn_backoff_ms(0), SPAWN_BACKOFF_MIN_MS);
        assert_eq!(spawn_backoff_ms(1), 2000);
        assert_eq!(spawn_backoff_ms(3), 8000);
        assert_eq!(spawn_backoff_ms(99), SPAWN_BACKOFF_MAX_MS);
    }

    #[test]
    fn signal_detection_ignores_digital_silence() {
        assert!(!has_signal(&[0.0; 512]));
        assert!(!has_signal(&[5e-5, -5e-5]));
        assert!(has_signal(&[0.0, 0.002, 0.0]));
    }
}
