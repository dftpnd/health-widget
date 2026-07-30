
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::transcript_log::TranscriptLog;

const STT_RATE: f64 = 16000.0;
const MAX_FINALS: usize = 50_000;
const FRESH_CAP: usize = 32;
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);
const RETRY_COLD: Duration = Duration::from_secs(300);
const WARMUP: Duration = Duration::from_secs(25);
const DIED_YOUNG: Duration = Duration::from_secs(20);
const YOUNG_STREAK_MAX: u32 = 3;
const SPEECH_RMS: f32 = 0.015;
const STALL_SPEECH_SECS: f32 = 90.0;
const WEB_RESERVE_MIB: u64 = 5000;
const LINGER: Duration = Duration::from_secs(10);

static START_GATE: Mutex<Option<Instant>> = Mutex::new(None);
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn guard<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn rms(batch: &[f32]) -> f32 {
    if batch.is_empty() {
        return 0.0;
    }
    let sum: f32 = batch.iter().map(|v| v * v).sum();
    (sum / batch.len() as f32).sqrt()
}

struct StartFail {
    why: String,
    retry_in: Option<Duration>,
}

impl StartFail {
    fn hard(why: impl Into<String>) -> Self {
        Self { why: why.into(), retry_in: None }
    }
}

#[derive(Default)]
pub struct Transcript {
    pub finals: String,
    pub partial: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    Off,
    Starting,
    Warmup,
    Live,
    Down(String),
}

impl Health {
    pub fn label(&self) -> Option<String> {
        match self {
            Health::Down(why) => Some(format!("⚠ распознавание не работает: {why}")),
            _ => None,
        }
    }
}

pub struct Transcriber {
    alive: Arc<AtomicBool>,
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
    pub fn is_dead(&self) -> bool {
        self.dead
    }

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
    fn start(
        src_rate: u32,
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
        state: Arc<Mutex<Transcript>>,
        fresh: Arc<Mutex<VecDeque<String>>>,
        out_seq: Arc<AtomicU64>,
    ) -> Result<(Transcriber, Feeder), StartFail> {
        let mut gate = guard(&START_GATE);
        if let Some(last) = *gate {
            let since = last.elapsed();
            if since < WARMUP {
                return Err(StartFail {
                    why: "жду прогрева соседнего канала".to_string(),
                    retry_in: Some(WARMUP - since),
                });
            }
        }
        if let Some(free) = free_vram_mib() {
            let need = need_mib(channel);
            if free < need {
                let why = if need > min_free_mib() {
                    format!("мало VRAM ({free} < {need} МиБ, держу место под веб-канал)")
                } else {
                    format!("мало VRAM ({free} МиБ < {need} МиБ)")
                };
                return Err(StartFail::hard(why));
            }
        }
        let python = python_path().ok_or_else(|| StartFail::hard("нет venv-whisper"))?;
        let model = model_spec();
        let script = ensure_script().ok_or_else(|| StartFail::hard("нет whisper_stream.py"))?;

        let mut child = Command::new(&python)
            .arg(&script)
            .arg(&model)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| StartFail::hard(format!("не запустился: {e}")))?;

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

