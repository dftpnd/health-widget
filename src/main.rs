
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod audio;
mod avatar;
mod chat;
mod clip;
mod config;
mod data;
mod deepseek;
mod detect;
mod hr_reply;
mod instance;
mod pilot;
mod pilot_scan;
mod pilot_stats;
mod pilot_notify;
mod kwin_shot;
mod recorder;
mod screenshot;
mod state;
mod tartarus;
mod telemetry;
mod terminal;
mod transcribe;
mod transcript_log;
mod webmic;
mod winctl;

use config::Config;
use data::Metrics;
use transcript_log::TranscriptLog;

const CH_MIC: &str = "🎤 я";
const CH_ZOOM: &str = "🔊 телемост";
const CH_WEB: &str = "🌐 веб";
const WEB_FRESH_GLOW: Duration = Duration::from_millis(2200);

const GRIP: f32 = 16.0;
const MARGIN: f32 = 12.0;
const CHAT_WIN_W: f32 = 440.0;
const CHAT_WIN_H: f32 = 520.0;
const WEB_WIN_W: f32 = 420.0;
const WEB_WIN_H: f32 = 320.0;

const PILOT_PROFILES: &[(&str, &str)] = &[
    ("fullstack", "Fullstack"),
    ("back", "Backend"),
    ("bulat", "Булат"),
];

const PILOT_STRICTNESS: &[(&str, &str, f32)] = &[
    ("strict", "Строго", 0.55),
    ("medium", "Средне", 0.50),
    ("any", "Любые", 0.0),
    ("fresh", "🆕 Свежие", 0.0),
];
const PILOT_FRESH_KEY: &str = "fresh";
const DEFAULT_STRICTNESS: &str = "medium";

const TERMINAL_W: f32 = 340.0;
const TOP_ROW_H: f32 = 64.0;

fn profile_stats_path(autopilot_dir: &std::path::Path, profile: &str) -> std::path::PathBuf {
    autopilot_dir
        .join("data")
        .join(format!("stats-{profile}.json"))
}

struct Shared {
    user_visible: AtomicBool,
    sharing_active: AtomicBool,
    pos: std::sync::Mutex<Option<(i32, i32)>>,
    shutdown: AtomicBool,
}

struct ActiveCall {
    id: i64,
    name: String,
}

struct AudioState {
    mic: Option<audio::AudioMonitor>,
    zoom: Option<audio::AudioMonitor>,
    mic_target: Option<String>,
    prog_target: Option<String>,
    mics: Vec<audio::Device>,
    programs: Vec<audio::Device>,
    scope: Vec<f32>,
}

struct AutopilotState {
    proc: Option<pilot::Pilot>,
    want: Option<pilot::Phase>,
    profile: String,
    strictness: String,
    status: String,
    stats: Option<pilot_stats::PilotStats>,
    stats_mtime: Option<std::time::SystemTime>,
    scan: Option<pilot_scan::ScanStatus>,
    scan_mtime: Option<std::time::SystemTime>,
    notify_on: bool,
}

struct ShotState {
    status: Arc<std::sync::Mutex<screenshot::ShotStatus>>,
    request: Arc<AtomicBool>,
    active: bool,
    points: Vec<[u32; 2]>,
}

struct AvatarState {
    cam: Option<avatar::Avatar>,
    error: Option<String>,
    tex: Option<egui::TextureHandle>,
}

#[derive(Clone)]
struct TranscriptKeys {
    clear_mic: Arc<AtomicBool>,
    clear_zoom: Arc<AtomicBool>,
    copy_mic: Arc<AtomicBool>,
    copy_zoom: Arc<AtomicBool>,
    clear_chat: Arc<AtomicBool>,
}

impl TranscriptKeys {
    fn new() -> Self {
        Self {
            clear_mic: Arc::new(AtomicBool::new(false)),
            clear_zoom: Arc::new(AtomicBool::new(false)),
            copy_mic: Arc::new(AtomicBool::new(false)),
            copy_zoom: Arc::new(AtomicBool::new(false)),
            clear_chat: Arc::new(AtomicBool::new(false)),
        }
    }
}

const WINDOW_MOVE_STEP: i32 = 40;

#[derive(Clone)]
struct WindowMove {
    dx: Arc<AtomicI32>,
    dy: Arc<AtomicI32>,
    busy: Arc<AtomicBool>,
}

