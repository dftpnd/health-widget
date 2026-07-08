//! health-widget — приватный always-on-top виджет показателей здоровья (Wayland).
//!
//! Идея приватности по слоям (подробно — в README):
//!  * KDE Plasma 6.6+: окно помечается свойством KWin `excludeFromCapture` (KWin-скрипт
//!    health-widget-exclude) — виджет виден локально, но НЕ попадает в захват. Лучший слой;
//!    при нём авто-скрытие не нужно (HEALTH_AUTO_HIDE=0). Настраивается вне этого бинаря.
//!  * Виджет — ОТДЕЛЬНОЕ окно. Если ты шаришь одно окно/приложение, он в захват не попадает.
//!  * Авто-скрытие: фоновый поток детектит активный захват кросс-десктопно (PipeWire, а на
//!    GNOME — резервно org.gnome.Mutter.ScreenCast) и прячет виджет сам (best-effort).
//!  * SIGUSR1 мгновенно прячет/показывает виджет (повесь на него системный ярлык).
//!
//! Клиент на Wayland сам «не захватывать это окно» выставить не может. На KDE 6.6+ это делает
//! KWin (слой выше); на GNOME системного флага нет — там работает «шарь одно окно» +
//! авто-скрытие + хоткей.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod audio;
mod config;
mod data;
mod detect;
mod pilot;
mod pilot_scan;
mod pilot_stats;
mod pilot_notify;
mod recorder;
mod state;
mod transcribe;
mod transcript_log;
mod winctl;

use config::Config;
use data::Metrics;
use transcript_log::TranscriptLog;

/// Метки каналов для сохранения транскрипции (кто говорит): микрофон — «я»,
/// звук созвона — «телемост» (собеседники).
const CH_MIC: &str = "🎤 я";
const CH_ZOOM: &str = "🔊 телемост";

/// Размер угловой ручки ресайза (в точках).
const GRIP: f32 = 16.0;
/// Внутренние поля рамки виджета (в точках) — совпадают с Frame::inner_margin.
const MARGIN: f32 = 12.0;

/// Профили автопилота: (имя для `--profile` = аккаунт/резюме, подпись в UI).
/// Первый — базовый (legacy-пути автопилота, уже залогиненный браузер).
const PILOT_PROFILES: &[(&str, &str)] = &[
    ("fullstack", "Fullstack"),
    ("back", "Backend"),
    ("bulat", "Булат"),
];

/// Строгость откликов: (ключ в state.json, подпись, порог косинуса к резюме).
/// Порог уходит автопилоту как MIN_SIMILARITY — вакансии ниже него не откликаются.
/// Значения по шкале модели эмбеддингов (косинусы пула ~0.14…0.69, медиана ~0.46):
/// «Строго» 0.55 ≈ топ‑22%, «Средне» 0.50 ≈ топ‑37%, «Любые» 0.0 — весь пул.
const PILOT_STRICTNESS: &[(&str, &str, f32)] = &[
    ("strict", "Строго", 0.55),
    ("medium", "Средне", 0.50),
    ("any", "Любые", 0.0),
];
/// Пресет строгости по умолчанию (старое состояние без поля).
const DEFAULT_STRICTNESS: &str = "medium";

/// Путь к per-profile сводке откликов. БД вакансий общая, но история/счётчики —
/// свои у каждого профиля: автопилот пишет `data/stats-<profile>.json`.
fn profile_stats_path(autopilot_dir: &std::path::Path, profile: &str) -> std::path::PathBuf {
    autopilot_dir
        .join("data")
        .join(format!("stats-{profile}.json"))
}

/// Общее состояние между потоками и UI.
struct Shared {
    /// Хочет ли пользователь видеть виджет (тумблер по SIGUSR1).
    user_visible: AtomicBool,
    /// Идёт ли захват экрана (ставит фоновый детектор).
    sharing_active: AtomicBool,
    /// Позиция окна по данным KWin (Wayland не отдаёт её клиенту; опрашивает фоновый поток).
    pos: std::sync::Mutex<Option<(i32, i32)>>,
    /// Запрошено корректное завершение (ставит поток SIGTERM/SIGINT). Обрабатывает update().
    shutdown: AtomicBool,
}

/// Идущий кол: id записи в БД и его название (для подписи кнопки/статуса).
struct ActiveCall {
    id: i64,
    name: String,
}