        let stdin = child.stdin.take().ok_or_else(|| StartFail::hard("нет stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| StartFail::hard("нет stdout"))?;
        let alive = Arc::new(AtomicBool::new(true));

        {
            let state = state.clone();
            let fresh = fresh.clone();
            let alive = alive.clone();
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
                    out_seq.fetch_add(1, Ordering::Relaxed);
                    let mut g = guard(&state);
                    if let Some(p) = v.get("partial").and_then(|p| p.as_str()) {
                        g.partial = p.to_string();
                    } else if let Some(t) = v.get("final").and_then(|t| t.as_str()) {
                        if !g.finals.is_empty() {
                            g.finals.push(' ');
                        }
                        g.finals.push_str(t);
                        trim_head(&mut g.finals, MAX_FINALS);
                        g.partial.clear();
                        {
                            let mut q = guard(&fresh);
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
                alive.store(false, Ordering::Relaxed);
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
        *gate = Some(Instant::now());
        crate::telemetry::event(
            "stt.start",
            serde_json::json!({ "channel": channel, "model": model }),
        );
        Ok((Transcriber { alive, child, channel }, feeder))
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn linger(&mut self) {
        let deadline = Instant::now() + LINGER;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return,
            }
        }
        crate::telemetry::event(
            "stt.linger.timeout",
            serde_json::json!({ "channel": self.channel }),
        );
    }
}

struct Retry {
    next: Instant,
    delay: Duration,
    young_streak: u32,
}

struct Stall {
    seen_seq: u64,
    speech_secs: f32,
}

struct Live {
    transcriber: Option<Transcriber>,
    feeder: Option<Feeder>,
    started: Instant,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.feeder.take();
        if let Some(mut t) = self.transcriber.take() {
            std::thread::spawn(move || t.linger());
        }
    }
}

pub struct Stt {
    state: Arc<Mutex<Transcript>>,
    fresh: Arc<Mutex<VecDeque<String>>>,
    out_seq: Arc<AtomicU64>,
    live: Mutex<Option<Live>>,
    retry: Mutex<Retry>,
    stall: Mutex<Stall>,
    health: Mutex<Health>,
    enabled: bool,
    src_rate: u32,
    channel: &'static str,
    log: Option<Arc<TranscriptLog>>,
}

impl Stt {
    pub fn new(
        src_rate: u32,
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
        want: bool,
    ) -> Arc<Self> {
        let enabled =
            want && is_enabled() && std::env::var("HEALTH_TRANSCRIBE").as_deref() != Ok("0");
        Arc::new(Self {
            state: Arc::new(Mutex::new(Transcript::default())),
            fresh: Arc::new(Mutex::new(VecDeque::new())),
            out_seq: Arc::new(AtomicU64::new(0)),
            live: Mutex::new(None),
            retry: Mutex::new(Retry { next: Instant::now(), delay: RETRY_MIN, young_streak: 0 }),
            stall: Mutex::new(Stall { seen_seq: 0, speech_secs: 0.0 }),
            health: Mutex::new(if enabled { Health::Starting } else { Health::Off }),
            enabled,
            src_rate,
            channel,
            log,
        })
    }

    pub fn feed(&self, batch: &[f32]) {
        if !self.enabled {
            return;
        }
        let mut live = guard(&self.live);
        if live.is_none() {
            self.try_start(&mut live);
        }
        let Some(l) = live.as_mut() else {
            return;
        };
        let (Some(feeder), Some(transcriber)) = (l.feeder.as_mut(), l.transcriber.as_ref()) else {
            return;
        };
        feeder.feed(batch);
        let fault = if feeder.is_dead() || !transcriber.is_alive() {
            Some(("процесс распознавания упал", "died"))
        } else if self.stalled(batch) {
            Some(("нет ответа от распознавания", "stall"))
        } else {
            None
        };
        let Some((why, kind)) = fault else {
            return;
        };
        let young = l.started.elapsed() < DIED_YOUNG;
        drop(live.take());
        self.reset_stall();
        crate::telemetry::event(
            "stt.fault",
            serde_json::json!({ "channel": self.channel, "kind": kind, "young": young }),
        );
        let cold = {
            let mut r = guard(&self.retry);
            r.young_streak = if young { r.young_streak + 1 } else { 0 };
            r.young_streak >= YOUNG_STREAK_MAX
        };
        if cold {
            self.set_health(Health::Down(
                "распознавание падает на старте, пауза 5 мин".to_string(),
            ));
            let mut r = guard(&self.retry);
            r.next = Instant::now() + RETRY_COLD;
            r.delay = RETRY_MIN;
            r.young_streak = 0;
        } else {
            self.set_health(Health::Down(why.to_string()));
            self.schedule_retry();
        }
    }

    fn stalled(&self, batch: &[f32]) -> bool {
        let seq = self.out_seq.load(Ordering::Relaxed);
        let mut s = guard(&self.stall);
        if seq != s.seen_seq {
            s.seen_seq = seq;
            s.speech_secs = 0.0;
            return false;
        }
        if rms(batch) >= SPEECH_RMS {
            s.speech_secs += batch.len() as f32 / self.src_rate as f32;
        }
        s.speech_secs >= STALL_SPEECH_SECS
    }

    fn reset_stall(&self) {
        let mut s = guard(&self.stall);
        s.seen_seq = self.out_seq.load(Ordering::Relaxed);
        s.speech_secs = 0.0;
    }

    fn try_start(&self, live: &mut Option<Live>) {
        if Instant::now() < guard(&self.retry).next {
            return;
        }
        match Transcriber::start(
            self.src_rate,
            self.channel,
            self.log.clone(),
            self.state.clone(),
            self.fresh.clone(),
            self.out_seq.clone(),
        ) {
            Ok((transcriber, feeder)) => {
                *live = Some(Live {
                    transcriber: Some(transcriber),
                    feeder: Some(feeder),
                    started: Instant::now(),
                });
                self.reset_stall();
                let mut r = guard(&self.retry);
                r.delay = RETRY_MIN;
                r.next = Instant::now();
                drop(r);
                self.set_health(Health::Live);
            }
            Err(fail) => {
                match fail.retry_in {
                    Some(d) => {
                        self.set_health(Health::Warmup);
                        guard(&self.retry).next = Instant::now() + d;
                    }
                    None => {
                        self.set_health(Health::Down(fail.why));
                        self.schedule_retry();
                    }
                }
            }
        }
    }

    fn schedule_retry(&self) {
        let mut r = guard(&self.retry);
        r.next = Instant::now() + r.delay;
        r.delay = next_delay(r.delay);
    }

    fn set_health(&self, next: Health) {
        let mut g = guard(&self.health);
        if *g == next {
            return;
        }
        *g = next.clone();
        let (status, why) = match &next {
            Health::Off => ("off", String::new()),
            Health::Starting => ("starting", String::new()),
            Health::Warmup => ("warmup", String::new()),
            Health::Live => ("live", String::new()),
            Health::Down(w) => ("down", w.clone()),
        };
        crate::telemetry::event(
            "stt.health",
            serde_json::json!({ "channel": self.channel, "status": status, "why": why }),
        );
    }

    pub fn health(&self) -> Health {
        guard(&self.health).clone()
    }

    pub fn fresh_handle(&self) -> Arc<Mutex<VecDeque<String>>> {
        self.fresh.clone()
    }

    pub fn state_handle(&self) -> Arc<Mutex<Transcript>> {
        self.state.clone()
    }

    pub fn text(&self) -> (String, String) {
        let g = guard(&self.state);
        (g.finals.clone(), g.partial.clone())
    }

    pub fn clear(&self) {
        let mut g = guard(&self.state);
        g.finals.clear();
        g.partial.clear();
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

fn need_mib(channel: &str) -> u64 {
    let base = min_free_mib();
    if channel == crate::CH_MIC {
        base + WEB_RESERVE_MIB
    } else {
        base
    }
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

fn next_delay(cur: Duration) -> Duration {
    (cur * 2).min(RETRY_MAX)
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
    use super::{next_delay, parse_free_mib, rms, trim_head, Health, Stt, SPEECH_RMS, RETRY_MAX, RETRY_MIN};
    use std::time::Duration;

    #[test]
    fn mic_yields_vram_to_web_channel() {
        let base = super::need_mib(crate::CH_ZOOM);
        assert_eq!(super::need_mib(crate::CH_WEB), base, "веб не уступает");
        assert_eq!(
            super::need_mib(crate::CH_MIC),
            base + super::WEB_RESERVE_MIB,
            "микрофон стартует только если остаётся место под веб"
        );
    }

    #[test]
    fn rms_separates_speech_from_room_noise() {
        assert_eq!(rms(&[]), 0.0);
        let quiet: Vec<f32> = (0..512).map(|i| if i % 2 == 0 { 1e-3 } else { -1e-3 }).collect();
        assert!(rms(&quiet) < SPEECH_RMS, "фоновый шум не считается речью");
        let loud: Vec<f32> = (0..512).map(|i| if i % 2 == 0 { 0.2 } else { -0.2 }).collect();
        assert!(rms(&loud) >= SPEECH_RMS, "речь считается речью");
    }

    #[test]
    fn retry_backs_off_and_caps() {
        let mut d = RETRY_MIN;
        for _ in 0..10 {
            d = next_delay(d);
        }
        assert_eq!(d, RETRY_MAX);
        assert_eq!(next_delay(RETRY_MIN), Duration::from_secs(10));
    }

    #[test]
    fn disabled_stt_never_spawns() {
        let stt = Stt::new(44100, "тест", None, false);
        stt.feed(&[0.1, 0.2, 0.3]);
        assert_eq!(stt.health(), Health::Off);
        assert_eq!(stt.text(), (String::new(), String::new()));
    }

    #[test]
    fn down_health_has_visible_label() {
        assert!(Health::Down("мало VRAM".into()).label().is_some());
        assert_eq!(Health::Live.label(), None);
        assert_eq!(Health::Off.label(), None);
        assert_eq!(Health::Starting.label(), None);
        assert_eq!(Health::Warmup.label(), None);
    }

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