impl WindowMove {
    fn new() -> Self {
        Self {
            dx: Arc::new(AtomicI32::new(0)),
            dy: Arc::new(AtomicI32::new(0)),
            busy: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct App {
    cfg: Config,
    shared: Arc<Shared>,
    metrics: Metrics,
    last_mtime: Option<std::time::SystemTime>,
    currently_visible: bool,
    transcript_log: Option<Arc<TranscriptLog>>,
    active_call: Option<ActiveCall>,
    audio: AudioState,
    avatar: AvatarState,
    pinned: bool,
    autopilot: AutopilotState,
    hr_reply: Arc<std::sync::Mutex<hr_reply::HrReplyState>>,
    last_saved: state::State,
    prev_state: state::State,
    stable_since: Instant,
    shot: ShotState,
    cursor_warp_request: Arc<AtomicBool>,
    paste_code: Arc<AtomicBool>,
    prev_cursor: Arc<std::sync::Mutex<Option<(f64, f64)>>>,
    transcript_keys: TranscriptKeys,
    win_move: WindowMove,
    terminal: Option<terminal::Terminal>,
    terminal_open: bool,
    width_one_col: Option<f32>,
    terminal_width: f32,
    autopilot_collapsed: bool,
    metrics_collapsed: bool,
    chat: chat::Chat,
    chat_collapsed: bool,
    deepseek: Option<deepseek::Slot>,
    help_open: bool,
    clipboard_preview: Arc<std::sync::Mutex<String>>,
    clip_open: bool,
    clip_pos: Arc<std::sync::Mutex<Option<(i32, i32)>>>,
    chat_open: bool,
    chat_pos: Arc<std::sync::Mutex<Option<(i32, i32)>>>,
    chat_spawn_size: egui::Vec2,
    chat_size: egui::Vec2,
    webmic: Option<webmic::WebMic>,
    webmic_error: Option<String>,
    web_raised: bool,
    web_pos: Arc<std::sync::Mutex<Option<(i32, i32)>>>,
    web_spawn_size: egui::Vec2,
    web_size: egui::Vec2,
    web_textures: std::collections::HashMap<u64, egui::TextureHandle>,
    web_last_post_id: u64,
    web_fresh_posts: Vec<(u64, Instant)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config, shared: Arc<Shared>, st: state::State) -> Self {
        let (metrics, last_mtime) = data::load(&cfg.json_path);

        let pilot_profile = st
            .pilot_profile
            .clone()
            .unwrap_or_else(|| "fullstack".to_string());
        let pilot_strictness = st
            .pilot_strictness
            .clone()
            .unwrap_or_else(|| DEFAULT_STRICTNESS.to_string());

        let shot_status: Arc<std::sync::Mutex<screenshot::ShotStatus>> =
            Arc::new(std::sync::Mutex::new(screenshot::ShotStatus::Idle));
        let shot_request = Arc::new(AtomicBool::new(false));

        let cursor_warp_request = Arc::new(AtomicBool::new(false));
        let paste_code = Arc::new(AtomicBool::new(false));
        let transcript_keys = TranscriptKeys::new();
        let win_move = WindowMove::new();

        {
            let shared = shared.clone();
            let ctx = cc.egui_ctx.clone();
            let shot_request = shot_request.clone();
            let cursor_warp_request = cursor_warp_request.clone();
            let rt_cursor_warp = libc::SIGRTMIN() + 2;
            std::thread::spawn(move || {
                let mut signals = signal_hook::iterator::Signals::new([
                    signal_hook::consts::SIGUSR1,
                    signal_hook::consts::SIGUSR2,
                    rt_cursor_warp,
                ])
                .expect("cannot register signal handler");
                for sig in signals.forever() {
                    if sig == signal_hook::consts::SIGUSR2 {
                        shot_request.store(true, Ordering::Relaxed);
                        ctx.request_repaint();
                        continue;
                    }
                    if sig == rt_cursor_warp {
                        cursor_warp_request.store(true, Ordering::Relaxed);
                        ctx.request_repaint();
                        continue;
                    }
                    let prev = shared.user_visible.load(Ordering::Relaxed);
                    shared.user_visible.store(!prev, Ordering::Relaxed);
                    ctx.request_repaint();
                }
            });
        }

        tartarus::spawn(tartarus::Handles {
            shot_request: shot_request.clone(),
            cursor_warp_request: cursor_warp_request.clone(),
            clear_mic: transcript_keys.clear_mic.clone(),
            clear_zoom: transcript_keys.clear_zoom.clone(),
            copy_mic: transcript_keys.copy_mic.clone(),
            copy_zoom: transcript_keys.copy_zoom.clone(),
            clear_chat: transcript_keys.clear_chat.clone(),
            paste_code: paste_code.clone(),
            move_dx: win_move.dx.clone(),
            move_dy: win_move.dy.clone(),
            ctx: cc.egui_ctx.clone(),
        });

        {
            let shared = shared.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                let mut signals = signal_hook::iterator::Signals::new([
                    signal_hook::consts::SIGTERM,
                    signal_hook::consts::SIGINT,
                ])
                .expect("cannot register SIGTERM/SIGINT handler");
                if signals.forever().next().is_some() {
                    shared.shutdown.store(true, Ordering::Relaxed);
                    ctx.request_repaint();
                }
            });
        }

        if cfg.auto_hide_on_share && detect::available() {
            let shared = shared.clone();
            let ctx = cc.egui_ctx.clone();
            let poll = cfg.detect_poll;
            std::thread::spawn(move || loop {
                let active = detect::screencast_active();
                let prev = shared.sharing_active.swap(active, Ordering::Relaxed);
                if prev != active {
                    ctx.request_repaint();
                }
                std::thread::sleep(poll);
            });
        }

        let clip_pos = Arc::new(std::sync::Mutex::new(
            st.clip_x.zip(st.clip_y).map(|(x, y)| (x as i32, y as i32)),
        ));
        let chat_pos = Arc::new(std::sync::Mutex::new(
            st.chat_x.zip(st.chat_y).map(|(x, y)| (x as i32, y as i32)),
        ));
        let web_pos = Arc::new(std::sync::Mutex::new(
            st.web_x.zip(st.web_y).map(|(x, y)| (x as i32, y as i32)),
        ));

        {
            let want_pin = st.pinned;
            let pos = st.x.zip(st.y).map(|(x, y)| (x as i32, y as i32));
            let clip = st
                .clip_open
                .then(|| st.clip_x.zip(st.clip_y))
                .flatten()
                .map(|(x, y)| (x as i32, y as i32));
            let chat = st
                .chat_open
                .then(|| st.chat_x.zip(st.chat_y))
                .flatten()
                .map(|(x, y)| (x as i32, y as i32));
            if want_pin || pos.is_some() || clip.is_some() || chat.is_some() {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(800));
                    for _ in 0..2 {
                        if want_pin {
                            winctl::set_keep_above(true);
                        }
                        if let Some((x, y)) = pos {
                            winctl::set_position(x, y);
                        }
                        if let Some((x, y)) = clip {
                            winctl::set_clip_position(x, y);
                        }
                        if let Some((x, y)) = chat {
                            winctl::set_chat_position(x, y);
                        }
                        std::thread::sleep(Duration::from_millis(600));
                    }
                });
            }
        }

        {
            let shared = shared.clone();
            let clip_pos = clip_pos.clone();
            let chat_pos = chat_pos.clone();
            let web_pos = web_pos.clone();
            winctl::follow_geometry(move |ev| match ev {
                winctl::GeomEvent::Main(x, y) => {
                    if let Ok(mut g) = shared.pos.lock() {
                        *g = Some((x, y));
                    }
                }
                winctl::GeomEvent::Clip(x, y) => {
                    if let Ok(mut g) = clip_pos.lock() {
                        *g = Some((x, y));
                    }
                }
                winctl::GeomEvent::Chat(x, y) => {
                    if let Ok(mut g) = chat_pos.lock() {
                        *g = Some((x, y));
                    }
                }
                winctl::GeomEvent::Web(x, y) => {
                    if let Ok(mut g) = web_pos.lock() {
                        *g = Some((x, y));
                    }
                }
            });
        }

        let clipboard_preview = Arc::new(std::sync::Mutex::new(String::new()));
        {
            let clipboard_preview = clipboard_preview.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                let cur = clip::get().unwrap_or_default();
                let cur_len = cur.len();
                let changed = clipboard_preview
                    .lock()
                    .map(|mut g| {
                        if *g == cur {
                            false
                        } else {
                            *g = cur;
                            true
                        }
                    })
                    .unwrap_or(false);
                if changed {
                    eprintln!("clip poll: {} байт", cur_len);
                    ctx.request_repaint();
                }
            });
        }

        if cfg.autopilot_bin.exists() {
            let dir = cfg.autopilot_dir.clone();
            let bin = cfg.autopilot_bin.clone();
            let profile = pilot_profile.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                pilot::refresh_scan_status(&dir, &bin, &profile);
                ctx.request_repaint();
            });
        }

        let transcript_log = TranscriptLog::open().map(Arc::new);

        let mic = if st.mic_on {
            audio::AudioMonitor::start(st.mic_target.as_deref(), CH_MIC, transcript_log.clone())
        } else {
            None
        };
        let zoom = if st.zoom_on {
            audio::AudioMonitor::start_sink_monitor(CH_ZOOM, transcript_log.clone())
        } else {
            None
        };

        let pilot_notify_on = pilot_notify::read_enabled(&cfg.autopilot_dir.join("data"));

        Self {
            cfg,
            shared,
            metrics,
            last_mtime,
            currently_visible: true,
            transcript_log: transcript_log.clone(),
            active_call: None,
            audio: AudioState {
                mic,
                zoom,
                mic_target: st.mic_target.clone(),
                prog_target: None,
                mics: audio::list_mics(),
                programs: audio::list_programs(),
                scope: Vec::with_capacity(2048),
            },
            avatar: AvatarState { cam: None, error: None, tex: None },
            pinned: st.pinned,
            autopilot: AutopilotState {
                proc: None,
                want: None,
                profile: pilot_profile,
                strictness: pilot_strictness,
                status: String::new(),
                stats: None,
                stats_mtime: None,
                scan: None,
                scan_mtime: None,
                notify_on: pilot_notify_on,
            },
            hr_reply: Arc::new(std::sync::Mutex::new(hr_reply::HrReplyState::Idle)),
            last_saved: st.clone(),
            prev_state: st.clone(),
            stable_since: Instant::now(),
            shot: ShotState {
                status: shot_status,
                request: shot_request,
                active: false,
                points: Vec::new(),
            },
            cursor_warp_request,
            paste_code,
            prev_cursor: Arc::new(std::sync::Mutex::new(None)),
            transcript_keys,
            win_move,
            terminal: None,
            terminal_open: st.terminal_open,
            width_one_col: None,
            terminal_width: st.terminal_width.unwrap_or(TERMINAL_W),
            autopilot_collapsed: st.autopilot_collapsed,
            metrics_collapsed: st.metrics_collapsed,
            chat: chat::Chat::default(),
            chat_collapsed: st.chat_collapsed,
            deepseek: None,
            help_open: false,
            clipboard_preview,
            clip_open: st.clip_open,
            clip_pos,
            chat_open: st.chat_open,
            chat_pos,
            chat_spawn_size: egui::vec2(
                st.chat_w.unwrap_or(CHAT_WIN_W),
                st.chat_h.unwrap_or(CHAT_WIN_H),
            ),
            chat_size: egui::vec2(
                st.chat_w.unwrap_or(CHAT_WIN_W),
                st.chat_h.unwrap_or(CHAT_WIN_H),
            ),
            webmic: (std::env::var("HEALTH_WEBMIC").as_deref() == Ok("1"))
                .then(|| webmic::WebMic::start(CH_WEB, transcript_log.clone()).ok())
                .flatten(),
            webmic_error: None,
            web_raised: false,
            web_pos,
            web_spawn_size: egui::vec2(st.web_w.unwrap_or(WEB_WIN_W), st.web_h.unwrap_or(WEB_WIN_H)),
            web_size: egui::vec2(st.web_w.unwrap_or(WEB_WIN_W), st.web_h.unwrap_or(WEB_WIN_H)),
            web_textures: std::collections::HashMap::new(),
            web_last_post_id: 0,
            web_fresh_posts: Vec::new(),
        }
    }

    fn toggle_avatar(&mut self) {
        if let Some(cam) = self.avatar.cam.take() {
            cam.stop();
            self.avatar.error = None;
            return;
        }
        let samples = match &self.audio.mic {
            Some(m) => m.samples_handle(),
            None => Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
        };
        let mut phrases = Vec::new();
        if let Some(q) = self.audio.mic.as_ref().and_then(|m| m.fresh_finals()) {
            phrases.push(q);
        }
        if let Some(q) = self.audio.zoom.as_ref().and_then(|z| z.fresh_finals()) {
            phrases.push(q);
        }
        match avatar::Avatar::start(&self.cfg.avatar, samples, phrases) {
            Ok(cam) => {
                self.avatar.cam = Some(cam);
                self.avatar.error = None;
            }
            Err(e) => self.avatar.error = Some(e.to_string()),
        }
    }

    fn start_program(&self) -> Option<audio::AudioMonitor> {
        match self.audio.prog_target.as_deref() {
            Some(t) => audio::AudioMonitor::start(Some(t), CH_ZOOM, self.transcript_log.clone()),
            None => audio::AudioMonitor::start_sink_monitor(CH_ZOOM, self.transcript_log.clone()),
        }
    }

    fn start_call(&mut self) {
        let Some(log) = self.transcript_log.clone() else {
            return;
        };
        let name = call_name_now();
        let Some(id) = log.start_call(&name) else {
            return;
        };
        telemetry::event("call.start", serde_json::json!({ "id": id, "name": name }));
        self.active_call = Some(ActiveCall { id, name });
        self.reconcile_call_recording();
    }

    fn end_call(&mut self) {
        let Some(call) = self.active_call.take() else {
            return;
        };
        telemetry::event("call.end", serde_json::json!({ "id": call.id, "name": call.name }));
        if let Some(mon) = &self.audio.mic {
            mon.stop_recording();
        }
        if let Some(mon) = &self.audio.zoom {
            mon.stop_recording();
        }
        if let Some(log) = &self.transcript_log {
            log.end_call(call.id);
        }
    }

    fn reconcile_call_recording(&self) {
        let (Some(call), Some(log)) = (&self.active_call, &self.transcript_log) else {
            return;
        };
        let Some(dir) = transcript_log::call_dir(call.id) else {
            return;
        };
        for (mon, channel, file) in [
            (&self.audio.mic, CH_MIC, "mic.wav"),
            (&self.audio.zoom, CH_ZOOM, "zoom.wav"),
        ] {
            if let Some(m) = mon {
                if !m.is_recording() {
                    let path = dir.join(file);
                    if m.start_recording(&path).is_ok() {
                        log.add_track(call.id, channel, &path.to_string_lossy());
                    }
                }
            }
        }
    }

    fn pilot_min_sim(&self) -> f32 {
        PILOT_STRICTNESS
            .iter()
            .find(|(k, _, _)| *k == self.autopilot.strictness)
            .map(|(_, _, v)| *v)
            .unwrap_or(0.0)
    }

    fn pilot_apply_fresh(&self) -> bool {
        self.autopilot.strictness == PILOT_FRESH_KEY
    }

    fn reconcile_pilot(&mut self) {
        let desired = match self.autopilot.want.clone() {
            None => {
                if self.autopilot.proc.is_some() {
                    telemetry::event("pilot.stop", serde_json::json!({}));
                }
                self.autopilot.proc = None;
                return;
            }
            Some(p) => p,
        };
        let phase = format!("{desired:?}");
        let same_phase = self.autopilot.proc.as_ref().map(|p| p.phase()) == Some(&desired);
        let same_profile =
            self.autopilot.proc.as_ref().map(|p| p.profile()) == Some(Some(self.autopilot.profile.as_str()));
        let same_sim = self.autopilot.proc.as_ref().map(|p| p.min_sim()) == Some(self.pilot_min_sim());
        let same_order =
            self.autopilot.proc.as_ref().map(|p| p.apply_fresh()) == Some(self.pilot_apply_fresh());
        if same_phase && same_profile && same_sim && same_order {
            return;
        }
        self.autopilot.proc = None;
        self.autopilot.proc = pilot::Pilot::start(
            &self.cfg.autopilot_dir,
            &self.cfg.autopilot_bin,
            desired,
            Some(self.autopilot.profile.as_str()),
            Some(self.pilot_min_sim()),
            self.pilot_apply_fresh(),
        );
        if self.autopilot.proc.is_none() {
            self.autopilot.want = None;
            self.autopilot.status = "не удалось запустить автопилот".to_string();
            telemetry::error("pilot.fail", "не удалось запустить автопилот");
        } else {
            self.autopilot.status.clear();
            telemetry::event(
                "pilot.spawn",
                serde_json::json!({ "phase": phase, "profile": self.autopilot.profile }),
            );
        }
    }

    fn current_state(&self, ctx: &egui::Context) -> state::State {
        let size = ctx.screen_rect().size();
        let win_w = if self.terminal_open {
            (size.x - self.terminal_width).max(200.0)
        } else {
            size.x
        };
        let (x, y) = match self.shared.pos.lock().ok().and_then(|g| *g) {
            Some((px, py)) => (Some(px as f32), Some(py as f32)),
            None => (self.last_saved.x, self.last_saved.y),
        };
        let (clip_x, clip_y) = match self.clip_pos.lock().ok().and_then(|g| *g) {
            Some((px, py)) => (Some(px as f32), Some(py as f32)),
            None => (self.last_saved.clip_x, self.last_saved.clip_y),
        };
        let (chat_x, chat_y) = match self.chat_pos.lock().ok().and_then(|g| *g) {
            Some((px, py)) => (Some(px as f32), Some(py as f32)),
            None => (self.last_saved.chat_x, self.last_saved.chat_y),
        };
        let (web_x, web_y) = match self.web_pos.lock().ok().and_then(|g| *g) {
            Some((px, py)) => (Some(px as f32), Some(py as f32)),
            None => (self.last_saved.web_x, self.last_saved.web_y),
        };
        state::State {
            x,
            y,
            width: Some(win_w),
            height: Some(size.y),
            mic_on: self.audio.mic.is_some(),
            mic_target: self.audio.mic_target.clone(),
            zoom_on: self.audio.zoom.is_some(),
            pinned: self.pinned,
            pilot_profile: Some(self.autopilot.profile.clone()),
            pilot_strictness: Some(self.autopilot.strictness.clone()),
            terminal_width: Some(self.terminal_width),
            autopilot_collapsed: self.autopilot_collapsed,
            metrics_collapsed: self.metrics_collapsed,
            chat_collapsed: self.chat_collapsed,
            terminal_open: self.terminal_open,
            clip_open: self.clip_open,
            clip_x,
            clip_y,
            chat_open: self.chat_open,
            chat_x,
            chat_y,
            chat_w: Some(self.chat_size.x),
            chat_h: Some(self.chat_size.y),
            web_x,
            web_y,
            web_w: Some(self.web_size.x),
            web_h: Some(self.web_size.y),
        }
    }

    fn maybe_reload(&mut self) {
        if let Ok(meta) = std::fs::metadata(&self.cfg.json_path) {
            if let Ok(mtime) = meta.modified() {
                if self.last_mtime != Some(mtime) {
                    let (metrics, mt) = data::load(&self.cfg.json_path);
                    self.metrics = metrics;
                    self.last_mtime = mt;
                }
            }
        }
        let stats_path = profile_stats_path(&self.cfg.autopilot_dir, &self.autopilot.profile);
        let mtime = std::fs::metadata(&stats_path).and_then(|m| m.modified()).ok();
        if mtime != self.autopilot.stats_mtime {
            self.autopilot.stats = pilot_stats::load(&stats_path);
            self.autopilot.stats_mtime = mtime;
        }
        let scan_path = self.cfg.autopilot_dir.join("data").join("scan.json");
        let scan_mtime = std::fs::metadata(&scan_path).and_then(|m| m.modified()).ok();
        if scan_mtime != self.autopilot.scan_mtime {
            self.autopilot.scan = pilot_scan::load(&scan_path);
            self.autopilot.scan_mtime = scan_mtime;
        }
        self.autopilot.notify_on =
            pilot_notify::read_enabled(&self.cfg.autopilot_dir.join("data"));
    }

    fn show_shot_overlay(&mut self, ctx: &egui::Context) {
        let vb = egui::ViewportBuilder::default()
            .with_title("health-widget-shot")
            .with_fullscreen(true)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_mouse_passthrough(false);
        let id = egui::ViewportId::from_hash_of("hw-shot-overlay");

        let mut done: Option<Option<[u32; 4]>> = None;

        ctx.show_viewport_immediate(id, vb, |octx, _class| {
            if octx.input(|i| i.key_pressed(egui::Key::Escape)) {
                done = Some(None);
            }
            let click = octx.input(|i| {
                i.pointer
                    .primary_clicked()
                    .then(|| i.pointer.interact_pos())
                    .flatten()
            });

            egui::CentralPanel::default()
                .frame(
                    egui::Frame::default()
                        .inner_margin(egui::Margin::same(0))
                        .fill(egui::Color32::TRANSPARENT),
                )
                .show(octx, |ui| {
                    ui.allocate_response(ui.available_size(), egui::Sense::click());
                });

            if done.is_none() {
                if let Some(pos) = click {
                    let px = [pos.x.round().max(0.0) as u32, pos.y.round().max(0.0) as u32];
                    self.shot.points.push(px);
                    if self.shot.points.len() >= 2 {
                        let (a, b) = (self.shot.points[0], self.shot.points[1]);
                        done = Some(Some([
                            a[0].min(b[0]),
                            a[1].min(b[1]),
                            a[0].abs_diff(b[0]),
                            a[1].abs_diff(b[1]),
                        ]));
                    }
                }
            }
            octx.request_repaint();
        });

        if let Some(res) = done {
            self.shot.active = false;
            self.shot.points.clear();
            ctx.request_repaint();
            match res {
                Some([x, y, w, h]) => {
                    *self.shot.status.lock().unwrap() = screenshot::ShotStatus::Working;
                    screenshot::grab(x as i32, y as i32, w, h, ctx.clone(), self.shot.status.clone());
                }
                None => *self.shot.status.lock().unwrap() = screenshot::ShotStatus::Cancelled,
            }
        }
    }
    fn draw_header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let title = self.metrics.title.clone();
        let pinned = self.pinned;
        let mut toggle_pin = false;
        let mut toggle_terminal = false;
        let mut toggle_webmic = false;
        let mut do_restart = false;
        ui.horizontal(|ui| {
            if let Some(t) = &title {
                ui.label(
                    egui::RichText::new(t)
                        .size(15.0)
                        .strong()
                        .color(egui::Color32::from_rgb(180, 200, 255)),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let hint = if pinned {
                    "Закреплено поверх всех — открепить"
                } else {
                    "Закрепить поверх всех окон"
                };
                if ui.selectable_label(pinned, "📌").on_hover_text(hint).clicked() {
                    toggle_pin = true;
                }
                if ui
                    .selectable_label(self.help_open, "❓")
                    .on_hover_text("Бинды клавиатуры")
                    .clicked()
                {
                    self.help_open = !self.help_open;
                }
                if ui
                    .selectable_label(self.terminal_open, "🖥")
                    .on_hover_text("Терминал")
                    .clicked()
                {
                    toggle_terminal = true;
                }
                if ui
                    .selectable_label(self.webmic.is_some(), "🌐")
                    .on_hover_text(
                        "Веб-микрофон: https-страница на порту 8787.\n\
                         При включении ссылка с токеном копируется в буфер.",
                    )
                    .clicked()
                {
                    toggle_webmic = true;
                }
                if let Some(e) = &self.webmic_error {
                    ui.label(
                        egui::RichText::new(format!("✖ {e}"))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(230, 120, 120)),
                    );
                }
                if ui
                    .button("⟳")
                    .on_hover_text("Пересобрать (--release) и перезапустить")
                    .clicked()
                {
                    do_restart = true;
                }
                if ui
                    .button("⌨")
                    .on_hover_text(
                        "Напечатать код из буфера в позиции курсора (бинд 10).\n\
                         Для набора в другом окне жми клавишу 10 на кейпаде, \
                         а не эту кнопку — клик забирает фокус на виджет.",
                    )
                    .clicked()
                {
                    type_clipboard_code();
                }
                ui.label(
                    egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(90, 96, 108)),
                )
                .on_hover_text(format!(
                    "health-widget v{}\ncommit {}\nсборка {}",
                    env!("CARGO_PKG_VERSION"),
                    env!("GIT_HASH"),
                    env!("BUILD_TIME"),
                ));
            });
        });
        if toggle_pin {
            self.pinned = !self.pinned;
            winctl::set_keep_above(self.pinned);
        }
        if toggle_terminal {
            self.terminal_open = !self.terminal_open;
            let cur = ctx.screen_rect();
            if self.terminal_open {
                self.width_one_col = Some(cur.width());
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    cur.width() + self.terminal_width,
                    cur.height(),
                )));
            } else {
                let target = self
                    .width_one_col
                    .take()
                    .unwrap_or((cur.width() - self.terminal_width).max(200.0));
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    target,
                    cur.height(),
                )));
            }
        }
        if toggle_webmic {
            self.toggle_webmic();
        }
        if do_restart {
            rebuild_and_restart();
        }
        if self.help_open {
            self.draw_keys_help(ctx);
        }
        ui.add_space(2.0);
    }

    fn draw_keys_help(&mut self, ctx: &egui::Context) {
        let binds: &[(&str, &str)] = &[
            ("02", "🧹 Очистить транскрипт микрофона"),
            ("03", "📤 Копировать микрофон + отправить в чат"),
            ("05", "📷 Скриншот"),
            ("07", "🧹 Очистить транскрипт зума"),
            ("08", "📤 Копировать зум + отправить в чат"),
            ("10", "⌨ Печатать код из буфера в позиции курсора"),
            ("12", "🗑 Очистить чат"),
            ("20", "🎯 Курсор в центр виджета"),
            ("D-pad", "🕹 Двигать виджет по экрану"),
        ];
        let accent = egui::Color32::from_rgb(180, 200, 255);
        let modal = egui::Modal::new(egui::Id::new("keys_help")).show(ctx, |ui| {
            ui.set_max_width(340.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("⌨ Бинды клавиатуры").size(15.0).strong().color(accent));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").clicked() {
                        self.help_open = false;
                    }
                });
            });
            ui.add_space(6.0);
            egui::Grid::new("keys_help_grid")
                .num_columns(2)
                .spacing([12.0, 6.0])
                .striped(true)
                .show(ui, |ui| {
                    for (key, action) in binds {
                        ui.label(egui::RichText::new(*key).monospace().strong().color(accent));
                        ui.label(egui::RichText::new(*action).size(12.0));
                        ui.end_row();
                    }
                });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Остальные клавиши шлют F13–F24 / Ctrl+F13–F21 как глобальные хоткеи.")
                    .size(10.0)
                    .italics()
                    .color(egui::Color32::from_rgb(120, 126, 138)),
            );
        });
        if modal.should_close() {
            self.help_open = false;
        }
    }

    fn draw_sound(&mut self, ui: &mut egui::Ui, min_height: f32) {
        let mic_on = self.audio.mic.is_some();
        let zoom_on = self.audio.zoom.is_some();
        let mic_target = self.audio.mic_target.clone();
        let prog_target = self.audio.prog_target.clone();
        let mut toggle_mic = false;
        let mut toggle_zoom = false;
        let mut mic_off = false;
        let mut zoom_off = false;
        let mut new_mic: Option<Option<String>> = None;
        let mut new_prog: Option<Option<String>> = None;
        let mut refresh = false;
        let mut refresh_mic = false;

        section_sized(ui, "🎧 Звук и транскрипция", min_height, |ui| {
        ui.horizontal(|ui| {
            if ui
                .selectable_label(mic_on, "🎤")
                .on_hover_text("Слушать микрофон")
                .clicked()
            {
                toggle_mic = true;
            }
            let cur = if mic_on {
                device_label(&mic_target, &self.audio.mics, "🎤 по умолчанию")
            } else {
                "⊘ выключено".to_string()
            };
            egui::ComboBox::from_id_salt("mic-src")
                .width(150.0)
                .selected_text(egui::RichText::new(cur).size(11.0))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(!mic_on, "⊘ выключено").clicked() {
                        mic_off = true;
                    }
                    if ui.selectable_label(mic_on && mic_target.is_none(), "🎤 по умолчанию").clicked() {
                        new_mic = Some(None);
                    }
                    for d in &self.audio.mics {
                        let sel = mic_on && mic_target.as_deref() == Some(d.target.as_str());
                        if ui.selectable_label(sel, &d.label).clicked() {
                            new_mic = Some(Some(d.target.clone()));
                        }
                    }
                });
            if ui.small_button("⟳").on_hover_text("обновить список микрофонов").clicked() {
                refresh_mic = true;
            }
        });

        ui.horizontal(|ui| {
            if ui
                .selectable_label(zoom_on, "🔊")
                .on_hover_text("Слушать звук программы или всего вывода")
                .clicked()
            {
                toggle_zoom = true;
            }
            let cur = if zoom_on {
                device_label(&prog_target, &self.audio.programs, "🔊 весь вывод")
            } else {
                "⊘ выключено".to_string()
            };
            egui::ComboBox::from_id_salt("prog-src")
                .width(150.0)
                .selected_text(egui::RichText::new(cur).size(11.0))
                .show_ui(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                        if ui.selectable_label(!zoom_on, "⊘ выключено").clicked() {
                            zoom_off = true;
                        }
                        if ui.selectable_label(zoom_on && prog_target.is_none(), "🔊 весь вывод").clicked() {
                            new_prog = Some(None);
                        }
                        for d in &self.audio.programs {
                            let sel = zoom_on && prog_target.as_deref() == Some(d.target.as_str());
                            if ui.selectable_label(sel, &d.label).clicked() {
                                new_prog = Some(Some(d.target.clone()));
                            }
                        }
                    });
                });
            if ui.small_button("⟳").on_hover_text("обновить список программ").clicked() {
                refresh = true;
            }
        });
        });

        if refresh {
            self.audio.mics = audio::list_mics();
            self.audio.programs = audio::list_programs();
        }
        if refresh_mic {
            self.audio.mics = audio::list_mics();
        }
        if let Some(sel) = new_mic {
            self.audio.mic_target = sel;
            self.audio.mic = audio::AudioMonitor::start(self.audio.mic_target.as_deref(), CH_MIC, self.transcript_log.clone());
        }
        if let Some(sel) = new_prog {
            self.audio.prog_target = sel;
            self.audio.zoom = self.start_program();
        }
        if mic_off {
            self.audio.mic = None;
        }
        if zoom_off {
            self.audio.zoom = None;
        }
        if toggle_mic {
            self.audio.mic = if self.audio.mic.is_some() {
                None
            } else {
                audio::AudioMonitor::start(self.audio.mic_target.as_deref(), CH_MIC, self.transcript_log.clone())
            };
        }
        if toggle_zoom {
            self.audio.zoom = if self.audio.zoom.is_some() {
                None
            } else {
                self.start_program()
            };
        }
        ui.add_space(2.0);
    }

    fn draw_avatar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        section(ui, "🐼 Виртуальная камера", |ui| {
            let on = self.avatar.cam.as_ref().is_some_and(|c| c.is_running());
            let label = if on { "🐼 Камера: в эфире" } else { "🐼 Камера" };
            if ui.button(label).clicked() {
                self.toggle_avatar();
            }
            if let Some(err) = &self.avatar.error {
                ui.colored_label(egui::Color32::RED, err);
            }
            let frame = self.avatar.cam.as_ref().and_then(|cam| cam.last_frame());
            if let Some((w, h, rgba)) = frame {
                let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                match &mut self.avatar.tex {
                    Some(tex) => tex.set(img, egui::TextureOptions::LINEAR),
                    None => {
                        self.avatar.tex =
                            Some(ctx.load_texture("avatar_preview", img, egui::TextureOptions::LINEAR));
                    }
                }
            }
            if on {
                if let Some(tex) = &self.avatar.tex {
                    ui.image((tex.id(), egui::vec2(160.0, 120.0)));
                }
                ctx.request_repaint_after(Duration::from_millis(66));
            }
        });
    }

    fn toggle_clip_window(&mut self) {
        self.clip_open = !self.clip_open;
        if !self.clip_open {
            return;
        }
        let pos = self
            .clip_pos
            .lock()
            .ok()
            .and_then(|g| *g)
            .or_else(|| {
                self.last_saved
                    .clip_x
                    .zip(self.last_saved.clip_y)
                    .map(|(x, y)| (x as i32, y as i32))
            });
        if let Some((x, y)) = pos {
            std::thread::spawn(move || {
                for _ in 0..2 {
                    std::thread::sleep(Duration::from_millis(400));
                    winctl::set_clip_position(x, y);
                }
            });
        }
    }

    fn draw_clipboard(&mut self, ui: &mut egui::Ui) {
        section(ui, "📋 Буфер", |ui| {
            let label = if self.clip_open {
                "📋 Скрыть панель"
            } else {
                "📋 Показать панель"
            };
            if ui
                .button(label)
                .on_hover_text("Плавающая панель с содержимым буфера обмена")
                .clicked()
            {
                self.toggle_clip_window();
            }
            self.clip_preview_ui(ui);
        });
    }

    fn clip_text(&self) -> Option<String> {
        let text = self
            .clipboard_preview
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let binary = text.contains('\u{FFFD}')
            || text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'));
        (!text.is_empty() && !binary).then_some(text)
    }

    fn clip_counters(text: &str) -> String {
        let chars = text.chars().count();
        let lines = text.lines().count().max(1);
        format!("{chars} симв · {lines} стр")
    }

    fn clip_preview_ui(&self, ui: &mut egui::Ui) {
        let Some(text) = self.clip_text() else {
            self.clip_empty_ui(ui);
            return;
        };
        let preview: String = text
            .trim()
            .chars()
            .take(160)
            .map(|c| if c == '\n' { '⏎' } else { c })
            .collect();
        ui.add(
            egui::Label::new(egui::RichText::new(preview).size(11.0).monospace())
                .truncate(),
        );
        ui.weak(Self::clip_counters(&text));
    }

    fn clip_full_ui(&self, ui: &mut egui::Ui) {
        let Some(text) = self.clip_text() else {
            self.clip_empty_ui(ui);
            return;
        };
        let shown: String = text.chars().take(4000).collect();
        let clipped = shown.len() < text.len();
        egui::ScrollArea::vertical()
            .max_height(380.0)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(shown).size(11.0).monospace(),
                    )
                    .wrap(),
                );
            });
        let mut counters = Self::clip_counters(&text);
        if clipped {
            counters.push_str(" · показаны первые 4000");
        }
        ui.weak(counters);
    }

    fn clip_empty_ui(&self, ui: &mut egui::Ui) {
        let text = self
            .clipboard_preview
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        if text.is_empty() {
            ui.weak("пусто");
        } else {
            ui.weak(format!("не текст ({} байт)", text.len()));
        }
    }

    fn show_clip_window(&mut self, ctx: &egui::Context) {
        let vb = egui::ViewportBuilder::default()
            .with_title(winctl::CLIP_CAPTION)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_inner_size([380.0, 72.0]);
        let id = egui::ViewportId::from_hash_of("hw-clip-panel");
        let bg = egui::Color32::from_rgba_unmultiplied(18, 18, 22, self.cfg.bg_alpha);
        let mut close = false;

        ctx.show_viewport_immediate(id, vb, |cctx, _class| {
            if cctx.input(|i| i.viewport().close_requested()) {
                close = true;
            }
            let frame = egui::Frame::default()
                .fill(bg)
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(58, 63, 78)))
                .inner_margin(egui::Margin::same(8))
                .corner_radius(10);
            let inner = egui::CentralPanel::default().frame(frame).show(cctx, |ui| {
                let drag = ui.interact(
                    ui.max_rect(),
                    ui.id().with("clip-drag"),
                    egui::Sense::click_and_drag(),
                );
                if drag.drag_started() {
                    cctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("📋 Буфер")
                            .size(10.5)
                            .strong()
                            .color(egui::Color32::from_rgb(120, 130, 150)),
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.small_button("✕").clicked() {
                                close = true;
                            }
                        },
                    );
                });
                self.clip_full_ui(ui);
                ui.min_rect().size()
            });

            let content = inner.inner + egui::vec2(16.0, 16.0);
            let cur = cctx.screen_rect().size();
            let target = egui::vec2(380.0, content.y.clamp(64.0, 460.0));
            if (target.y - cur.y).abs() > 0.5 || (target.x - cur.x).abs() > 0.5 {
                cctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target));
            }
        });

        if close {
            self.clip_open = false;
        }
    }

    fn toggle_chat_window(&mut self) {
        self.chat_open = !self.chat_open;
        if !self.chat_open {
            return;
        }
        self.chat_spawn_size = self.chat_size;
        let pos = self
            .chat_pos
            .lock()
            .ok()
            .and_then(|g| *g)
            .or_else(|| {
                self.last_saved
                    .chat_x
                    .zip(self.last_saved.chat_y)
                    .map(|(x, y)| (x as i32, y as i32))
            });
        if let Some((x, y)) = pos {
            std::thread::spawn(move || {
                for _ in 0..2 {
                    std::thread::sleep(Duration::from_millis(400));
                    winctl::set_chat_position(x, y);
                }
            });
        }
    }

    fn show_chat_window(&mut self, ctx: &egui::Context) {
        let vb = egui::ViewportBuilder::default()
            .with_title(winctl::CHAT_CAPTION)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(true)
            .with_inner_size([self.chat_spawn_size.x, self.chat_spawn_size.y])
            .with_min_inner_size([300.0, 240.0]);
        let id = egui::ViewportId::from_hash_of("hw-chat-panel");
        let bg = egui::Color32::from_rgba_unmultiplied(18, 18, 22, self.cfg.bg_alpha);
        let mut close = false;
        let mut submitted = None;

        ctx.show_viewport_immediate(id, vb, |cctx, _class| {
            if cctx.input(|i| i.viewport().close_requested()) {
                close = true;
            }
            let frame = egui::Frame::default()
                .fill(bg)
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(58, 63, 78)))
                .inner_margin(egui::Margin::same(8))
                .corner_radius(10);
            egui::CentralPanel::default().frame(frame).show(cctx, |ui| {
                let panel = ui.max_rect();
                let grip_rect =
                    egui::Rect::from_min_max(panel.max - egui::vec2(GRIP, GRIP), panel.max);
                let drag = ui.interact(
                    panel,
                    ui.id().with("chat-drag"),
                    egui::Sense::click_and_drag(),
                );
                if drag.drag_started()
                    && !drag
                        .interact_pointer_pos()
                        .is_some_and(|p| grip_rect.contains(p))
                {
                    cctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("💬 Чат")
                            .size(10.5)
                            .strong()
                            .color(egui::Color32::from_rgb(120, 130, 150)),
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui.small_button("✕").clicked() {
                                close = true;
                            }
                        },
                    );
                });
                let log_height = (ui.available_height() - 56.0).max(80.0);
                submitted = self.chat.ui_capped(ui, log_height);
                draw_resize_grip(ui, cctx, grip_rect);
            });
            self.chat_size = cctx.screen_rect().size();
        });

        if close {
            self.chat_open = false;
        }
        if let Some(q) = submitted {
            self.start_deepseek(ctx.clone(), q);
        }
    }

    fn toggle_webmic(&mut self) {
        self.webmic_error = None;
        self.web_last_post_id = 0;
        self.web_fresh_posts.clear();
        if self.webmic.take().is_some() {
            self.web_raised = false;
            return;
        }
        match webmic::WebMic::start(CH_WEB, self.transcript_log.clone()) {
            Ok(w) => {
                self.web_spawn_size = self.web_size;
                clip::set_async(w.hint());
                self.webmic = Some(w);
                let pos = self.web_pos.lock().ok().and_then(|g| *g).or_else(|| {
                    self.last_saved
                        .web_x
                        .zip(self.last_saved.web_y)
                        .map(|(x, y)| (x as i32, y as i32))
                });
                if let Some((x, y)) = pos {
                    std::thread::spawn(move || {
                        for _ in 0..2 {
                            std::thread::sleep(Duration::from_millis(400));
                            winctl::set_web_position(x, y);
                        }
                    });
                }
            }
            Err(e) => self.webmic_error = Some(e),
        }
    }

    fn show_web_window(&mut self, ctx: &egui::Context) {
        let Some(shared) = self.webmic.as_ref().map(|w| w.shared()) else {
            self.web_raised = false;
            return;
        };
        if !self.web_raised {
            self.web_raised = true;
            std::thread::spawn(|| {
                for _ in 0..2 {
                    std::thread::sleep(Duration::from_millis(400));
                    winctl::set_web_keep_above();
                }
            });
        }
        enum Snap {
            Text(u64, String),
            Image(u64),
        }
        impl Snap {
            fn id(&self) -> u64 {
                match self {
                    Snap::Text(id, _) | Snap::Image(id) => *id,
                }
            }
        }
        let (lines, partial, stt_on, active, posts, new_imgs) = match shared.lock() {
            Ok(mut g) => {
                g.zoom = self.audio.zoom.as_ref().and_then(|m| m.transcript_handle());
                let mut posts = Vec::new();
                let mut new_imgs = Vec::new();
                for p in &g.posts {
                    match p {
                        webmic::Post::Text(id, t) => posts.push(Snap::Text(*id, t.clone())),
                        webmic::Post::Image(id, px) => {
                            if !self.web_textures.contains_key(id) {
                                new_imgs.push((*id, px.clone()));
                            }
                            posts.push(Snap::Image(*id));
                        }
                    }
                }
                (
                    g.lines.iter().cloned().collect::<Vec<_>>(),
                    g.partial.clone(),
                    g.stt_on,
                    g.client_active(),
                    posts,
                    new_imgs,
                )
            }
            Err(_) => return,
        };
        for (id, px) in new_imgs {
            let tex = ctx.load_texture(format!("hwweb-{id}"), px, Default::default());
            self.web_textures.insert(id, tex);
        }
        self.web_textures
            .retain(|id, _| posts.iter().any(|p| matches!(p, Snap::Image(pid) if pid == id)));
        let now = Instant::now();
        for p in &posts {
            if p.id() > self.web_last_post_id {
                self.web_fresh_posts.push((p.id(), now));
            }
        }
        if let Some(max_id) = posts.iter().map(Snap::id).max() {
            self.web_last_post_id = self.web_last_post_id.max(max_id);
        }
        self.web_fresh_posts
            .retain(|(_, t)| t.elapsed() < WEB_FRESH_GLOW);
        let vb = egui::ViewportBuilder::default()
            .with_title(winctl::WEB_CAPTION)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(true)
            .with_inner_size([self.web_spawn_size.x, self.web_spawn_size.y])
            .with_min_inner_size([320.0, 280.0]);
        let id = egui::ViewportId::from_hash_of("hw-web-panel");
        let bg = egui::Color32::from_rgba_unmultiplied(18, 18, 22, self.cfg.bg_alpha);
        let mut close = false;

        ctx.show_viewport_immediate(id, vb, |cctx, _class| {
            let tick = if self.web_fresh_posts.is_empty() {
                Duration::from_millis(250)
            } else {
                Duration::from_millis(30)
            };
            cctx.request_repaint_after(tick);
            if cctx.input(|i| i.viewport().close_requested()) {
                close = true;
            }
            let frame = egui::Frame::default()
                .fill(bg)
                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(58, 63, 78)))
                .inner_margin(egui::Margin::same(8))
                .corner_radius(10);
            egui::CentralPanel::default().frame(frame).show(cctx, |ui| {
                let panel = ui.max_rect();
                let grip_rect =
                    egui::Rect::from_min_max(panel.max - egui::vec2(GRIP, GRIP), panel.max);
                let drag = ui.interact(
                    panel,
                    ui.id().with("web-drag"),
                    egui::Sense::click_and_drag(),
                );
                if drag.drag_started()
                    && !drag
                        .interact_pointer_pos()
                        .is_some_and(|p| grip_rect.contains(p))
                {
                    cctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("🌐 Веб-микрофон")
                            .size(10.5)
                            .strong()
                            .color(egui::Color32::from_rgb(120, 130, 150)),
                    );
                    let (dot, status) = if active {
                        ("🟢", "клиент говорит")
                    } else if stt_on {
                        ("🟡", "тишина")
                    } else {
                        ("⚪", "жду клиента")
                    };
                    ui.label(
                        egui::RichText::new(format!("{dot} {status}"))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(140, 146, 158)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            close = true;
                        }
                    });
                });
                let half = (ui.available_height() - 40.0).max(80.0) / 2.0;
                ui.label(
                    egui::RichText::new("🗣 Речь")
                        .size(10.0)
                        .strong()
                        .color(egui::Color32::from_rgb(120, 130, 150)),
                );
                egui::ScrollArea::vertical()
                    .id_salt("web-speech")
                    .max_height(half)
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if lines.is_empty() && partial.is_empty() {
                            ui.label(
                                egui::RichText::new("говори на странице — текст появится тут")
                                    .size(12.0)
                                    .italics()
                                    .color(egui::Color32::from_rgb(90, 96, 108)),
                            );
                        }
                        for l in &lines {
                            ui.label(
                                egui::RichText::new(l)
                                    .size(16.0)
                                    .color(egui::Color32::from_rgb(205, 210, 220)),
                            );
                        }
                        if !partial.is_empty() {
                            ui.label(
                                egui::RichText::new(&partial)
                                    .size(16.0)
                                    .italics()
                                    .color(egui::Color32::from_rgb(130, 136, 148)),
                            );
                        }
                    });
                ui.separator();
                ui.label(
                    egui::RichText::new("📥 Присланное")
                        .size(10.0)
                        .strong()
                        .color(egui::Color32::from_rgb(120, 130, 150)),
                );
                egui::ScrollArea::vertical()
                    .id_salt("web-posts")
                    .max_height(ui.available_height() - GRIP)
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if posts.is_empty() {
                            ui.label(
                                egui::RichText::new("пришли текст или картинку со страницы")
                                    .size(12.0)
                                    .italics()
                                    .color(egui::Color32::from_rgb(90, 96, 108)),
                            );
                        }
                        for p in &posts {
                            let glow = self
                                .web_fresh_posts
                                .iter()
                                .find(|(id, _)| *id == p.id())
                                .map(|(_, t)| {
                                    1.0 - t.elapsed().as_secs_f32()
                                        / WEB_FRESH_GLOW.as_secs_f32()
                                })
                                .filter(|k| *k > 0.0);
                            let frame = match glow {
                                Some(k) => egui::Frame::default()
                                    .stroke(egui::Stroke::new(
                                        1.5,
                                        egui::Color32::from_rgba_unmultiplied(
                                            110,
                                            170,
                                            255,
                                            (k * 230.0) as u8,
                                        ),
                                    ))
                                    .corner_radius(6)
                                    .inner_margin(egui::Margin::same(4)),
                                None => egui::Frame::default(),
                            };
                            frame.show(ui, |ui| match p {
                                Snap::Text(_, t) => {
                                    ui.label(
                                        egui::RichText::new(t)
                                            .size(15.0)
                                            .color(egui::Color32::from_rgb(205, 210, 220)),
                                    );
                                }
                                Snap::Image(id) => {
                                    if let Some(tex) = self.web_textures.get(id) {
                                        let size = tex.size_vec2();
                                        let k = (ui.available_width() / size.x).min(1.0);
                                        ui.image((tex.id(), size * k));
                                    }
                                }
                            });
                        }
                    });
                draw_resize_grip(ui, cctx, grip_rect);
            });
            self.web_size = cctx.screen_rect().size();
        });

        if close {
            self.webmic = None;
            self.web_raised = false;
        }
    }

    fn draw_call(&mut self, ui: &mut egui::Ui, min_height: f32) {
        self.reconcile_call_recording();
        let mut call_toggle = false;
        let active_name = self.active_call.as_ref().map(|c| c.name.clone());
        let zoom_silent_min = self
            .active_call
            .as_ref()
            .and(self.audio.zoom.as_ref())
            .map(|z| z.silent_for())
            .filter(|d| *d >= Duration::from_secs(120))
            .map(|d| d.as_secs() / 60);
        section_sized(ui, "🎙 Кол", min_height, |ui| {
            ui.horizontal(|ui| {
                let recording = active_name.is_some();
                let (label, hint) = if recording {
                    ("⏹ Завершить", "Остановить запись и сохранить кол")
                } else {
                    ("🔴 Кол", "Начать запись звонка: звук обоих каналов + текст")
                };
                if ui.button(label).on_hover_text(hint).clicked() {
                    call_toggle = true;
                }
                if let Some(n) = &active_name {
                    ui.label(
                        egui::RichText::new(format!("● {n}"))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(230, 120, 120)),
                    );
                }
            });
            if let Some(m) = zoom_silent_min {
                ui.label(
                    egui::RichText::new(format!("⚠ телемост молчит {m} мин"))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(230, 120, 120)),
                );
            }
        });
        if call_toggle {
            if self.active_call.is_some() {
                self.end_call();
            } else {
                self.start_call();
            }
        }
    }

    fn draw_screen(&mut self, ui: &mut egui::Ui, min_height: f32) {
        let mut shoot = false;
        let shot_line = {
            use screenshot::ShotStatus::*;
            match &*self.shot.status.lock().unwrap() {
                Idle => None,
                Marking => Some((
                    "⧗ кликни две точки…".to_string(),
                    egui::Color32::from_rgb(210, 200, 120),
                )),
                Working => Some((
                    "⧗ режу…".to_string(),
                    egui::Color32::from_rgb(210, 200, 120),
                )),
                Saved(p) => {
                    let name = std::path::Path::new(p)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("сохранено");
                    Some((
                        format!("✔ {name}"),
                        egui::Color32::from_rgb(120, 200, 120),
                    ))
                }
                Cancelled => Some(("отменено".to_string(), egui::Color32::GRAY)),
                Failed(e) => {
                    Some((format!("✖ {e}"), egui::Color32::from_rgb(230, 120, 120)))
                }
            }
        };
        section_sized(ui, "📸 Скрин", min_height, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.shot.active,
                        egui::Button::new("📸 Область"),
                    )
                    .on_hover_text(
                        "Кликнуть две точки на экране — сохранить PNG области \
                         в ~/.local/share/health-widget/screenshots/",
                    )
                    .clicked()
                {
                    shoot = true;
                }
                if let Some((text, color)) = &shot_line {
                    ui.label(egui::RichText::new(text).size(11.0).color(*color));
                }
            });
        });
        if shoot {
            self.shot.request.store(true, Ordering::Relaxed);
        }
    }

    fn draw_autopilot(&mut self, ui: &mut egui::Ui) {
        if self.cfg.autopilot_bin.exists() {
            use pilot::Phase;
            let mut new_want: Option<Option<Phase>> = None;
            let mut new_profile: Option<String> = None;
            let mut new_strictness: Option<String> = None;
            let mut toggle_pause = false;
            let running = self.autopilot.want.is_some();
            let paused = self.autopilot.proc.as_ref().is_some_and(|p| p.is_paused());
            let status = if paused {
                "⏸ на паузе".to_string()
            } else {
                self.autopilot.proc
                    .as_ref()
                    .and_then(|p| p.last_line())
                    .unwrap_or_else(|| self.autopilot.status.clone())
            };
            let mut ap_collapsed = self.autopilot_collapsed;
            section_collapsible(ui, "🤖 Автопилот", &mut ap_collapsed, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("👤").size(13.0)).on_hover_text(
                        "Профиль автопилота: аккаунт браузера, резюме и контакты",
                    );
                    let cur = PILOT_PROFILES
                        .iter()
                        .find(|(k, _)| *k == self.autopilot.profile)
                        .map(|(_, l)| *l)
                        .unwrap_or("Fullstack");
                    egui::ComboBox::from_id_salt("pilot-profile")
                        .width(150.0)
                        .selected_text(egui::RichText::new(cur).size(11.0))
                        .show_ui(ui, |ui| {
                            for (key, label) in PILOT_PROFILES {
                                if ui
                                    .selectable_label(self.autopilot.profile == *key, *label)
                                    .clicked()
                                    && self.autopilot.profile != *key
                                {
                                    new_profile = Some((*key).to_string());
                                }
                            }
                        });
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.autopilot.want == Some(Phase::Chat), "💬 Чат")
                        .on_hover_text("Автопилот: вести чаты с работодателями")
                        .clicked()
                    {
                        new_want = Some(if self.autopilot.want == Some(Phase::Chat) {
                            None
                        } else {
                            Some(Phase::Chat)
                        });
                    }
                    if ui
                        .selectable_label(self.autopilot.want == Some(Phase::Apply), "📨 Отклики")
                        .on_hover_text("Автопилот: разбирать очередь скана — откликаться")
                        .clicked()
                    {
                        new_want = Some(if self.autopilot.want == Some(Phase::Apply) {
                            None
                        } else {
                            Some(Phase::Apply)
                        });
                    }
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .add_enabled(running, egui::Button::new("⏻ Выключить"))
                                .on_hover_text("Остановить автопилот и закрыть браузер")
                                .clicked()
                            {
                                new_want = Some(None);
                            }
                            let (label, hint) = if paused {
                                ("▶ Продолжить", "Снять паузу — продолжить с того же места")
                            } else {
                                ("⏸ Пауза", "Заморозить автопилот на месте (браузер не закрывается)")
                            };
                            if ui
                                .add_enabled(self.autopilot.proc.is_some(), egui::Button::new(label))
                                .on_hover_text(hint)
                                .clicked()
                            {
                                toggle_pause = true;
                            }
                        },
                    );
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let on = self.autopilot.notify_on;
                    let label = if on {
                        "🔔 Уведомления: вкл"
                    } else {
                        "🔕 Уведомления: выкл"
                    };
                    if ui
                        .selectable_label(on, label)
                        .on_hover_text(
                            "TG-пинги о собеседовании/контактах/позитиве в чате \
                             (общий тумблер на все профили)",
                        )
                        .clicked()
                    {
                        let data_dir = self.cfg.autopilot_dir.join("data");
                        if let Err(e) = pilot_notify::set_enabled(&data_dir, !on) {
                            eprintln!("notify.json write failed: {e}");
                        } else {
                            self.autopilot.notify_on = !on;
                        }
                    }
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let running = matches!(
                        &*self.hr_reply.lock().unwrap(),
                        hr_reply::HrReplyState::Running
                    );
                    ui.add_enabled_ui(!running, |ui| {
                        ui.menu_button("✍️ Ответить HR", |ui| {
                            for (key, label) in PILOT_PROFILES {
                                if ui.button(*label).clicked() {
                                    hr_reply::start(
                                        self.hr_reply.clone(),
                                        ui.ctx().clone(),
                                        self.cfg.autopilot_dir.clone(),
                                        self.cfg.autopilot_bin.clone(),
                                        (*key).to_string(),
                                    );
                                    ui.close_menu();
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "Черновик ответа рекрутёру: берёт текст из буфера, \
                             отвечает через LLM от лица выбранного профиля и кладёт \
                             ответ обратно в буфер",
                        );
                    });
                    match &*self.hr_reply.lock().unwrap() {
                        hr_reply::HrReplyState::Running => {
                            ui.add(egui::Spinner::new().size(14.0));
                            ui.label(egui::RichText::new("думаю…").size(11.0));
                        }
                        hr_reply::HrReplyState::Done => {
                            ui.label(
                                egui::RichText::new("✓ ответ в буфере")
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(120, 210, 150)),
                            );
                        }
                        hr_reply::HrReplyState::Error(e) => {
                            ui.label(
                                egui::RichText::new(e.clone())
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(230, 120, 120)),
                            );
                        }
                        _ => {}
                    }
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🎯 Отклик на:").size(11.0))
                        .on_hover_text(
                            "Порог соответствия вакансии твоему резюме (косинус \
                             эмбеддингов). Ниже порога — не откликаемся.",
                        );
                    for (key, label, thr) in PILOT_STRICTNESS {
                        let active = self.autopilot.strictness == *key;
                        let hint = if *key == PILOT_FRESH_KEY {
                            "Без порога — самые свежие вакансии пула по дате публикации"
                                .to_string()
                        } else if *thr > 0.0 {
                            format!("Порог ≥ {thr:.2} — только достаточно близкие вакансии")
                        } else {
                            "Без порога — откликаться на весь пул (по убыванию похожести)"
                                .to_string()
                        };
                        if ui
                            .selectable_label(active, *label)
                            .on_hover_text(hint)
                            .clicked()
                            && !active
                        {
                            new_strictness = Some((*key).to_string());
                        }
                    }
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let applying = self.autopilot.want == Some(Phase::Apply);
                            let (lbl, hint) = if applying {
                                ("⏹ Стоп", "Остановить отклики")
                            } else {
                                (
                                    "▶ Начать",
                                    "Начать отклики по соответствию резюме \
                                     (с выбранной строгостью)",
                                )
                            };
                            if ui.button(lbl).on_hover_text(hint).clicked() {
                                new_want =
                                    Some(if applying { None } else { Some(Phase::Apply) });
                            }
                        },
                    );
                });
                if let Some(scan) = &self.autopilot.scan {
                    if !scan.groups.is_empty() {
                        ui.add_space(2.0);
                        let total: i64 = scan.groups.iter().map(|g| g.pending).sum();
                        if ui
                            .selectable_label(
                                self.autopilot.want == Some(Phase::ScanAll),
                                format!("🔎 Все группы ({total})"),
                            )
                            .on_hover_text(
                                "Сканировать все группы подряд в очередь, \
                                 затем само запустится дообогащение \
                                 (повторно — стоп)",
                            )
                            .clicked()
                        {
                            new_want = Some(if self.autopilot.want == Some(Phase::ScanAll) {
                                None
                            } else {
                                Some(Phase::ScanAll)
                            });
                        }
                        ui.horizontal_wrapped(|ui| {
                            for g in &scan.groups {
                                let active =
                                    self.autopilot.want == Some(Phase::Scan(g.name.clone()));
                                let label = format!("🔎 {} ({})", g.name, g.pending);
                                if ui
                                    .selectable_label(active, label)
                                    .on_hover_text(
                                        "Скан группы в очередь откликов, \
                                         затем само запустится дообогащение \
                                         (повторно — стоп)",
                                    )
                                    .clicked()
                                {
                                    new_want = Some(if active {
                                        None
                                    } else {
                                        Some(Phase::Scan(g.name.clone()))
                                    });
                                }
                            }
                        });
                    }
                }
                if let Some(scan) = &self.autopilot.scan {
                    let enrich_active = self.autopilot.want == Some(Phase::Enrich);
                    if enrich_active || scan.unenriched > 0 {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(
                                    enrich_active,
                                    format!("✨ Дообогатить ({})", scan.unenriched),
                                )
                                .on_hover_text(
                                    "Открыть необогащённые вакансии пула и сохранить \
                                     полное описание, дату публикации и вектор \
                                     (точный подбор под резюме). Повторно — стоп; \
                                     по завершении гаснет сама.",
                                )
                                .clicked()
                            {
                                new_want = Some(if enrich_active {
                                    None
                                } else {
                                    Some(Phase::Enrich)
                                });
                            }
                            if enrich_active {
                                ui.add(egui::Spinner::new().size(14.0));
                            }
                        });
                    }
                }
                if !status.is_empty() {
                    let mut job = egui::text::LayoutJob::default();
                    job.wrap.max_width = ui.available_width();
                    job.append(
                        &status,
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::proportional(10.0),
                            color: egui::Color32::from_rgb(120, 128, 140),
                            ..Default::default()
                        },
                    );
                    ui.label(job);
                }
                if let Some(s) = &self.autopilot.stats {
                    let color = egui::Color32::from_rgb(150, 160, 175);
                    ui.label(
                        egui::RichText::new(format!(
                            "📨 откликов: {} (сегодня {})",
                            s.applied_total, s.applied_today
                        ))
                        .size(11.0)
                        .color(color),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "💬 чатов обработано: {}",
                            s.chats_acted
                        ))
                        .size(11.0)
                        .color(color),
                    );
                }
            });
            self.autopilot_collapsed = ap_collapsed;
            if let Some(p) = new_profile {
                self.autopilot.profile = p;
                self.autopilot.scan_mtime = None;
                self.autopilot.stats = None;
                self.autopilot.stats_mtime = None;
                if self.autopilot.want.is_some() {
                    self.reconcile_pilot();
                }
            }
            if let Some(s) = new_strictness {
                self.autopilot.strictness = s;
                if self.autopilot.want == Some(Phase::Apply) {
                    self.reconcile_pilot();
                }
            }
            if let Some(w) = new_want {
                self.autopilot.want = w;
                self.reconcile_pilot();
            }
            if toggle_pause {
                if let Some(p) = self.autopilot.proc.as_mut() {
                    if p.is_paused() {
                        p.resume();
                    } else {
                        p.pause();
                    }
                }
            }
        }
    }

    fn process_transcript_keys(&mut self, ctx: &egui::Context) {
        if self.transcript_keys.clear_mic.swap(false, Ordering::Relaxed) {
            if let Some(mon) = &self.audio.mic {
                mon.clear_transcript();
            }
        }
        if self.transcript_keys.clear_zoom.swap(false, Ordering::Relaxed) {
            if let Some(mon) = &self.audio.zoom {
                mon.clear_transcript();
            }
        }
        if self.transcript_keys.copy_mic.swap(false, Ordering::Relaxed) {
            let txt = self.audio.mic.as_ref().and_then(|m| m.transcript()).map(|(f, _)| f);
            if let Some(txt) = txt {
                if !txt.is_empty() {
                    telemetry::event("keys.copy", serde_json::json!({ "channel": CH_MIC, "len": txt.len() }));
                    clip::set_async(txt.clone());
                    self.start_deepseek(ctx.clone(), txt);
                }
            }
        }
        if self.transcript_keys.copy_zoom.swap(false, Ordering::Relaxed) {
            let txt = self.audio.zoom.as_ref().and_then(|m| m.transcript()).map(|(f, _)| f);
            if let Some(txt) = txt {
                if !txt.is_empty() {
                    telemetry::event("keys.copy", serde_json::json!({ "channel": CH_ZOOM, "len": txt.len() }));
                    clip::set_async(txt.clone());
                    self.start_deepseek(ctx.clone(), txt);
                }
            }
        }
        if self.transcript_keys.clear_chat.swap(false, Ordering::Relaxed) {
            self.chat.clear();
            self.deepseek = None;
        }
    }

    fn process_window_move(&mut self, ctx: &egui::Context) {
        let dx = self.win_move.dx.swap(0, Ordering::Relaxed);
        let dy = self.win_move.dy.swap(0, Ordering::Relaxed);
        if dx == 0 && dy == 0 {
            return;
        }
        if self.win_move.busy.swap(true, Ordering::Relaxed) {
            self.win_move.dx.fetch_add(dx, Ordering::Relaxed);
            self.win_move.dy.fetch_add(dy, Ordering::Relaxed);
            return;
        }
        let busy = self.win_move.busy.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            winctl::move_by(dx * WINDOW_MOVE_STEP, dy * WINDOW_MOVE_STEP);
            busy.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
    }

    fn draw_scopes(&mut self, ui: &mut egui::Ui) {
        if self.audio.mic.is_some() || self.audio.zoom.is_some() {
            let mut picked: Option<String> = None;
            let mut clear_mic = false;
            let mut clear_zoom = false;
            section(ui, "📈 Осциллограммы", |ui| {
                if let Some(mon) = &self.audio.mic {
                    mon.snapshot(&mut self.audio.scope);
                    let color = egui::Color32::from_rgb(120, 210, 150);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("🎤 Микрофон").size(11.0).color(color),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("🧹").on_hover_text("Очистить текст").clicked() {
                                clear_mic = true;
                            }
                        });
                    });
                    draw_scope(ui, &self.audio.scope, color);
                    picked = draw_transcript(ui, mon.transcript(), color, "mic")
                        .or(picked.take());
                }
                if let Some(mon) = &self.audio.zoom {
                    mon.snapshot(&mut self.audio.scope);
                    let color = egui::Color32::from_rgb(130, 180, 250);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("🔊 Zoom/Телемост").size(11.0).color(color),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("🧹").on_hover_text("Очистить текст").clicked() {
                                clear_zoom = true;
                            }
                        });
                    });
                    draw_scope(ui, &self.audio.scope, color);
                    picked = draw_transcript(ui, mon.transcript(), color, "zoom")
                        .or(picked.take());
                }
            });
            if clear_mic {
                if let Some(mon) = &self.audio.mic {
                    mon.clear_transcript();
                }
            }
            if clear_zoom {
                if let Some(mon) = &self.audio.zoom {
                    mon.clear_transcript();
                }
            }
            if let Some(q) = picked {
                self.start_deepseek(ui.ctx().clone(), q);
            }
        }
    }

    fn start_deepseek(&mut self, ctx: egui::Context, question: String) {
        let q = question.trim().to_string();
        if q.is_empty() || self.deepseek.is_some() {
            return;
        }
        telemetry::event(
            "chat.ask",
            serde_json::json!({ "len": q.len(), "profile": self.autopilot.profile }),
        );
        self.chat.push_user(q.clone());
        self.chat.set_pending(true);
        self.chat_collapsed = false;
        self.deepseek = Some(deepseek::ask(
            ctx,
            self.cfg.autopilot_dir.clone(),
            self.autopilot.profile.clone(),
            q,
        ));
    }

    fn poll_deepseek(&mut self) {
        let done = self
            .deepseek
            .as_ref()
            .and_then(|s| s.lock().ok().and_then(|mut g| g.take()));
        if let Some(res) = done {
            self.deepseek = None;
            self.chat.set_pending(false);
            match res {
                Ok(answer) => self.chat.push_bot(answer),
                Err(e) => self.chat.push_bot(format!("⚠ {e}")),
            }
        }
    }

    fn draw_chat(&mut self, ui: &mut egui::Ui) {
        let chat_open = self.chat_open;
        let inner = section_collapsible(ui, "💬 Чат", &mut self.chat_collapsed, |ui| {
            let label = if chat_open {
                "💬 Прикрепить"
            } else {
                "💬 Открепить"
            };
            let toggle = ui
                .button(label)
                .on_hover_text("Плавающее окно чата, независимое от виджета")
                .clicked();
            let submitted = if chat_open {
                ui.weak("чат в отдельном окне");
                None
            } else {
                self.chat.ui(ui)
            };
            (toggle, submitted)
        });
        if let Some((toggle, submitted)) = inner {
            if toggle {
                self.toggle_chat_window();
            }
            if let Some(q) = submitted {
                self.start_deepseek(ui.ctx().clone(), q);
            }
        }
    }

}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.shared.shutdown.load(Ordering::Relaxed) {
            telemetry::event("app.shutdown", serde_json::json!({}));
            self.end_call();
            self.autopilot.want = None;
            self.reconcile_pilot();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        self.maybe_reload();

        self.poll_deepseek();

        if self.shot.request.swap(false, Ordering::Relaxed) && !self.shot.active {
            telemetry::event("shot.request", serde_json::json!({}));
            self.shot.active = true;
            self.shot.points.clear();
            *self.shot.status.lock().unwrap() = screenshot::ShotStatus::Marking;
        }
        if self.shot.active {
            self.show_shot_overlay(ctx);
        }

        if self.clip_open {
            self.show_clip_window(ctx);
        }

        if self.chat_open {
            self.show_chat_window(ctx);
        }

        if self.webmic.is_some() {
            self.show_web_window(ctx);
        }

        if self.cursor_warp_request.swap(false, Ordering::Relaxed) {
            telemetry::event("hotkey.cursor_warp", serde_json::json!({}));
            let cslot = self.prev_cursor.clone();
            std::thread::spawn(move || {
                let mut slot = cslot.lock().unwrap();
                if let Some((nx, ny)) = slot.take() {
                    winctl::warp_cursor_norm(nx, ny);
                } else {
                    *slot = winctl::cursor_pos_norm();
                    if let Some((nx, ny)) = winctl::widget_center_norm() {
                        winctl::warp_cursor_norm(nx, ny);
                    }
                }
            });
        }

        if self.paste_code.swap(false, Ordering::Relaxed) {
            type_clipboard_code();
        }

        if self.autopilot.proc.as_mut().is_some_and(|p| !p.alive()) {
            let finished = self.autopilot.proc.as_ref().map(|p| p.phase().clone());
            self.autopilot.proc = None;
            if matches!(
                finished,
                Some(pilot::Phase::Scan(_)) | Some(pilot::Phase::ScanAll)
            ) {
                telemetry::event(
                    "pilot.exit",
                    serde_json::json!({ "reason": "скан завершён", "chain": "enrich" }),
                );
                self.autopilot.want = Some(pilot::Phase::Enrich);
                self.reconcile_pilot();
                if self.autopilot.proc.is_some() {
                    self.autopilot.status = "скан завершён — дообогащаю пул".to_string();
                }
            } else {
                self.autopilot.want = None;
                let msg = match finished {
                    Some(pilot::Phase::Enrich) => "обогащение завершено",
                    _ => "автопилот остановлен",
                };
                self.autopilot.status = msg.to_string();
                telemetry::event("pilot.exit", serde_json::json!({ "reason": msg }));
            }
        }

        let want_visible = self.shared.user_visible.load(Ordering::Relaxed)
            && !(self.cfg.auto_hide_on_share && self.shared.sharing_active.load(Ordering::Relaxed));

        if want_visible != self.currently_visible {
            telemetry::event("vis.change", serde_json::json!({ "visible": want_visible }));
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(want_visible));
            self.currently_visible = want_visible;
        }

        if want_visible {
            let bg = egui::Color32::from_rgba_unmultiplied(18, 18, 22, self.cfg.bg_alpha);
            let frame = egui::Frame::default()
                .fill(bg)
                .inner_margin(egui::Margin::same(MARGIN as i8))
                .corner_radius(10);

            if self.terminal_open {
                let resp = egui::SidePanel::right("terminal_panel")
                    .resizable(true)
                    .default_width(self.terminal_width)
                    .frame(frame)
                    .show(ctx, |ui| {
                        let term = self
                            .terminal
                            .get_or_insert_with(|| terminal::Terminal::new(ctx));
                        term.ui(ui);
                    });
                self.terminal_width = resp.response.rect.width();
            }

            let inner = egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                let panel = ui.max_rect();
                let grip_rect =
                    egui::Rect::from_min_max(panel.max - egui::vec2(GRIP, GRIP), panel.max);

                let drag =
                    ui.interact(panel, ui.id().with("drag-move"), egui::Sense::click_and_drag());
                if drag.drag_started()
                    && !drag
                        .interact_pointer_pos()
                        .is_some_and(|p| grip_rect.contains(p))
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                ui.spacing_mut().item_spacing.y = 6.0;

                self.draw_header(ui, ctx);

                ui.columns(3, |cols| {
                    self.draw_sound(&mut cols[0], TOP_ROW_H);
                    self.draw_call(&mut cols[1], TOP_ROW_H);
                    self.draw_screen(&mut cols[2], TOP_ROW_H);
                });
                ui.add_space(2.0);

                ui.columns(2, |cols| {
                    self.draw_avatar(&mut cols[0], ctx);
                    self.draw_clipboard(&mut cols[1]);
                });

                self.draw_autopilot(ui);

                self.process_transcript_keys(ctx);

                self.process_window_move(ctx);

                self.draw_scopes(ui);

                self.draw_chat(ui);

                draw_resize_grip(ui, ctx, grip_rect);

                ui.min_rect().size()
            });

            let content = inner.inner + egui::vec2(2.0 * MARGIN, 2.0 * MARGIN);
            let cur = ctx.screen_rect().size();
            let target = egui::vec2(content.x.max(cur.x), content.y.max(cur.y));
            if (target.x - cur.x).abs() > 0.5 || (target.y - cur.y).abs() > 0.5 {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target));
            }
        }

        let now = Instant::now();
        let cur = self.current_state(ctx);
        if cur != self.prev_state {
            self.prev_state = cur.clone();
            self.stable_since = now;
        }
        if cur != self.last_saved && now.duration_since(self.stable_since) > Duration::from_millis(700) {
            state::save(&cur);
            self.last_saved = cur;
        }

        let scope_active = self.audio.mic.is_some() || self.audio.zoom.is_some();
        let interval = if want_visible && scope_active { 33 } else { 500 };
        ctx.request_repaint_after(Duration::from_millis(interval));
    }
}

fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    section_impl(ui, title, None, None, add_contents)
}

fn section_sized<R>(
    ui: &mut egui::Ui,
    title: &str,
    min_height: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    section_impl(ui, title, None, Some(min_height), add_contents)
}

fn section_collapsible<R>(
    ui: &mut egui::Ui,
    title: &str,
    collapsed: &mut bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    section_impl(ui, title, Some(collapsed), None, add_contents)
}

fn section_impl<R>(
    ui: &mut egui::Ui,
    title: &str,
    collapsed: Option<&mut bool>,
    min_height: Option<f32>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let title_color = egui::Color32::from_rgb(120, 130, 150);
    egui::Frame::default()
        .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(58, 63, 78)))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if let Some(h) = min_height {
                ui.set_min_height(h);
            }
            let title_rich = egui::RichText::new(title)
                .size(10.5)
                .strong()
                .color(title_color);

            let Some(collapsed) = collapsed else {
                ui.label(title_rich);
                ui.add_space(4.0);
                return Some(add_contents(ui));
            };

            let toggled = ui
                .horizontal(|ui| {
                    let icon = if *collapsed { "▸" } else { "▾" };
                    let header = ui.add(
                        egui::Label::new(title_rich).sense(egui::Sense::click()),
                    );
                    let icon_resp = ui
                        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(icon).size(10.5).color(title_color),
                                )
                                .sense(egui::Sense::click()),
                            )
                        })
                        .inner;
                    let hovered = header.hovered() || icon_resp.hovered();
                    if hovered {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    header.clicked() || icon_resp.clicked()
                })
                .inner;
            if toggled {
                *collapsed = !*collapsed;
            }
            if *collapsed {
                None
            } else {
                ui.add_space(4.0);
                Some(add_contents(ui))
            }
        })
        .inner
}

