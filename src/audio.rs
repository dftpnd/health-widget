
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
const WATCH_TICK: std::time::Duration = std::time::Duration::from_secs(2);
const WATCH_AGREE: u32 = 2;
const LEVEL_PERIOD: std::time::Duration = std::time::Duration::from_secs(10);
const SILENCE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

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

#[derive(Debug, PartialEq)]
enum SoundChange {
    Lost(u64),
    Back(u64),
}

fn track_silence(
    signal: bool,
    silent_for: std::time::Duration,
    reported: bool,
) -> Option<SoundChange> {
    match (signal, reported) {
        (true, true) => Some(SoundChange::Back(silent_for.as_secs())),
        (false, false) if silent_for >= SILENCE_AFTER => {
            Some(SoundChange::Lost(silent_for.as_secs()))
        }
        _ => None,
    }
}

struct Level {
    sum_sq: f64,
    peak: f32,
    count: usize,
    since: std::time::Instant,
}

impl Level {
    fn new() -> Self {
        Self { sum_sq: 0.0, peak: 0.0, count: 0, since: std::time::Instant::now() }
    }

    fn add(&mut self, batch: &[f32]) {
        for &v in batch.iter().filter(|v| v.is_finite()) {
            self.sum_sq += (v as f64) * (v as f64);
            self.peak = self.peak.max(v.abs());
            self.count += 1;
        }
    }

    fn due(&self) -> bool {
        if self.count == 0 {
            return false;
        }
        let waited = self.since.elapsed();
        waited >= LEVEL_PERIOD && (self.peak > SIGNAL_FLOOR || waited >= SILENCE_AFTER)
    }

    fn take(&mut self) -> (f64, f32) {
        let rms = (self.sum_sq / self.count.max(1) as f64).sqrt();
        let peak = self.peak;
        self.sum_sq = 0.0;
        self.peak = 0.0;
        self.count = 0;
        self.since = std::time::Instant::now();
        (rms, peak)
    }
}

fn set_notice(slot: &Arc<Mutex<Option<String>>>, text: Option<String>) {
    if let Ok(mut g) = slot.lock() {
        *g = text;
    }
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

#[derive(Clone, PartialEq, Debug)]
enum Listening {
    Device,
    Program { key: String, target: String },
    WaitingFor(String),
    Sink,
}

impl Listening {
    fn notice(&self) -> Option<String> {
        match self {
            Listening::WaitingFor(key) => {
                Some(format!("{key} не звучит — слушаю весь вывод, жду его"))
            }
            _ => None,
        }
    }
}

fn program_target_now(list: &[Device], key: &str) -> Option<String> {
    match_program(list, key).map(|d| d.target.clone())
}

impl Source {
    fn spawn(
        &self,
        channel: &'static str,
    ) -> Option<(Child, std::process::ChildStdout, Listening, Option<String>)> {
        match self {
            Source::Device(t) => spawn_pw_record(t.as_deref(), false, channel)
                .map(|(c, o)| (c, o, Listening::Device, t.clone())),
            Source::Sink => {
                let sink = playing_sink();
                report_sink(channel, sink.as_deref(), "весь вывод");
                spawn_pw_record(sink.as_deref(), true, channel)
                    .map(|(c, o)| (c, o, Listening::Sink, sink))
            }
            Source::Program(key) => match resolve_program(key) {
                Some(target) => spawn_pw_record(Some(&target), false, channel).map(|(c, o)| {
                    let listening =
                        Listening::Program { key: key.clone(), target: target.clone() };
                    (c, o, listening, Some(target))
                }),
                None => {
                    crate::telemetry::event(
                        "audio.program.unresolved",
                        serde_json::json!({ "channel": channel, "key": key }),
                    );
                    let sink = playing_sink();
                    report_sink(channel, sink.as_deref(), "ждём программу");
                    spawn_pw_record(sink.as_deref(), true, channel)
                        .map(|(c, o)| (c, o, Listening::WaitingFor(key.clone()), sink))
                }
            },
        }
    }

    fn spawn_failure_notice(&self) -> &'static str {
        "захват не запускается"
    }

    fn describe(&self) -> (Option<&str>, bool) {
        match self {
            Source::Device(t) => (t.as_deref(), false),
            Source::Program(key) => (Some(key.as_str()), false),
            Source::Sink => (None, true),
        }
    }
}