struct App {
    cfg: Config,
    shared: Arc<Shared>,
    metrics: Metrics,
    /// Кэш времени модификации json для авто-reload.
    last_mtime: Option<std::time::SystemTime>,
    /// Текущее фактическое состояние видимости окна (чтобы не слать команду каждый кадр).
    currently_visible: bool,
    /// Хранилище полной транскрипции (SQLite): каждый финальный сегмент с временем и
    /// каналом. None — БД не открылась (тогда пишется только бегущий хвост в UI).
    transcript_log: Option<Arc<TranscriptLog>>,
    /// Активный кол (запись звука+текста обоих каналов), None — сейчас не пишем.
    active_call: Option<ActiveCall>,
    /// Черновик названия следующего кола (вводится до старта записи).
    call_name_input: String,
    /// Канал микрофона (слушатель + осциллограф), None — выключен.
    mic: Option<audio::AudioMonitor>,
    /// Канал звука программы/вывода (Zoom/Телемост/Discord…), None — выключен.
    zoom: Option<audio::AudioMonitor>,
    /// Выбранный микрофон (имя источника; None — по умолчанию).
    mic_target: Option<String>,
    /// Выбранная программа (node id; None — весь вывод = monitor по умолчанию).
    prog_target: Option<String>,
    /// Списки для селекторов: микрофоны и играющие программы.
    mics: Vec<audio::Device>,
    programs: Vec<audio::Device>,
    /// Переиспользуемый буфер снимка сэмплов (без аллокаций на кадр).
    scope: Vec<f32>,
    /// Закреплено ли окно «поверх всех» (keep-above через KWin).
    pinned: bool,
    /// Процесс автопилота (work-autopilot), None — не запущен. Один на профиль браузера.
    pilot: Option<pilot::Pilot>,
    /// Желаемая фаза автопилота (кнопки взаимоисключающи): чат / отклики / ничего.
    want: Option<pilot::Phase>,
    /// Выбранный профиль автопилота (аккаунт/резюме): "fullstack" | "back" | "bulat".
    /// Определяет `--profile` при запуске и каталог данных для счётчиков/групп.
    pilot_profile: String,
    /// Строгость откликов ("strict" | "medium" | "any") — порог релевантности к резюме,
    /// уходит автопилоту как MIN_SIMILARITY. Определяет, на что бот вообще откликается.
    pilot_strictness: String,
    /// Короткое сообщение об ошибке старта автопилота (для показа под кнопками).
    pilot_status: String,
    /// Счётчики автопилота (отклики/чаты) из stats.json, None — файла ещё нет.
    pilot_stats: Option<pilot_stats::PilotStats>,
    /// mtime прочитанного stats.json — чтобы перечитывать только при изменении.
    pilot_stats_mtime: Option<std::time::SystemTime>,
    /// Группы скана + число новых вакансий из scan.json, None — файла ещё нет.
    pilot_scan: Option<pilot_scan::ScanStatus>,
    /// mtime прочитанного scan.json — перечитываем только при изменении.
    pilot_scan_mtime: Option<std::time::SystemTime>,
    /// Общий тумблер TG-уведомлений автопилота (из data/notify.json).
    pilot_notify_on: bool,
    /// Последнее сохранённое на диск состояние (чтобы не перезаписывать без изменений).
    last_saved: state::State,
    /// Состояние предыдущего кадра + момент его появления — для дебаунса записи.
    prev_state: state::State,
    stable_since: Instant,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config, shared: Arc<Shared>, st: state::State) -> Self {
        // Стартовая загрузка данных.
        let (metrics, last_mtime) = data::load(&cfg.json_path);

        // Профиль автопилота из состояния (старое состояние без поля → базовый fullstack).
        let pilot_profile = st
            .pilot_profile
            .clone()
            .unwrap_or_else(|| "fullstack".to_string());
        let pilot_strictness = st
            .pilot_strictness
            .clone()
            .unwrap_or_else(|| DEFAULT_STRICTNESS.to_string());

        // Поток обработки SIGUSR1: тумблер видимости + перерисовка.
        {
            let shared = shared.clone();
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                let mut signals =
                    signal_hook::iterator::Signals::new([signal_hook::consts::SIGUSR1])
                        .expect("cannot register SIGUSR1 handler");
                for _ in signals.forever() {
                    let prev = shared.user_visible.load(Ordering::Relaxed);
                    shared.user_visible.store(!prev, Ordering::Relaxed);
                    ctx.request_repaint();
                }
            });
        }

        // Поток корректного завершения по SIGTERM/SIGINT: ставит флаг и будит UI. Саму
        // очистку (гашение автопилота → закрытие браузера → снятие lock профиля) делает
        // update() на главном потоке, где живёт self.pilot. Без этого голый SIGTERM убил
        // бы GUI мгновенно, без Drop, осиротив автопилот с залоченным профилем браузера.
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

        // Поток детекта screencast (если включено и busctl доступен).
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

        // Восстановление позиции/закрепления через KWin — с задержкой и повтором: на первом
        // кадре окно ещё не размещено, и начальная раскладка KWin перебивает наш set_position.
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

        // Поток опроса позиции окна через KWin (Wayland не даёт её клиенту напрямую).
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

        // Скан-статус (группы + счётчики) в фоне: шеллим `autopilot scan-status`, чтобы
        // кнопки групп появились в блоке автопилота ещё до первого запуска скана.
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

        // Хранилище полной транскрипции (общее на оба канала за сеанс). None — БД не
        // открылась, тогда транскрипция работает как раньше (только бегущий хвост в UI).
        let transcript_log = TranscriptLog::open().map(Arc::new);

        // Восстанавливаем ранее включённые каналы (микрофон — с сохранённым устройством;
        // программа эфемерна, поэтому zoom-канал стартует с «весь вывод» = monitor по умолчанию).
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
            mic,
            zoom,
            mic_target: st.mic_target.clone(),
            prog_target: None,
            mics: audio::list_mics(),
            programs: audio::list_programs(),
            scope: Vec::with_capacity(2048),
            pinned: st.pinned,
            // Автопилот НЕ восстанавливаем при старте (боевой режим шлёт реальные отклики):
            // кнопки всегда стартуют неактивными, запуск — только по явному клику.
            pilot: None,
            want: None,
            pilot_profile,
            pilot_strictness,
            pilot_status: String::new(),
            pilot_stats: None,
            pilot_stats_mtime: None,
            pilot_scan: None,
            pilot_scan_mtime: None,
            pilot_notify_on,
            last_saved: st.clone(),
            prev_state: st.clone(),
            stable_since: Instant::now(),
        }
    }

    /// Запустить zoom-канал: выбранная программа (node id) или, если не выбрана, весь вывод.
    fn start_program(&self) -> Option<audio::AudioMonitor> {
        let target = self.prog_target.clone().or_else(audio::default_monitor);
        audio::AudioMonitor::start(target.as_deref(), CH_ZOOM, self.transcript_log.clone())
    }

    /// Начать кол: создать запись в БД (название + дата), включить запись дорожек активных
    /// каналов и пометить транскрипцию этим колом.
    fn start_call(&mut self) {
        let Some(log) = self.transcript_log.clone() else {
            return; // без БД кол не завести
        };
        let name = match self.call_name_input.trim() {
            "" => "без названия".to_string(),
            n => n.to_string(),
        };
        let Some(id) = log.start_call(&name) else {
            return;
        };
        self.active_call = Some(ActiveCall { id, name });
        self.reconcile_call_recording();
    }

    /// Завершить кол: остановить запись дорожек (WAV финализируется) и проставить время конца.
    fn end_call(&mut self) {
        let Some(call) = self.active_call.take() else {
            return;
        };
        if let Some(mon) = &self.mic {
            mon.stop_recording();
        }
        if let Some(mon) = &self.zoom {
            mon.stop_recording();
        }
        if let Some(log) = &self.transcript_log {
            log.end_call(call.id);
        }
    }

    /// Привести запись дорожек в соответствие активному колу: каждый включённый канал,
    /// который ещё не пишется, начинает писать свою дорожку (mic.wav / zoom.wav). Вызывается
    /// при старте кола и после смены каналов — так канал, включённый по ходу кола, тоже пишется.
    fn reconcile_call_recording(&self) {
        let (Some(call), Some(log)) = (&self.active_call, &self.transcript_log) else {
            return;
        };
        let Some(dir) = transcript_log::call_dir(call.id) else {
            return;
        };
        for (mon, channel, file) in [
            (&self.mic, CH_MIC, "mic.wav"),
            (&self.zoom, CH_ZOOM, "zoom.wav"),
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

    /// Порог релевантности (MIN_SIMILARITY) для текущего пресета строгости.
    fn pilot_min_sim(&self) -> f32 {
        PILOT_STRICTNESS
            .iter()
            .find(|(k, _, _)| *k == self.pilot_strictness)
            .map(|(_, _, v)| *v)
            .unwrap_or(0.0)
    }

    /// Привести процесс автопилота в соответствие с желаемой фазой (`want`).
    /// None → гасим. Иначе, если процесса нет или фаза изменилась → (пере)запуск.
    /// Один браузер-профиль/окно → всегда ровно один процесс, смена фазы = рестарт.
    fn reconcile_pilot(&mut self) {
        let desired = match self.want.clone() {
            None => {
                self.pilot = None; // Drop мягко гасит процесс и закрывает браузер
                return;
            }
            Some(p) => p,
        };
        // Ничего не делаем, только если совпали И фаза, И профиль (аккаунт): смена
        // любого требует перезапуска — это другой браузер/окно.
        let same_phase = self.pilot.as_ref().map(|p| p.phase()) == Some(&desired);
        let same_profile =
            self.pilot.as_ref().map(|p| p.profile()) == Some(Some(self.pilot_profile.as_str()));
        // Порог релевантности тоже фиксируется при старте — его смена требует рестарта.
        let same_sim = self.pilot.as_ref().map(|p| p.min_sim()) == Some(self.pilot_min_sim());
        if same_phase && same_profile && same_sim {
            return; // уже крутится в нужной фазе, профиле и строгости
        }
        // Сперва гасим старую фазу (Drop → SIGTERM → браузер закрывается, профиль
        // освобождается), и только потом стартуем новую — иначе новый Chromium
        // поднимется на занятом профиле и уронит старый (TargetClosedError).
        self.pilot = None;
        self.pilot = pilot::Pilot::start(
            &self.cfg.autopilot_dir,
            &self.cfg.autopilot_bin,
            desired,
            Some(self.pilot_profile.as_str()),
            Some(self.pilot_min_sim()),
        );
        if self.pilot.is_none() {
            // Не стартовал — сбрасываем кнопки и показываем причину.
            self.want = None;
            self.pilot_status = "не удалось запустить автопилот".to_string();
        } else {
            self.pilot_status.clear();
        }
    }

    /// Собрать текущее состояние для сохранения (размер/позиция/источник/закрепление).
    fn current_state(&self, ctx: &egui::Context) -> state::State {
        let size = ctx.screen_rect().size();
        // Позицию берём из KWin (её опрашивает фоновый поток); нет данных — держим прежнюю.
        let (x, y) = match self.shared.pos.lock().ok().and_then(|g| *g) {
            Some((px, py)) => (Some(px as f32), Some(py as f32)),
            None => (self.last_saved.x, self.last_saved.y),
        };
        state::State {
            x,
            y,
            width: Some(size.x),
            height: Some(size.y),
            mic_on: self.mic.is_some(),
            mic_target: self.mic_target.clone(),
            zoom_on: self.zoom.is_some(),
            pinned: self.pinned,
            pilot_profile: Some(self.pilot_profile.clone()),
            pilot_strictness: Some(self.pilot_strictness.clone()),
        }
    }

    /// Пере-читать json, если файл изменился.
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
        // Счётчики откликов профиля (stats-<profile>.json) — свои у каждого аккаунта.
        let stats_path = profile_stats_path(&self.cfg.autopilot_dir, &self.pilot_profile);
        let mtime = std::fs::metadata(&stats_path).and_then(|m| m.modified()).ok();
        if mtime != self.pilot_stats_mtime {
            self.pilot_stats = pilot_stats::load(&stats_path);
            self.pilot_stats_mtime = mtime;
        }
        // Статус скана (scan.json) — общий пул на все профили. По mtime: во время скана
        // число растёт; виджет подхватывает без опроса содержимого.
        let scan_path = self.cfg.autopilot_dir.join("data").join("scan.json");
        let scan_mtime = std::fs::metadata(&scan_path).and_then(|m| m.modified()).ok();
        if scan_mtime != self.pilot_scan_mtime {
            self.pilot_scan = pilot_scan::load(&scan_path);
            self.pilot_scan_mtime = scan_mtime;
        }
        // Тумблер уведомлений (дёшево — читаем на каждой перезагрузке метрик).
        self.pilot_notify_on =
            pilot_notify::read_enabled(&self.cfg.autopilot_dir.join("data"));
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Полностью прозрачный фон окна — сам виджет рисует свою подложку.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Корректное завершение по SIGTERM/SIGINT: сперва штатно гасим автопилот
        // (reconcile_pilot с want=None → Drop у Pilot → закрытие браузера, снятие lock),
        // затем закрываем окно — событийный цикл выйдет, процесс завершится чисто.
        if self.shared.shutdown.load(Ordering::Relaxed) {
            self.end_call(); // финализируем WAV-дорожки и проставляем время конца кола
            self.want = None;
            self.reconcile_pilot();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        self.maybe_reload();

        // Автопилот мог сам завершиться/упасть — тогда гасим кнопки. Разовые фазы
        // (скан/обогащение) завершаются сами, дойдя до конца, — сообщаем об этом,
        // а не «остановлен».
        if self.pilot.as_mut().is_some_and(|p| !p.alive()) {
            let done_msg = match self.pilot.as_ref().map(|p| p.phase()) {
                Some(pilot::Phase::Scan(_)) | Some(pilot::Phase::ScanAll) => Some("скан завершён"),
                Some(pilot::Phase::Enrich) => Some("обогащение завершено"),
                _ => None,
            };
            self.pilot = None;
            self.want = None;
            self.pilot_status = done_msg.unwrap_or("автопилот остановлен").to_string();
        }

        // Итоговая видимость = пользователь хочет И (не идёт шаринг ИЛИ авто-скрытие выключено).
        let want_visible = self.shared.user_visible.load(Ordering::Relaxed)
            && !(self.cfg.auto_hide_on_share && self.shared.sharing_active.load(Ordering::Relaxed));

        if want_visible != self.currently_visible {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(want_visible));
            self.currently_visible = want_visible;
        }

        if want_visible {
            let bg = egui::Color32::from_rgba_unmultiplied(18, 18, 22, self.cfg.bg_alpha);
            let frame = egui::Frame::default()
                .fill(bg)
                .inner_margin(egui::Margin::same(MARGIN as i8))
                .corner_radius(10);

            let inner = egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                let panel = ui.max_rect();
                // Угол ресайза (правый-нижний) — резервируем под ручку, чтобы drag-move его не крал.
                let grip_rect =
                    egui::Rect::from_min_max(panel.max - egui::vec2(GRIP, GRIP), panel.max);

                // Тащить окно за любое пустое место подложки (декораций нет). На Wayland
                // клиент не может сам себя переместить — просим композитор начать интерактивный
                // move. Лейблы (только hover) drag не мешают; угол ресайза — исключаем.
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

                // Строка заголовка: слева — название, справа — кнопка «поверх всех» (📌).
                let title = self.metrics.title.clone();
                let pinned = self.pinned;
                let mut toggle_pin = false;
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
                        // Версия сборки — чтобы сразу видеть, свежий ли это бинарь.
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
                ui.add_space(2.0);

                // Два канала: микрофон и программа/вывод. У каждого — тумблер + селектор.
                let mic_on = self.mic.is_some();
                let zoom_on = self.zoom.is_some();
                let mic_target = self.mic_target.clone();
                let prog_target = self.prog_target.clone();
                let mut toggle_mic = false;
                let mut toggle_zoom = false;
                let mut mic_off = false;
                let mut zoom_off = false;
                let mut new_mic: Option<Option<String>> = None;
                let mut new_prog: Option<Option<String>> = None;
                let mut refresh = false;
                let mut refresh_mic = false;

                section(ui, "🎧 Звук и транскрипция", |ui| {
                // Строка 1 — микрофон: тумблер + выбор устройства.
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(mic_on, "🎤")
                        .on_hover_text("Слушать микрофон")
                        .clicked()
                    {
                        toggle_mic = true;
                    }
                    // Подпись селектора отражает состояние: выключен — «⊘ выключено».
                    let cur = if mic_on {
                        device_label(&mic_target, &self.mics, "🎤 по умолчанию")
                    } else {
                        "⊘ выключено".to_string()
                    };
                    egui::ComboBox::from_id_salt("mic-src")
                        .width(150.0)
                        .selected_text(egui::RichText::new(cur).size(11.0))
                        .show_ui(ui, |ui| {
                            // Первый пункт — отключить источник.
                            if ui.selectable_label(!mic_on, "⊘ выключено").clicked() {
                                mic_off = true;
                            }
                            if ui.selectable_label(mic_on && mic_target.is_none(), "🎤 по умолчанию").clicked() {
                                new_mic = Some(None);
                            }
                            for d in &self.mics {
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

                // Строка 2 — программа/вывод: тумблер + выбор программы + обновление списка.
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(zoom_on, "🔊")
                        .on_hover_text("Слушать звук программы или всего вывода")
                        .clicked()
                    {
                        toggle_zoom = true;
                    }
                    let cur = if zoom_on {
                        device_label(&prog_target, &self.programs, "🔊 весь вывод")
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
                                for d in &self.programs {
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
                }); // конец блока «Звук»

                // Применяем изменения выбора/тумблеров.
                if refresh {
                    self.mics = audio::list_mics();
                    self.programs = audio::list_programs();
                }
                if refresh_mic {
                    self.mics = audio::list_mics();
                }
                // Выбор устройства в списке = включить канал на нём (start и когда был выключен).
                if let Some(sel) = new_mic {
                    self.mic_target = sel;
                    self.mic = audio::AudioMonitor::start(self.mic_target.as_deref(), CH_MIC, self.transcript_log.clone());
                }
                if let Some(sel) = new_prog {
                    self.prog_target = sel;
                    self.zoom = self.start_program();
                }
                // Пункт «⊘ выключено» — погасить канал.
                if mic_off {
                    self.mic = None;
                }
                if zoom_off {
                    self.zoom = None;
                }
                if toggle_mic {
                    self.mic = if self.mic.is_some() {
                        None
                    } else {
                        audio::AudioMonitor::start(self.mic_target.as_deref(), CH_MIC, self.transcript_log.clone())
                    };
                }
                if toggle_zoom {
                    self.zoom = if self.zoom.is_some() {
                        None
                    } else {
                        self.start_program()
                    };
                }
                ui.add_space(2.0);

                // Кол: запись звонка (две дорожки WAV + транскрипт) под одним названием и
                // датой. Каналы могли смениться выше — приводим запись в соответствие колу.
                self.reconcile_call_recording();
                let mut call_toggle = false;
                let active_name = self.active_call.as_ref().map(|c| c.name.clone());
                let mut name_buf = self.call_name_input.clone();
                section(ui, "🎙 Кол", |ui| {
                    ui.horizontal(|ui| {
                        let recording = active_name.is_some();
                        let (label, hint) = if recording {
                            ("⏹ Завершить кол", "Остановить запись и сохранить кол")
                        } else {
                            ("🔴 Новый кол", "Начать запись звонка: звук обоих каналов + текст")
                        };
                        if ui.button(label).on_hover_text(hint).clicked() {
                            call_toggle = true;
                        }
                        if let Some(n) = &active_name {
                            ui.label(
                                egui::RichText::new(format!("● запись: {n}"))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(230, 120, 120)),
                            );
                        } else {
                            ui.add(
                                egui::TextEdit::singleline(&mut name_buf)
                                    .hint_text("название кола")
                                    .desired_width(150.0),
                            );
                        }
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
                ui.add_space(2.0);

                // Автопилот: взаимоисключающие тумблеры фаз (чат / отклики / скан группы) +
                // «Выключить»/«Пауза». Одно окно браузера → одна фаза за раз, любой выбор
                // (пере)запускает процесс. Показываем только если бинарь установлен.
                if self.cfg.autopilot_bin.exists() {
                    use pilot::Phase;
                    let mut new_want: Option<Option<Phase>> = None;
                    let mut new_profile: Option<String> = None;
                    let mut new_strictness: Option<String> = None;
                    let mut toggle_pause = false;
                    let running = self.want.is_some();
                    let paused = self.pilot.as_ref().is_some_and(|p| p.is_paused());
                    // Краткий статус: пауза → явная метка, иначе последняя строка лога
                    // автопилота либо сообщение об ошибке.
                    let status = if paused {
                        "⏸ на паузе".to_string()
                    } else {
                        self.pilot
                            .as_ref()
                            .and_then(|p| p.last_line())
                            .unwrap_or_else(|| self.pilot_status.clone())
                    };
                    section(ui, "🤖 Автопилот", |ui| {
                        // Профиль (аккаунт/резюме): под кем работает автопилот. Смена —
                        // перезапуск под другой аккаунт браузера (свой логин, свои счётчики).
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("👤").size(13.0)).on_hover_text(
                                "Профиль автопилота: аккаунт браузера, резюме и контакты",
                            );
                            let cur = PILOT_PROFILES
                                .iter()
                                .find(|(k, _)| *k == self.pilot_profile)
                                .map(|(_, l)| *l)
                                .unwrap_or("Fullstack");
                            egui::ComboBox::from_id_salt("pilot-profile")
                                .width(150.0)
                                .selected_text(egui::RichText::new(cur).size(11.0))
                                .show_ui(ui, |ui| {
                                    for (key, label) in PILOT_PROFILES {
                                        if ui
                                            .selectable_label(self.pilot_profile == *key, *label)
                                            .clicked()
                                            && self.pilot_profile != *key
                                        {
                                            new_profile = Some((*key).to_string());
                                        }
                                    }
                                });
                        });
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(self.want == Some(Phase::Chat), "💬 Чат")
                                .on_hover_text("Автопилот: вести чаты с работодателями")
                                .clicked()
                            {
                                // Повторный клик по активной — выключить; иначе переключить.
                                new_want = Some(if self.want == Some(Phase::Chat) {
                                    None
                                } else {
                                    Some(Phase::Chat)
                                });
                            }
                            if ui
                                .selectable_label(self.want == Some(Phase::Apply), "📨 Отклики")
                                .on_hover_text("Автопилот: разбирать очередь скана — откликаться")
                                .clicked()
                            {
                                new_want = Some(if self.want == Some(Phase::Apply) {
                                    None
                                } else {
                                    Some(Phase::Apply)
                                });
                            }
                            // Справа: «Выключить» (гасит фазу) и «Пауза» (заморозить/
                            // продолжить на месте). В right_to_left первым идёт правый край.
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
                                        .add_enabled(self.pilot.is_some(), egui::Button::new(label))
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
                            let on = self.pilot_notify_on;
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
                                    self.pilot_notify_on = !on;
                                }
                            }
                        });
                        // Строгость откликов: порог релевантности вакансии к резюме.
                        // «Любые» — весь пул по очереди похожести; «Строго/Средне» —
                        // откликаться только на достаточно близкое. Уходит автопилоту как
                        // MIN_SIMILARITY; смена на ходу перезапускает фазу откликов.
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("🎯 Отклик на:").size(11.0))
                                .on_hover_text(
                                    "Порог соответствия вакансии твоему резюме (косинус \
                                     эмбеддингов). Ниже порога — не откликаемся.",
                                );
                            for (key, label, thr) in PILOT_STRICTNESS {
                                let active = self.pilot_strictness == *key;
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
                            // Старт/стоп откликов прямо в ряду строгости: «выбрал порог →
                            // нажал» одним жестом. Это та же фаза, что и «📨 Отклики» сверху
                            // (обе подсвечиваются вместе). Запускает разбор пула по
                            // соответствию резюме с выбранным порогом.
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let applying = self.want == Some(Phase::Apply);
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
                        // Скан по группам: своя кнопка-тумблер на каждую группу поиска
                        // (из scan.json). В скобках — число новых (ещё не отработанных)
                        // вакансий в очереди. Клик запускает скан группы, повторный —
                        // останавливает; скан гаснет и сам, дойдя до конца выдачи.
                        if let Some(scan) = &self.pilot_scan {
                            if !scan.groups.is_empty() {
                                ui.add_space(2.0);
                                // «Все группы» — один процесс сканирует группы по очереди.
                                let total: i64 = scan.groups.iter().map(|g| g.pending).sum();
                                if ui
                                    .selectable_label(
                                        self.want == Some(Phase::ScanAll),
                                        format!("🔎 Все группы ({total})"),
                                    )
                                    .on_hover_text(
                                        "Сканировать все группы подряд в очередь \
                                         (повторно — стоп)",
                                    )
                                    .clicked()
                                {
                                    new_want = Some(if self.want == Some(Phase::ScanAll) {
                                        None
                                    } else {
                                        Some(Phase::ScanAll)
                                    });
                                }
                                ui.horizontal_wrapped(|ui| {
                                    for g in &scan.groups {
                                        let active =
                                            self.want == Some(Phase::Scan(g.name.clone()));
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
                        // Дообогащение пула: открыть необогащённые вакансии и сохранить
                        // полное описание + дату публикации + вектор (для точного подбора
                        // под резюме). Разовая фаза-тумблер; пока идёт — спиннер загрузки.
                        // Число в скобках — остаток необогащённых (из scan.json).
                        if let Some(scan) = &self.pilot_scan {
                            let enrich_active = self.want == Some(Phase::Enrich);
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
                                    // Индикатор загрузки, пока обогащение идёт (Spinner сам
                                    // просит перерисовку — статус-лог ниже тикает вживую).
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
                        // Счётчики: сколько откликов и обработанных чатов (из stats.json).
                        if let Some(s) = &self.pilot_stats {
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
                    if let Some(p) = new_profile {
                        self.pilot_profile = p;
                        // Счётчики откликов свои у каждого профиля — сбрасываем кэш, чтобы
                        // maybe_reload перечитал stats нового аккаунта. Пул (scan.json)
                        // общий, но mtime тоже сбросим — перечитается при следующем кадре.
                        self.pilot_scan_mtime = None;
                        self.pilot_stats = None;
                        self.pilot_stats_mtime = None;
                        // Если автопилот запущен — перезапустить под новый аккаунт.
                        if self.want.is_some() {
                            self.reconcile_pilot();
                        }
                    }
                    if let Some(s) = new_strictness {
                        self.pilot_strictness = s;
                        // Порог читается автопилотом при старте. Если сейчас крутятся
                        // отклики — перезапустить с новым MIN_SIMILARITY; иначе применится
                        // при следующем запуске фазы откликов.
                        if self.want == Some(Phase::Apply) {
                            self.reconcile_pilot();
                        }
                    }
                    if let Some(w) = new_want {
                        self.want = w;
                        self.reconcile_pilot();
                    }
                    if toggle_pause {
                        if let Some(p) = self.pilot.as_mut() {
                            if p.is_paused() {
                                p.resume();
                            } else {
                                p.pause();
                            }
                        }
                    }
                }

                if !self.metrics.items.is_empty() || self.metrics.title.is_none() {
                    section(ui, "📊 Показатели", |ui| {
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
                }

                // Осциллограммы активных каналов (у каждого — своя подпись и цвет).
                if self.mic.is_some() || self.zoom.is_some() {
                    section(ui, "📈 Осциллограммы", |ui| {
                        if let Some(mon) = &self.mic {
                            mon.snapshot(&mut self.scope);
                            let color = egui::Color32::from_rgb(120, 210, 150);
                            ui.label(egui::RichText::new("🎤 Микрофон").size(11.0).color(color));
                            draw_scope(ui, &self.scope, color);
                            draw_transcript(ui, mon.transcript(), color);
                        }
                        if let Some(mon) = &self.zoom {
                            mon.snapshot(&mut self.scope);
                            let color = egui::Color32::from_rgb(130, 180, 250);
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new("🔊 Zoom/Телемост").size(11.0).color(color),
                            );
                            draw_scope(ui, &self.scope, color);
                            draw_transcript(ui, mon.transcript(), color);
                        }
                    });
                }

                // Ручка ресайза в правом-нижнем углу: тянешь — композитор растягивает окно.
                draw_resize_grip(ui, ctx, grip_rect);

                // Желаемый размер контента (может превышать окно, если что-то не влезло).
                ui.min_rect().size()
            });

            // Авто-подгон: окно не меньше своего контента, чтобы ничего не обрезалось.
            // Растём при нехватке места; вручную увеличенный размер сохраняем (не ужимаем).
            let content = inner.inner + egui::vec2(2.0 * MARGIN, 2.0 * MARGIN);
            let cur = ctx.screen_rect().size();
            let target = egui::vec2(content.x.max(cur.x), content.y.max(cur.y));
            if (target.x - cur.x).abs() > 0.5 || (target.y - cur.y).abs() > 0.5 {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target));
            }
        }

        // Сохранение состояния с дебаунсом: пишем на диск, только когда значения устоялись
        // (≥700мс без изменений) и отличаются от записанного — не дёргаем ФС во время drag/resize.
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

        // Осциллографу нужен плавный кадр (~30fps); иначе редкий тик (500мс) обслуживает и
        // авто-reload, и опрос автопилота (обновить статус, поймать его завершение).
        let scope_active = self.mic.is_some() || self.zoom.is_some();
        let interval = if want_visible && scope_active { 33 } else { 500 };
        ctx.request_repaint_after(Duration::from_millis(interval));
    }
}

/// Блок виджета: рамка с бордером, заголовком-подписью и внутренними отступами.
/// Раскладывает содержимое на всю ширину панели, чтобы рамки блоков были одинаковыми.
fn section<R>(
    ui: &mut egui::Ui,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Frame::default()
        .fill(egui::Color32::from_rgba_unmultiplied(255, 255, 255, 6))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(58, 63, 78)))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                egui::RichText::new(title)
                    .size(10.5)
                    .strong()
                    .color(egui::Color32::from_rgb(120, 130, 150)),
            );
            ui.add_space(4.0);
            add_contents(ui)
        })
        .inner
}

/// Подпись селектора: описание выбранного устройства/потока либо дефолтная подпись.
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

/// Нарисовать осциллограмму сэмплов в строку фиксированной высоты заданным цветом.
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
    // Осевая линия для наглядности тишины.
    painter.line_segment(
        [egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)],
        egui::Stroke::new(0.5, egui::Color32::from_rgb(40, 44, 52)),
    );
}