fn call_name_now() -> String {
    rusqlite::Connection::open_in_memory()
        .and_then(|c| {
            c.query_row(
                "SELECT strftime('%Y-%m-%d %H:%M','now','localtime')",
                [],
                |r| r.get::<_, String>(0),
            )
        })
        .unwrap_or_else(|_| "кол".to_string())
}

fn device_label(target: &Option<String>, list: &[audio::Device], default: &str) -> String {
    match target {
        None => default.to_string(),
        Some(t) => list
            .iter()
            .find(|d| &d.target == t)
            .map(|d| d.label.clone())
            .unwrap_or_else(|| t.clone()),
    }
}

fn draw_scope(ui: &mut egui::Ui, samples: &[f32], color: egui::Color32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(10, 12, 16));

    if samples.len() < 2 {
        return;
    }
    let mid = rect.center().y;
    let amp = rect.height() * 0.45;
    let n = samples.len();
    let pts: Vec<egui::Pos2> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let x = rect.left() + rect.width() * (i as f32 / (n - 1) as f32);
            let y = mid - s.clamp(-1.0, 1.0) * amp;
            egui::pos2(x, y)
        })
        .collect();
    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0, color)));
    painter.line_segment(
        [egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)],
        egui::Stroke::new(0.5, egui::Color32::from_rgb(40, 44, 52)),
    );
}