fn report_sink(channel: &'static str, sink: Option<&str>, why: &str) {
    crate::telemetry::event(
        "audio.sink.resolved",
        serde_json::json!({
            "channel": channel,
            "sink": sink.unwrap_or("по умолчанию"),
            "why": why,
        }),
    );
}

fn watch_target(
    listening: Listening,
    sink: Option<String>,
    channel: &'static str,
    child: Arc<Mutex<Option<Child>>>,
    shutdown: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    mine: u64,
) {
    if matches!(listening, Listening::Device) {
        return;
    }
    std::thread::spawn(move || {
        let mut agreed = 0u32;
        while !shutdown.load(Ordering::Relaxed) && generation.load(Ordering::Relaxed) == mine {
            std::thread::sleep(WATCH_TICK);
            if shutdown.load(Ordering::Relaxed) || generation.load(Ordering::Relaxed) != mine {
                return;
            }
            let objs = dump_objects();
            let reason = match &listening {
                Listening::WaitingFor(key) => match_program(&parse_programs(&objs), key)
                    .map(|d| {
                        (
                            "audio.program.appeared",
                            serde_json::json!({ "key": key, "target": d.target }),
                        )
                    }),
                Listening::Sink => {
                    let now = parse_playing_sink(&objs);
                    (now.is_some() && now != sink).then(|| {
                        (
                            "audio.sink.moved",
                            serde_json::json!({ "from": sink, "to": now }),
                        )
                    })
                }
                Listening::Program { key, target } => {
                    let now = program_target_now(&parse_programs(&objs), key);
                    (now.as_deref() != Some(target.as_str())).then(|| {
                        (
                            "audio.program.moved",
                            serde_json::json!({ "key": key, "from": target, "to": now }),
                        )
                    })
                }
                Listening::Device => None,
            };
            let Some((ev, mut fields)) = reason else {
                agreed = 0;
                continue;
            };
            agreed += 1;
            if agreed < WATCH_AGREE {
                continue;
            }
            if let Some(obj) = fields.as_object_mut() {
                obj.insert("channel".to_string(), serde_json::json!(channel));
            }
            crate::telemetry::event(ev, fields);
            if let Ok(mut g) = child.lock() {
                if let Some(c) = g.as_mut() {
                    let _ = c.kill();
                }
            }
            return;
        }
    });
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

fn base_key(key: &str) -> &str {
    match key.rsplit_once(" #") {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => head,
        _ => key,
    }
}

fn match_program<'a>(list: &'a [Device], key: &str) -> Option<&'a Device> {
    list.iter()
        .find(|d| d.key == key)
        .or_else(|| list.iter().find(|d| base_key(&d.key) == base_key(key)))
}

fn resolve_program(key: &str) -> Option<String> {
    match_program(&list_programs(), key).map(|d| d.target.clone())
}

fn dump_objects() -> Vec<serde_json::Value> {
    let out = match Command::new("pw-dump").output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    serde_json::from_slice(&out).unwrap_or_default()
}

pub fn list_programs() -> Vec<Device> {
    parse_programs(&dump_objects())
}

fn node_props(o: &serde_json::Value) -> Option<&serde_json::Value> {
    if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
        return None;
    }
    o.get("info")?.get("props")
}

