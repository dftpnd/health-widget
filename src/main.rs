
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod audio;
mod clip;
mod config;
mod data;
mod detect;
mod hr_reply;
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
mod winctl;

use config::Config;
use data::Metrics;
use transcript_log::TranscriptLog;

const CH_MIC: &str = "🎤 я";
const CH_ZOOM: &str = "🔊 телемост";

const GRIP: f32 = 16.0;
const MARGIN: f32 = 12.0;

const PILOT_PROFILES: &[(&str, &str)] = &[
    ("fullstack", "Fullstack"),
    ("back", "Backend"),
    ("bulat", "Булат"),
];

const PILOT_STRICTNESS: &[(&str, &str, f32)] = &[
    ("strict", "Строго", 0.55),
    ("medium", "Средне", 0.50),
    ("any", "Любые", 0.0),
];
const DEFAULT_STRICTNESS: &str = "medium";

const TERMINAL_W: f32 = 340.0;

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

struct App {
    cfg: Config,
    shared: Arc<Shared>,
    metrics: Metrics,
    last_mtime: Option<std::time::SystemTime>,
    currently_visible: bool,
    transcript_log: Option<Arc<TranscriptLog>>,
    active_call: Option<ActiveCall>,
    call_name_input: String,
    audio: AudioState,
    pinned: bool,
    autopilot: AutopilotState,
    hr_reply: Arc<std::sync::Mutex<hr_reply::HrReplyState>>,
    last_saved: state::State,
    prev_state: state::State,
    stable_since: Instant,
    shot: ShotState,
    mark_mic: Arc<AtomicU32>,
    mark_zoom: Arc<AtomicU32>,
    mark_mic_seen: u32,
    mark_zoom_seen: u32,
    cursor_warp_request: Arc<AtomicBool>,
    prev_cursor: Arc<std::sync::Mutex<Option<(f64, f64)>>>,
    markers_mic: MarkerState,
    markers_zoom: MarkerState,
    terminal: Option<terminal::Terminal>,
    terminal_open: bool,
    width_one_col: Option<f32>,
    terminal_width: f32,
    autopilot_collapsed: bool,
    metrics_collapsed: bool,
}

#[derive(Default)]
struct MarkerState {
    spans: Vec<(usize, usize)>,
    active_start: Option<usize>,
}

impl MarkerState {
    fn toggle(&mut self, len: usize) -> Option<(usize, usize)> {
        match self.active_start.take() {
            None => {
                self.active_start = Some(len);
                None
            }
            Some(start) => {
                let end = len;
                if start < end {
                    self.spans.push((start, end));
                    Some((start, end))
                } else {
                    None
                }
            }
        }
    }
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

        let mark_mic = Arc::new(AtomicU32::new(0));
        let mark_zoom = Arc::new(AtomicU32::new(0));
        let cursor_warp_request = Arc::new(AtomicBool::new(false));