fn draw_transcript(
    ui: &mut egui::Ui,
    data: Option<(String, String)>,
    color: egui::Color32,
    id_salt: &str,
) -> Option<String> {
    let (finals, partial) = match data {
        Some(t) => t,
        None => return None,
    };
    ui.add_space(2.0);
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .max_height(140.0)
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.set_min_height(140.0);
            if finals.is_empty() && partial.is_empty() {
                ui.label(
                    egui::RichText::new("… слушаю")
                        .size(11.0)
                        .italics()
                        .color(egui::Color32::from_rgb(90, 96, 108)),
                );
                return None;
            }
            let mut picked = None;
            if !finals.is_empty() {
                let mut text = finals.clone();
                let mut layouter = |ui: &egui::Ui, s: &str, wrap: f32| {
                    ui.fonts(|f| f.layout_job(transcript_job(s, color, wrap)))
                };
                let out = egui::TextEdit::multiline(&mut text)
                    .id_salt(id_salt)
                    .frame(false)
                    .desired_width(f32::INFINITY)
                    .desired_rows(3)
                    .layouter(&mut layouter)
                    .show(ui);
                if ui.input(|i| i.pointer.any_released()) {
                    if let Some(range) = out.cursor_range {
                        let chars = range.as_sorted_char_range();
                        if chars.start != chars.end {
                            let selected: String = text
                                .chars()
                                .skip(chars.start)
                                .take(chars.end - chars.start)
                                .collect();
                            if !selected.is_empty() {
                                ui.ctx().copy_text(selected.clone());
                                picked = Some(selected);
                            }
                        }
                    }
                }
            }
            if !partial.is_empty() {
                ui.label(
                    egui::RichText::new(&partial)
                        .italics()
                        .size(20.0)
                        .color(egui::Color32::from_rgb(140, 146, 158)),
                );
            }
            picked
        })
        .inner
}