fn serial_of(props: &serde_json::Value) -> Option<String> {
    match props.get("object.serial")? {
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn parse_programs(objs: &[serde_json::Value]) -> Vec<Device> {
    let mut res: Vec<Device> = Vec::new();
    for o in objs {
        let Some(props) = node_props(o) else {
            continue;
        };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Stream/Output/Audio") {
            continue;
        }
        let Some(target) = serial_of(props) else {
            continue;
        };
        let app = props
            .get("application.name")
            .and_then(|s| s.as_str())
            .or_else(|| props.get("node.name").and_then(|s| s.as_str()))
            .unwrap_or("?");
        let media = props.get("media.name").and_then(|s| s.as_str()).unwrap_or("");
        let key = if media.is_empty() {
            format!("🔊 {app}")
        } else {
            format!("🔊 {app} — {media}")
        };
        res.push(Device { target, label: key.clone(), key });
    }
    res.sort_by_key(|d| d.target.parse::<u64>().unwrap_or(u64::MAX));
    number_duplicates(&mut res);
    res
}

fn number_duplicates(list: &mut [Device]) {
    let keys: Vec<String> = list.iter().map(|d| d.key.clone()).collect();
    let mut seen: Vec<String> = Vec::new();
    for d in list.iter_mut() {
        if keys.iter().filter(|k| **k == d.key).count() < 2 {
            continue;
        }
        let n = seen.iter().filter(|k| **k == d.key).count() + 1;
        seen.push(d.key.clone());
        d.key = format!("{} #{n}", d.key);
        d.label = d.key.clone();
    }
}

fn sink_names(objs: &[serde_json::Value]) -> Vec<(u64, String)> {
    let mut res = Vec::new();
    for o in objs {
        let Some(props) = node_props(o) else {
            continue;
        };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Audio/Sink") {
            continue;
        }
        let (Some(id), Some(name)) = (
            o.get("id").and_then(|i| i.as_u64()),
            props.get("node.name").and_then(|n| n.as_str()),
        ) else {
            continue;
        };
        res.push((id, name.to_string()));
    }
    res
}

fn playing_stream_ids(objs: &[serde_json::Value]) -> Vec<u64> {
    let mut res = Vec::new();
    for o in objs {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(info) = o.get("info") else { continue };
        let Some(props) = info.get("props") else { continue };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Stream/Output/Audio") {
            continue;
        }
        if info.get("state").and_then(|s| s.as_str()) != Some("running") {
            continue;
        }
        if let Some(id) = o.get("id").and_then(|i| i.as_u64()) {
            res.push(id);
        }
    }
    res
}

fn parse_playing_sink(objs: &[serde_json::Value]) -> Option<String> {
    let streams = playing_stream_ids(objs);
    if streams.is_empty() {
        return None;
    }
    let sinks = sink_names(objs);
    let mut hits: Vec<(String, usize)> = Vec::new();
    for o in objs {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Link") {
            continue;
        }
        let Some(props) = o.get("info").and_then(|i| i.get("props")) else {
            continue;
        };
        let out = props.get("link.output.node").and_then(|v| v.as_u64());
        let inp = props.get("link.input.node").and_then(|v| v.as_u64());
        let (Some(out), Some(inp)) = (out, inp) else {
            continue;
        };
        if !streams.contains(&out) {
            continue;
        }
        let Some((_, name)) = sinks.iter().find(|(id, _)| *id == inp) else {
            continue;
        };
        match hits.iter_mut().find(|(n, _)| n == name) {
            Some((_, c)) => *c += 1,
            None => hits.push((name.clone(), 1)),
        }
    }
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    hits.into_iter().next().map(|(name, _)| name)
}