/// Показать онлайн-транскрипцию под осциллографом канала: накопленный текст обычным
/// цветом канала, текущую (незавершённую) гипотезу — приглушённо и курсивом.
/// `data` = None — распознавание для канала не запущено (нет venv/модели) → ничего не рисуем.
fn draw_transcript(ui: &mut egui::Ui, data: Option<(String, String)>, color: egui::Color32) {
    let (finals, partial) = match data {
        Some(t) => t,
        None => return,
    };
    if finals.is_empty() && partial.is_empty() {
        // Пока тишина/прогрев — тонкая подсказка, чтобы канал не выглядел «сломанным».
        ui.label(
            egui::RichText::new("… слушаю")
                .size(11.0)
                .italics()
                .color(egui::Color32::from_rgb(90, 96, 108)),
        );
        return;
    }
    ui.add_space(2.0);
    // Одна переносимая по словам строка: финальный текст + серым «хвост» гипотезы.
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    let base = egui::TextFormat {
        font_id: egui::FontId::proportional(12.0),
        color,
        ..Default::default()
    };
    if !finals.is_empty() {
        job.append(&finals, 0.0, base.clone());
    }
    if !partial.is_empty() {
        let sep = if finals.is_empty() { "" } else { " " };
        job.append(
            &format!("{sep}{partial}"),
            0.0,
            egui::TextFormat {
                italics: true,
                color: egui::Color32::from_rgb(140, 146, 158),
                ..base
            },
        );
    }
    ui.label(job);
}