fn transcript_job(text: &str, color: egui::Color32, wrap: f32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    job.append(
        text,
        0.0,
        TextFormat {
            font_id: egui::FontId::proportional(20.0),
            color,
            ..Default::default()
        },
    );
    job
}

fn draw_resize_grip(ui: &mut egui::Ui, ctx: &egui::Context, grip: egui::Rect) {
    let resp = ui.interact(
        grip,
        ui.id().with("resize-grip"),
        egui::Sense::click_and_drag(),
    );
    if resp.drag_started() {
        ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(
            egui::ResizeDirection::SouthEast,
        ));
    }
    if resp.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::ResizeNwSe);
    }

    let color = if resp.hovered() {
        egui::Color32::from_rgb(180, 200, 255)
    } else {
        egui::Color32::from_rgb(120, 128, 140)
    };
    let painter = ui.painter_at(grip);
    for i in 1..=3 {
        let off = i as f32 * 4.0;
        painter.line_segment(
            [
                egui::pos2(grip.right() - off, grip.bottom()),
                egui::pos2(grip.right(), grip.bottom() - off),
            ],
            egui::Stroke::new(1.5, color),
        );
    }
}

fn rebuild_and_restart() {
    let Ok(mut exe) = std::env::current_exe() else {
        return;
    };
    if let Some(clean) = exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        exe = std::path::PathBuf::from(clean);
    }
    let pid = std::process::id();
    let manifest = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .map(|d| d.join("Cargo.toml"))
        .filter(|m| m.exists());
    let build = match &manifest {
        Some(m) => format!(
            "cargo build --release --manifest-path '{}' > '{}' 2>&1; ",
            m.display(),
            m.with_file_name("target").join("restart-build.log").display(),
        ),
        None => String::new(),
    };
    let script = format!(
        "while kill -0 {pid} 2>/dev/null; do sleep 0.1; done; {build}exec '{}'",
        exe.display(),
    );
    let _ = std::process::Command::new("setsid")
        .arg("sh")
        .arg("-c")
        .arg(script)
        .spawn();
    std::process::exit(0);
}