        {
            let shared = shared.clone();
            let ctx = cc.egui_ctx.clone();
            let shot_request = shot_request.clone();
            let mark_mic = mark_mic.clone();
            let mark_zoom = mark_zoom.clone();
            let cursor_warp_request = cursor_warp_request.clone();
            let rt_mic = libc::SIGRTMIN();
            let rt_zoom = libc::SIGRTMIN() + 1;
            let rt_cursor_warp = libc::SIGRTMIN() + 2;
            std::thread::spawn(move || {
                let mut signals = signal_hook::iterator::Signals::new([
                    signal_hook::consts::SIGUSR1,
                    signal_hook::consts::SIGUSR2,
                    rt_mic,
                    rt_zoom,
                    rt_cursor_warp,
                ])
                .expect("cannot register signal handler");
                for sig in signals.forever() {
                    if sig == signal_hook::consts::SIGUSR2 {
                        shot_request.store(true, Ordering::Relaxed);
                        ctx.request_repaint();
                        continue;
                    }
                    if sig == rt_mic {
                        mark_mic.fetch_add(1, Ordering::Relaxed);
                        ctx.request_repaint();
                        continue;
                    }
                    if sig == rt_zoom {
                        mark_zoom.fetch_add(1, Ordering::Relaxed);
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
            mark_mic: mark_mic.clone(),
            mark_zoom: mark_zoom.clone(),
            cursor_warp_request: cursor_warp_request.clone(),
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

        {
            let want_pin = st.pinned;
            let pos = st.x.zip(st.y).map(|(x, y)| (x as i32, y as i32));
            if want_pin || pos.is_some() {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(800));
                    for _ in 0..2 {
                        if want_pin {
                            winctl::set_keep_above(true);
                        }
                        if let Some((x, y)) = pos {
                            winctl::set_position(x, y);
                        }
                        std::thread::sleep(Duration::from_millis(600));
                    }
                });
            }
        }

        {
            let shared = shared.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(4));
                if let Some(p) = winctl::get_position() {
                    if let Ok(mut g) = shared.pos.lock() {
                        *g = Some(p);
                    }
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
            audio::AudioMonitor::start(
                audio::default_monitor().as_deref(),
                CH_ZOOM,
                transcript_log.clone(),
            )
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
            transcript_log,
            active_call: None,
            call_name_input: String::new(),
            audio: AudioState {
                mic,
                zoom,
                mic_target: st.mic_target.clone(),
                prog_target: None,
                mics: audio::list_mics(),
                programs: audio::list_programs(),
                scope: Vec::with_capacity(2048),
            },
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
            mark_mic,
            mark_zoom,
            mark_mic_seen: 0,
            mark_zoom_seen: 0,
            cursor_warp_request,
            prev_cursor: Arc::new(std::sync::Mutex::new(None)),
            markers_mic: MarkerState::default(),
            markers_zoom: MarkerState::default(),
            terminal: None,
            terminal_open: st.terminal_open,
            width_one_col: None,
            terminal_width: st.terminal_width.unwrap_or(TERMINAL_W),
            autopilot_collapsed: st.autopilot_collapsed,
            metrics_collapsed: st.metrics_collapsed,
        }
    }

    fn start_program(&self) -> Option<audio::AudioMonitor> {
        let target = self.audio.prog_target.clone().or_else(audio::default_monitor);
        audio::AudioMonitor::start(target.as_deref(), CH_ZOOM, self.transcript_log.clone())
    }

    fn start_call(&mut self) {
        let Some(log) = self.transcript_log.clone() else {
            return;
        };
        let name = match self.call_name_input.trim() {
            "" => "без названия".to_string(),
            n => n.to_string(),
        };
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
        if same_phase && same_profile && same_sim {
            return;
        }
        self.autopilot.proc = None;
        self.autopilot.proc = pilot::Pilot::start(
            &self.cfg.autopilot_dir,
            &self.cfg.autopilot_bin,
            desired,
            Some(self.autopilot.profile.as_str()),
            Some(self.pilot_min_sim()),
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
            terminal_open: self.terminal_open,
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
                    .selectable_label(self.terminal_open, "🖥")
                    .on_hover_text("Терминал")
                    .clicked()
                {
                    toggle_terminal = true;
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
        ui.add_space(2.0);
    }

    fn draw_sound(&mut self, ui: &mut egui::Ui) {
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

        section(ui, "🎧 Звук и транскрипция", |ui| {
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

    fn draw_call_and_screen(&mut self, ui: &mut egui::Ui) {
        self.reconcile_call_recording();
        let mut call_toggle = false;
        let active_name = self.active_call.as_ref().map(|c| c.name.clone());
        let mut name_buf = self.call_name_input.clone();

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

        ui.columns(2, |cols| {
            section(&mut cols[0], "🎙 Кол", |ui| {
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
                    } else {
                        ui.add(
                            egui::TextEdit::singleline(&mut name_buf)
                                .hint_text("название")
                                .desired_width(f32::INFINITY),
                        );
                    }
                });
            });
            section(&mut cols[1], "📸 Скрин", |ui| {
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
        });

        self.call_name_input = name_buf;
        if call_toggle {
            if self.active_call.is_some() {
                self.end_call();
            } else {
                self.start_call();
            }
        }
        if shoot {
            self.shot.request.store(true, Ordering::Relaxed);
        }
        ui.add_space(2.0);
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
                        if ui
                            .selectable_label(active, *label)
                            .on_hover_text(if *thr > 0.0 {
                                format!("Порог ≥ {thr:.2} — только достаточно близкие вакансии")
                            } else {
                                "Без порога — откликаться на весь пул (по убыванию похожести)"
                                    .to_string()
                            })
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
                                "Сканировать все группы подряд в очередь \
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
                                        "Скан группы в очередь откликов \
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

    fn draw_metrics(&mut self, ui: &mut egui::Ui) {
        if !self.metrics.items.is_empty() || self.metrics.title.is_none() {
            let mut metrics_collapsed = self.metrics_collapsed;
            section_collapsible(ui, "📊 Показатели", &mut metrics_collapsed, |ui| {
                for m in &self.metrics.items {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&m.label)
                                .color(egui::Color32::from_rgb(150, 150, 160)),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    egui::RichText::new(&m.value)
                                        .strong()
                                        .color(egui::Color32::from_rgb(235, 235, 240)),
                                );
                            },
                        );
                    });
                }
                if self.metrics.items.is_empty() && self.metrics.title.is_none() {
                    ui.label(
                        egui::RichText::new(format!(
                            "нет данных: {}",
                            self.cfg.json_path.display()
                        ))
                        .color(egui::Color32::from_rgb(200, 120, 120)),
                    );
                }
            });
            self.metrics_collapsed = metrics_collapsed;
        }
    }