fn playing_sink() -> Option<String> {
    parse_playing_sink(&dump_objects())
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
        let (first_child, first_spawn) = match source.spawn(channel) {
            Some((child, out, listening, sink)) => (Some(child), Some((out, listening, sink))),
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
        let generation = Arc::new(AtomicU64::new(0));
        let gen = generation.clone();
        std::thread::spawn(move || {
            let source = source;
            let mut fails: u32 = 0;
            let mut acc: Vec<u8> = Vec::with_capacity(8192);
            let mut raw = [0u8; 4096];
            let mut batch: Vec<f32> = Vec::with_capacity(2048);
            let mut pending = first_spawn;
            let mut level = Level::new();
            let mut last_sound = std::time::Instant::now();
            let mut silence_reported = false;
            loop {
                if sd.load(Ordering::Relaxed) {
                    break;
                }
                let (mut stdout, listening, sink) = match pending.take() {
                    Some(s) => s,
                    None => match source.spawn(channel) {
                        Some((child, out, listening, sink)) => {
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
                            (out, listening, sink)
                        }
                        None => {
                            if sd.load(Ordering::Relaxed) {
                                break;
                            }
                            fails += 1;
                            let backoff = spawn_backoff_ms(fails);
                            set_notice(&note, Some(source.spawn_failure_notice().to_string()));
                            std::thread::sleep(std::time::Duration::from_millis(backoff));
                            continue;
                        }
                    },
                };
                let mine = gen.fetch_add(1, Ordering::Relaxed) + 1;
                let standing_notice = listening.notice();
                set_notice(&note, standing_notice.clone());
                let source_name = match (&listening, &sink) {
                    (Listening::Program { target, .. }, _) => format!("поток {target}"),
                    (_, Some(s)) => s.clone(),
                    (_, None) => "по умолчанию".to_string(),
                };
                watch_target(
                    listening,
                    sink,
                    channel,
                    cc.clone(),
                    sd.clone(),
                    gen.clone(),
                    mine,
                );

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
                                set_notice(&note, standing_notice.clone());
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
                            let signal = has_signal(&batch);
                            if signal {
                                if let Ok(mut t) = sig.lock() {
                                    *t = std::time::Instant::now();
                                }
                                last_sound = std::time::Instant::now();
                            }
                            level.add(&batch);
                            if level.due() {
                                let (rms, peak) = level.take();
                                crate::telemetry::event(
                                    "audio.level",
                                    serde_json::json!({
                                        "channel": channel,
                                        "rms": (rms * 10000.0).round() / 10000.0,
                                        "peak": ((peak * 10000.0).round() / 10000.0),
                                        "source": source_name,
                                    }),
                                );
                            }
                            match track_silence(signal, last_sound.elapsed(), silence_reported) {
                                Some(SoundChange::Lost(secs)) => {
                                    silence_reported = true;
                                    crate::telemetry::event(
                                        "audio.silence",
                                        serde_json::json!({
                                            "channel": channel,
                                            "secs": secs,
                                            "source": source_name,
                                        }),
                                    );
                                }
                                Some(SoundChange::Back(secs)) => {
                                    silence_reported = false;
                                    crate::telemetry::event(
                                        "audio.sound.back",
                                        serde_json::json!({ "channel": channel, "after_secs": secs }),
                                    );
                                }
                                None => {}
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
                if !got_first {
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

    fn stream(id: u64, serial: u64, app: &str, media: &str, state: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "type": "PipeWire:Interface:Node",
            "info": {
                "state": state,
                "props": {
                    "media.class": "Stream/Output/Audio",
                    "object.serial": serial,
                    "application.name": app,
                    "media.name": media,
                }
            }
        })
    }

    fn sink(id: u64, name: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "type": "PipeWire:Interface:Node",
            "info": { "state": "idle", "props": { "media.class": "Audio/Sink", "node.name": name } }
        })
    }

    fn link(out: u64, inp: u64) -> serde_json::Value {
        serde_json::json!({
            "type": "PipeWire:Interface:Link",
            "info": { "props": { "link.output.node": out, "link.input.node": inp } }
        })
    }

    #[test]
    fn idle_program_still_listed() {
        let objs = vec![stream(40, 230, "Google Chrome", "Playback", "idle")];
        let got = parse_programs(&objs);
        assert_eq!(got.len(), 1, "молчащая вкладка обязана быть в списке");
        assert_eq!(got[0].key, "🔊 Google Chrome — Playback");
    }

    #[test]
    fn duplicate_programs_get_distinct_keys() {
        let objs = vec![
            stream(41, 231, "Google Chrome", "Playback", "running"),
            stream(40, 230, "Google Chrome", "Playback", "running"),
        ];
        let got = parse_programs(&objs);
        assert_eq!(got[0].target, "230", "порядок стабилен по serial");
        assert_eq!(got[0].key, "🔊 Google Chrome — Playback #1");
        assert_eq!(got[1].key, "🔊 Google Chrome — Playback #2");
        assert_eq!(got[0].label, got[0].key);
        assert_ne!(got[0].key, got[1].key, "две вкладки не должны сливаться в один ключ");
    }

    #[test]
    fn single_program_keeps_plain_key() {
        let objs = vec![stream(40, 230, "Google Chrome", "Playback", "running")];
        assert_eq!(parse_programs(&objs)[0].key, "🔊 Google Chrome — Playback");
    }

    #[test]
    fn playing_sink_is_where_sound_goes_not_default() {
        let objs = vec![
            sink(71, "alsa_output.hdmi-stereo"),
            sink(81, "alsa_output.jabra"),
            stream(40, 230, "Google Chrome", "Playback", "running"),
            link(40, 81),
        ];
        assert_eq!(parse_playing_sink(&objs).as_deref(), Some("alsa_output.jabra"));
    }

    #[test]
    fn idle_stream_does_not_pick_sink() {
        let objs = vec![
            sink(71, "alsa_output.hdmi-stereo"),
            stream(40, 230, "Google Chrome", "Playback", "idle"),
            link(40, 71),
        ];
        assert_eq!(parse_playing_sink(&objs), None, "молчащий поток не выбирает синк");
    }

    #[test]
    fn busiest_sink_wins() {
        let objs = vec![
            sink(71, "alsa_output.hdmi-stereo"),
            sink(81, "alsa_output.jabra"),
            stream(40, 230, "Google Chrome", "Playback", "running"),
            stream(42, 232, "Google Chrome", "Playback", "running"),
            stream(44, 234, "Telegram", "", "running"),
            link(40, 81),
            link(42, 81),
            link(44, 71),
        ];
        assert_eq!(parse_playing_sink(&objs).as_deref(), Some("alsa_output.jabra"));
    }

    #[test]
    fn saved_key_survives_second_tab_appearing() {
        let list = parse_programs(&[
            stream(40, 230, "Google Chrome", "Playback", "running"),
            stream(41, 231, "Google Chrome", "Playback", "running"),
        ]);
        let d = match_program(&list, "🔊 Google Chrome — Playback")
            .expect("сохранённый ключ без номера обязан найтись");
        assert_eq!(d.target, "230");
    }

    #[test]
    fn exact_numbered_key_wins_over_base() {
        let list = parse_programs(&[
            stream(40, 230, "Google Chrome", "Playback", "running"),
            stream(41, 231, "Google Chrome", "Playback", "running"),
        ]);
        let d = match_program(&list, "🔊 Google Chrome — Playback #2").unwrap();
        assert_eq!(d.target, "231");
    }

    #[test]
    fn other_program_never_matches() {
        let list = parse_programs(&[stream(40, 230, "Telegram", "Playback", "running")]);
        assert!(match_program(&list, "🔊 Google Chrome — Playback").is_none());
    }

    #[test]
    fn base_key_strips_only_numeric_suffix() {
        assert_eq!(base_key("🔊 Chrome — Playback #2"), "🔊 Chrome — Playback");
        assert_eq!(base_key("🔊 Chrome — Playback"), "🔊 Chrome — Playback");
        assert_eq!(base_key("🔊 Chrome — Track #1 hits"), "🔊 Chrome — Track #1 hits");
    }

    #[test]
    fn waiting_for_program_is_visible_in_ui() {
        let n = Listening::WaitingFor("🔊 Google Chrome — Playback".to_string()).notice();
        assert!(n.unwrap().contains("Google Chrome"));
        assert_eq!(Listening::Sink.notice(), None);
        assert_eq!(Listening::Device.notice(), None);
    }

    #[test]
    fn reopened_tab_gets_new_serial_and_forces_rebind() {
        let key = "🔊 Google Chrome — Playback";
        let before = parse_programs(&[stream(40, 180, "Google Chrome", "Playback", "running")]);
        assert_eq!(program_target_now(&before, key).as_deref(), Some("180"));

        let after = parse_programs(&[stream(127, 277, "Google Chrome", "Playback", "running")]);
        assert_eq!(
            program_target_now(&after, key).as_deref(),
            Some("277"),
            "перезакрытая вкладка живёт под новым serial"
        );
        assert_ne!(program_target_now(&after, key).as_deref(), Some("180"));
    }

    #[test]
    fn closed_tab_leaves_no_target() {
        let key = "🔊 Google Chrome — Playback";
        assert_eq!(program_target_now(&parse_programs(&[]), key), None);
    }

    #[test]
    fn spawn_backoff_grows_and_caps() {
        assert_eq!(spawn_backoff_ms(0), SPAWN_BACKOFF_MIN_MS);
        assert_eq!(spawn_backoff_ms(1), 2000);
        assert_eq!(spawn_backoff_ms(3), 8000);
        assert_eq!(spawn_backoff_ms(99), SPAWN_BACKOFF_MAX_MS);
    }

    #[test]
    fn silence_reported_once_and_cleared_on_sound() {
        use std::time::Duration;
        assert_eq!(track_silence(false, Duration::from_secs(30), false), None);
        assert_eq!(
            track_silence(false, SILENCE_AFTER, false),
            Some(SoundChange::Lost(60)),
            "минута тишины обязана попасть в телеметрию"
        );
        assert_eq!(
            track_silence(false, Duration::from_secs(600), true),
            None,
            "повторно не спамим"
        );
        assert_eq!(
            track_silence(true, Duration::from_secs(600), true),
            Some(SoundChange::Back(600))
        );
        assert_eq!(track_silence(true, Duration::from_secs(1), false), None);
    }

    #[test]
    fn level_averages_and_keeps_peak() {
        let mut l = Level::new();
        l.add(&[0.5, -0.5, 0.5, -0.5]);
        let (rms, peak) = l.take();
        assert!((rms - 0.5).abs() < 1e-6);
        assert!((peak - 0.5).abs() < 1e-6);
        let (rms, peak) = l.take();
        assert_eq!((rms, peak), (0.0, 0.0), "после отправки счётчики обнуляются");
    }

    #[test]
    fn level_not_due_without_samples() {
        let l = Level::new();
        assert!(!l.due());
    }

    #[test]
    fn silent_level_reports_rarely_loud_often() {
        let mut quiet = Level::new();
        quiet.add(&[0.0; 64]);
        quiet.since = std::time::Instant::now() - LEVEL_PERIOD;
        assert!(!quiet.due(), "тишина не должна писаться каждые 10 с");
        quiet.since = std::time::Instant::now() - SILENCE_AFTER;
        assert!(quiet.due(), "но раз в минуту отметиться обязана");

        let mut loud = Level::new();
        loud.add(&[0.3; 64]);
        loud.since = std::time::Instant::now() - LEVEL_PERIOD;
        assert!(loud.due(), "звук пишем каждые 10 с");
    }

    #[test]
    fn level_ignores_garbage_samples() {
        let mut l = Level::new();
        l.add(&[f32::NAN, 0.5, f32::INFINITY, -0.5]);
        let (rms, peak) = l.take();
        assert!(rms.is_finite(), "мусор из заголовка потока не должен превращать rms в null");
        assert!((rms - 0.5).abs() < 1e-6);
        assert!((peak - 0.5).abs() < 1e-6);
    }

    #[test]
    fn signal_detection_ignores_digital_silence() {
        assert!(!has_signal(&[0.0; 512]));
        assert!(!has_signal(&[5e-5, -5e-5]));
        assert!(has_signal(&[0.0, 0.002, 0.0]));
    }
}