fn notify(body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "health-widget", "-i", "dialog-warning", "⌨ Печать кода", body])
        .spawn();
}

fn looks_like_code(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if t.contains('\n') || t.contains('\t') {
        return true;
    }
    if t.contains("  ") {
        return true;
    }
    const PUNCT: &[char] = &['{', '}', '(', ')', '[', ']', ';', '=', '<', '>', '#', '/', '\\', '|', '&', '*', '@', '$', '`'];
    if t.contains(PUNCT) {
        return true;
    }
    const TOKENS: &[&str] = &[
        "fn ", "def ", "class ", "const ", "let ", "var ", "func ", "import ", "return ",
        "public ", "private ", "void ", "int ", "::", "->", "=>", "//", "/*",
    ];
    TOKENS.iter().any(|k| t.contains(k))
}

fn type_clipboard_code() {
    telemetry::event("hotkey.paste_code", serde_json::json!({}));
    std::thread::spawn(|| {
        let Some(text) = clip::get() else {
            notify("Не удалось прочитать буфер обмена");
            return;
        };
        if text.trim().is_empty() {
            notify("Буфер обмена пуст");
            return;
        }
        if !looks_like_code(&text) {
            notify("В буфере не похоже на код — печать отменена");
            return;
        }
        if let Err(e) = winctl::type_text(text) {
            notify(&format!("Печать не удалась: {e}"));
        }
    });
}