    fn process_marker_presses(&mut self) {
        let c_mic = self.mark_mic.load(Ordering::Relaxed);
        let mic_finals = self.audio.mic.as_ref().and_then(|m| m.transcript()).map(|(f, _)| f);
        if let Some(txt) = apply_mark_presses(
            &mut self.markers_mic,
            &mut self.mark_mic_seen,
            c_mic,
            mic_finals.as_deref(),
        ) {
            telemetry::event("mark.copy", serde_json::json!({ "channel": CH_MIC, "len": txt.len() }));
            clip::set_async(txt);
        }
        let c_zoom = self.mark_zoom.load(Ordering::Relaxed);
        let zoom_finals = self.audio.zoom.as_ref().and_then(|m| m.transcript()).map(|(f, _)| f);
        if let Some(txt) = apply_mark_presses(
            &mut self.markers_zoom,
            &mut self.mark_zoom_seen,
            c_zoom,
            zoom_finals.as_deref(),
        ) {
            telemetry::event("mark.copy", serde_json::json!({ "channel": CH_ZOOM, "len": txt.len() }));
            clip::set_async(txt);
        }
    }

    fn draw_scopes(&mut self, ui: &mut egui::Ui) {
        if self.audio.mic.is_some() || self.audio.zoom.is_some() {
            section(ui, "📈 Осциллограммы", |ui| {
                if let Some(mon) = &self.audio.mic {
                    mon.snapshot(&mut self.audio.scope);
                    let color = egui::Color32::from_rgb(120, 210, 150);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("🎤 Микрофон").size(11.0).color(color),
                        );
                        marker_recording_badge(ui, &self.markers_mic);
                    });
                    draw_scope(ui, &self.audio.scope, color);
                    draw_transcript(ui, mon.transcript(), color, "mic", &self.markers_mic);
                }
                if let Some(mon) = &self.audio.zoom {
                    mon.snapshot(&mut self.audio.scope);
                    let color = egui::Color32::from_rgb(130, 180, 250);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("🔊 Zoom/Телемост").size(11.0).color(color),
                        );
                        marker_recording_badge(ui, &self.markers_zoom);
                    });
                    draw_scope(ui, &self.audio.scope, color);
                    draw_transcript(ui, mon.transcript(), color, "zoom", &self.markers_zoom);
                }
            });
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

        if self.shot.request.swap(false, Ordering::Relaxed) && !self.shot.active {
            telemetry::event("shot.request", serde_json::json!({}));
            self.shot.active = true;
            self.shot.points.clear();
            *self.shot.status.lock().unwrap() = screenshot::ShotStatus::Marking;
        }
        if self.shot.active {
            self.show_shot_overlay(ctx);
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

        if self.autopilot.proc.as_mut().is_some_and(|p| !p.alive()) {
            let done_msg = match self.autopilot.proc.as_ref().map(|p| p.phase()) {
                Some(pilot::Phase::Scan(_)) | Some(pilot::Phase::ScanAll) => Some("скан завершён"),
                Some(pilot::Phase::Enrich) => Some("обогащение завершено"),
                _ => None,
            };
            self.autopilot.proc = None;
            self.autopilot.want = None;
            let msg = done_msg.unwrap_or("автопилот остановлен");
            self.autopilot.status = msg.to_string();
            telemetry::event("pilot.exit", serde_json::json!({ "reason": msg }));
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

                self.draw_sound(ui);

                self.draw_call_and_screen(ui);

                self.draw_autopilot(ui);

                self.draw_metrics(ui);

                self.process_marker_presses();

                self.draw_scopes(ui);

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
    section_impl(ui, title, None, add_contents)
}

fn section_collapsible<R>(
    ui: &mut egui::Ui,
    title: &str,
    collapsed: &mut bool,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    section_impl(ui, title, Some(collapsed), add_contents)
}

fn section_impl<R>(
    ui: &mut egui::Ui,
    title: &str,
    collapsed: Option<&mut bool>,
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

fn marker_recording_badge(ui: &mut egui::Ui, markers: &MarkerState) {
    if markers.active_start.is_some() {
        let pulse = 0.5 + 0.5 * (ui.input(|i| i.time) * 3.0).sin() as f32;
        ui.label(
            egui::RichText::new("🔴 идёт запись маркера")
                .size(11.0)
                .color(egui::Color32::from_rgb(235, 90, 90).gamma_multiply(pulse)),
        );
    }
}

fn draw_transcript(
    ui: &mut egui::Ui,
    data: Option<(String, String)>,
    color: egui::Color32,
    id_salt: &str,
    markers: &MarkerState,
) {
    let (finals, partial) = match data {
        Some(t) => t,
        None => return,
    };
    if finals.is_empty() && partial.is_empty() {
        ui.label(
            egui::RichText::new("… слушаю")
                .size(11.0)
                .italics()
                .color(egui::Color32::from_rgb(90, 96, 108)),
        );
        return;
    }
    ui.add_space(2.0);
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .max_height(140.0)
        .auto_shrink([false, true])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if !finals.is_empty() {
                let mut text = finals.clone();
                let hl = egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 60);
                let mut ranges: Vec<(usize, usize)> = markers.spans.clone();
                if let Some(start) = markers.active_start {
                    ranges.push((start, text.len()));
                }
                let mut layouter = |ui: &egui::Ui, s: &str, wrap: f32| {
                    ui.fonts(|f| f.layout_job(transcript_job(s, &ranges, color, hl, wrap)))
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
                                ui.ctx().copy_text(selected);
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
        });
}

fn apply_mark_presses(
    markers: &mut MarkerState,
    seen: &mut u32,
    count: u32,
    finals: Option<&str>,
) -> Option<String> {
    let mut presses = count.wrapping_sub(*seen);
    *seen = count;
    if presses == 0 {
        return None;
    }
    if presses > 8 {
        presses = 1;
    }
    let text = finals.unwrap_or("");
    let len = text.len();
    let mut to_copy = None;
    for _ in 0..presses {
        if let Some((s, e)) = markers.toggle(len) {
            if let Some(slice) = text.get(s..e) {
                if !slice.is_empty() {
                    to_copy = Some(slice.to_string());
                }
            }
        }
    }
    to_copy
}

fn transcript_job(
    text: &str,
    ranges: &[(usize, usize)],
    color: egui::Color32,
    hl: egui::Color32,
    wrap: f32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let n = text.len();
    let mut cuts = vec![0usize, n];
    for &(s, e) in ranges {
        for mut b in [s.min(n), e.min(n)] {
            while b > 0 && !text.is_char_boundary(b) {
                b -= 1;
            }
            cuts.push(b);
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    let font = egui::FontId::proportional(20.0);
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let inside = ranges.iter().any(|&(s, e)| a >= s && a < e);
        let bg = if inside { hl } else { egui::Color32::TRANSPARENT };
        job.append(
            &text[a..b],
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                background: bg,
                ..Default::default()
            },
        );
    }
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

    telemetry::init();

    let cfg_for_app = cfg.clone();
    eframe::run_native(
        "health-widget",
        native_options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, cfg_for_app, shared, st)))),
    )
}