/// Нарисовать ручку ресайза в правом-нижнем углу панели и обработать перетаскивание.
/// На Wayland клиент не задаёт свой размер сам — просим композитор начать интерактивный
/// resize в направлении SouthEast.
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

    // Три диагональные риски — привычный вид «уголка ресайза».
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
    // Диагностика: `health-widget --check-capture` печатает, видит ли детектор активный
    // захват экрана прямо сейчас, и выходит (0 = захват идёт, 1 = нет). Удобно проверить
    // авто-скрытие со своим инструментом созвона до боевого звонка (см. README «Проверка»).
    if matches!(std::env::args().nth(1).as_deref(), Some("--version" | "-V")) {
        println!(
            "health-widget v{} (commit {}, сборка {})",
            env!("CARGO_PKG_VERSION"),
            env!("GIT_HASH"),
            env!("BUILD_TIME"),
        );
        std::process::exit(0);
    }

    // Выгрузка полной транскрипции: `--transcript` (всё) или `--transcript-today`
    // (только сегодня) — печатает хронологически «[время] канал: текст» и выходит.
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

    // Список записанных колов с дорожками: `--calls`.
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

    // Экспорт кола: `--export <id> [папка]` — обе дорожки + transcript.txt в
    // <папка>/<id>-<название>/. По умолчанию папка = ./tmp (временная папка проекта).
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

    let cfg = Config::load();
    let st = state::load();

    let shared = Arc::new(Shared {
        user_visible: AtomicBool::new(true),
        sharing_active: AtomicBool::new(false),
        shutdown: AtomicBool::new(false),
        pos: std::sync::Mutex::new(st.x.zip(st.y).map(|(x, y)| (x as i32, y as i32))),
    });

    // Стартовая геометрия из сохранённого состояния (иначе — из конфига/дефолтов).
    let size = [st.width.unwrap_or(cfg.width), st.height.unwrap_or(cfg.height)];
    let pos = [st.x.unwrap_or(cfg.x), st.y.unwrap_or(cfg.y)];

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