fn main() -> eframe::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--grab-test") {
        kwin_shot::grab_test();
        std::process::exit(0);
    }

    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!(
            "health-widget v{} (commit {}, сборка {})",
            env!("CARGO_PKG_VERSION"),
            env!("GIT_HASH"),
            env!("BUILD_TIME"),
        );
        std::process::exit(0);
    }

    if let Some(arg @ ("--transcript" | "--transcript-today")) =
        std::env::args().nth(1).as_deref()
    {
        let today = arg == "--transcript-today";
        match TranscriptLog::dump(today) {
            Some(text) if !text.is_empty() => print!("{text}"),
            Some(_) => eprintln!("транскрипция пуста"),
            None => {
                eprintln!("нет БД транскрипции (ещё ничего не записано)");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    if std::env::args().nth(1).as_deref() == Some("--calls") {
        match TranscriptLog::list_calls() {
            Some(text) if !text.is_empty() => print!("{text}"),
            Some(_) => eprintln!("колов пока нет"),
            None => {
                eprintln!("нет БД транскрипции (ещё ничего не записано)");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    if std::env::args().nth(1).as_deref() == Some("--export") {
        let args: Vec<String> = std::env::args().collect();
        let id: Option<i64> = args.get(2).and_then(|s| s.parse().ok());
        let Some(id) = id else {
            eprintln!("использование: health-widget --export <id кола> [папка]");
            std::process::exit(2);
        };
        let dest = args
            .get(3)
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("tmp"));
        match TranscriptLog::export_call(id, &dest) {
            Ok(path) => println!("кол #{id} выгружен в {}", path.display()),
            Err(e) => {
                eprintln!("ошибка экспорта: {e}");
                std::process::exit(1);
            }
        }
        std::process::exit(0);
    }

    if std::env::args().nth(1).as_deref() == Some("--check-capture") {
        if !detect::available() {
            eprintln!("детект недоступен: нет ни pw-dump, ни busctl");
            std::process::exit(2);
        }
        let active = detect::screencast_active();
        println!(
            "захват экрана: {}",
            if active { "АКТИВЕН (виджет спрятался бы)" } else { "не обнаружен" }
        );
        std::process::exit(if active { 0 } else { 1 });
    }

    if let Some(arg @ ("--telemetry" | "--telemetry-today")) =
        std::env::args().nth(1).as_deref()
    {
        let today = arg == "--telemetry-today";
        let limit = std::env::args()
            .nth(2)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(200);
        match telemetry::dump(limit, today) {
            Some(s) => println!("{s}"),
            None => eprintln!("нет телеметрии: {:?}", telemetry::path()),
        }
        std::process::exit(0);
    }

    telemetry::init();
    instance::acquire_or_replace();
    instance::wait_whisper_gone();

    let cfg = Config::load();
    let st = state::load();

    let shared = Arc::new(Shared {
        user_visible: AtomicBool::new(true),
        sharing_active: AtomicBool::new(false),
        shutdown: AtomicBool::new(false),
        pos: std::sync::Mutex::new(st.x.zip(st.y).map(|(x, y)| (x as i32, y as i32))),
    });

    let base_w = st.width.unwrap_or(cfg.width);
    let start_w = if st.terminal_open {
        base_w + st.terminal_width.unwrap_or(TERMINAL_W)
    } else {
        base_w
    };
    let size = [start_w, st.height.unwrap_or(cfg.height)];
    let pos = [st.x.unwrap_or(cfg.x), st.y.unwrap_or(cfg.y)];

    screenshot::ensure_registered();

    winctl::ensure_dotoold();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("health-widget")
        .with_inner_size(size)
        .with_position(pos)
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_resizable(true);

    if cfg.click_through {
        viewport = viewport.with_mouse_passthrough(true);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let cfg_for_app = cfg.clone();
    eframe::run_native(
        "health-widget",
        native_options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, cfg_for_app, shared, st)))),
    )
}