#[cfg(test)]
mod marker_tests {
    use super::{apply_mark_presses, MarkerState};

    #[test]
    fn start_then_stop_makes_span() {
        let mut m = MarkerState::default();
        assert_eq!(m.toggle(5), None);
        assert_eq!(m.active_start, Some(5));
        assert_eq!(m.toggle(12), Some((5, 12)));
        assert_eq!(m.spans, vec![(5, 12)]);
        assert_eq!(m.active_start, None);
    }

    #[test]
    fn markers_accumulate() {
        let mut m = MarkerState::default();
        m.toggle(0);
        m.toggle(3);
        m.toggle(5);
        m.toggle(9);
        assert_eq!(m.spans, vec![(0, 3), (5, 9)]);
    }

    #[test]
    fn empty_span_not_recorded() {
        let mut m = MarkerState::default();
        m.toggle(4);
        assert_eq!(m.toggle(4), None);
        assert!(m.spans.is_empty());
    }

    #[test]
    fn presses_start_grow_stop_copies_grown_slice() {
        let mut m = MarkerState::default();
        let mut seen = 0u32;
        let t1 = "привет";
        assert_eq!(apply_mark_presses(&mut m, &mut seen, 1, Some(t1)), None);
        assert_eq!(m.active_start, Some(12));
        let t2 = "привет мир";
        let got = apply_mark_presses(&mut m, &mut seen, 2, Some(t2));
        assert_eq!(got.as_deref(), Some(" мир"));
        assert_eq!(m.spans, vec![(12, 19)]);
    }

    #[test]
    fn no_new_presses_returns_none() {
        let mut m = MarkerState::default();
        let mut seen = 5u32;
        assert_eq!(apply_mark_presses(&mut m, &mut seen, 5, Some("abc")), None);
        assert_eq!(seen, 5);
    }

    #[test]
    fn press_on_disabled_channel_no_panic() {
        let mut m = MarkerState::default();
        let mut seen = 0u32;
        assert_eq!(apply_mark_presses(&mut m, &mut seen, 1, None), None);
        assert_eq!(apply_mark_presses(&mut m, &mut seen, 2, None), None);
        assert!(m.spans.is_empty());
    }
}
