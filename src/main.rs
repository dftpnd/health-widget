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

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod audio;
mod chat;
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
    /// Состояние генерации ответа рекрутёру («Ответить HR»): фоновый поток пишет,
    /// UI читает каждый кадр и кладёт готовый ответ в буфер обмена.
    hr_reply: Arc<std::sync::Mutex<hr_reply::HrReplyState>>,
    /// Последнее сохранённое на диск состояние (чтобы не перезаписывать без изменений).
    last_saved: state::State,
    /// Состояние предыдущего кадра + момент его появления — для дебаунса записи.
    prev_state: state::State,
    stable_since: Instant,
    /// Статус последнего снимка области экрана (кнопка «Скрин»). Фоновый поток
    /// грабера пишет сюда по завершении, UI читает под кнопкой каждый кадр.
    shot_status: Arc<std::sync::Mutex<screenshot::ShotStatus>>,
    /// Запрос начать разметку области. Ставится кнопкой «Скрин» или SIGUSR2
    /// (кнопка Tartarus); update() на главном потоке подхватывает и открывает
    /// оверлей. Через флаг — потому что сигнал прилетает в другом потоке.
    shot_request: Arc<AtomicBool>,
    /// Оверлей разметки сейчас открыт (ждём два клика).
    shot_active: bool,
    /// Точки-клики оверлея в физических пикселях экрана (нужно ровно две).
    shot_points: Vec<[u32; 2]>,
    /// Счётчики нажатий кнопок-маркеров транскрипции (RT-сигналы от Tartarus: физ. 10=микрофон,
    /// 15=телемост). Сигнальный поток инкрементит, UI сравнивает с *_seen и обрабатывает
    /// каждое новое нажатие как старт/стоп записи участка. Счётчик (а не булев тумблер) —
    /// чтобы не терять нажатия и не зависеть от порядка кадров.
    mark_mic: Arc<AtomicU32>,
    mark_zoom: Arc<AtomicU32>,
    /// Сколько нажатий уже обработано UI-потоком (микрофон/телемост).
    mark_mic_seen: u32,
    mark_zoom_seen: u32,
    /// Маркеры участков транскрипции (микрофон/телемост): завершённые диапазоны + активная запись.
    markers_mic: MarkerState,
    markers_zoom: MarkerState,
}

/// Состояние маркеров транскрипции одного канала. Кнопка стартует запись (запоминаем
/// байтовый офсет конца текста), повторное нажатие — стоп: диапазон уходит в `spans`
/// (подсветка остаётся навсегда), а текст участка копируется в буфер. Пока запись идёт,
/// подсвечивается растущий диапазон `active_start..конец`. Несколько маркеров копятся.
#[derive(Default)]
struct MarkerState {
    /// Завершённые маркеры — байтовые диапазоны [start, end) в накопленном тексте.
    spans: Vec<(usize, usize)>,
    /// Старт текущей записи (байтовый офсет), None — сейчас не пишем.
    active_start: Option<usize>,
}

impl MarkerState {
    /// Обработать одно нажатие кнопки при текущей длине текста `len` (в байтах).
    /// Возвращает диапазон для копирования, если это был «стоп».
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

        // Статус снимка экрана — общий для UI (кнопка «Скрин») и грабера.
        let shot_status: Arc<std::sync::Mutex<screenshot::ShotStatus>> =
            Arc::new(std::sync::Mutex::new(screenshot::ShotStatus::Idle));
        // Запрос разметки области — ставят кнопка «Скрин» и SIGUSR2, читает update().
        let shot_request = Arc::new(AtomicBool::new(false));

        // Счётчики нажатий кнопок-маркеров (RT-сигналы от Tartarus: 10=микрофон, 15=телемост).
        let mark_mic = Arc::new(AtomicU32::new(0));
        let mark_zoom = Arc::new(AtomicU32::new(0));

        // Поток обработки сигналов: SIGUSR1 — тумблер видимости; SIGUSR2 — начать
        // разметку области под снимок (кнопка «Скрин»); SIGRTMIN+0/+1 — тумблеры выделения
        // транскрипции микрофона/телемоста (кнопки Tartarus через remap → pkill --signal).
        {
            let shared = shared.clone();
            let ctx = cc.egui_ctx.clone();
            let shot_request = shot_request.clone();
            let mark_mic = mark_mic.clone();
            let mark_zoom = mark_zoom.clone();
            // SIGRTMIN зависит от glibc (обычно 34); вычисляем один раз, тем же значением
            // Tartarus шлёт `pkill --signal 34/35`.
            let rt_mic = libc::SIGRTMIN();
            let rt_zoom = libc::SIGRTMIN() + 1;
            std::thread::spawn(move || {
                let mut signals = signal_hook::iterator::Signals::new([
                    signal_hook::consts::SIGUSR1,
                    signal_hook::consts::SIGUSR2,
                    rt_mic,
                    rt_zoom,
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
            hr_reply: Arc::new(std::sync::Mutex::new(hr_reply::HrReplyState::Idle)),
            last_saved: st.clone(),
            prev_state: st.clone(),
            stable_since: Instant::now(),
            shot_status,
            shot_request,
            shot_active: false,
            shot_points: Vec::new(),
            mark_mic,
            mark_zoom,
            mark_mic_seen: 0,
            mark_zoom_seen: 0,
            markers_mic: MarkerState::default(),
            markers_zoom: MarkerState::default(),
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

    /// Полноэкранный прозрачный оверлей разметки области под снимок. Ловит два
    /// клика (в физических пикселях экрана), рисует крестики на отмеченном; по
    /// второму клику закрывается и отдаёт прямоугольник граберу. Esc — отмена.
    fn show_shot_overlay(&mut self, ctx: &egui::Context) {
        let vb = egui::ViewportBuilder::default()
            .with_title("health-widget-shot")
            .with_fullscreen(true)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_mouse_passthrough(false);
        let id = egui::ViewportId::from_hash_of("hw-shot-overlay");

        // Итог кадра: None — ещё размечаем; Some(None) — отмена; Some(Some(rect)).
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

            // Полностью прозрачный оверлей: экран выглядит как обычно (без вуали и
            // меток), но окно ловит клики (mouse_passthrough=false). Пустой frame с
            // прозрачной заливкой — на экране ничего не рисуем.
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::default()
                        .inner_margin(egui::Margin::same(0))
                        .fill(egui::Color32::TRANSPARENT),
                )
                .show(octx, |ui| {
                    // Сенс на всю площадь — гарантируем, что окно принимает клики.
                    ui.allocate_response(ui.available_size(), egui::Sense::click());
                });

            if done.is_none() {
                if let Some(pos) = click {
                    // Логические координаты (как геометрия окон KWin) — их ждёт CaptureArea.
                    let px = [pos.x.round().max(0.0) as u32, pos.y.round().max(0.0) as u32];
                    self.shot_points.push(px);
                    if self.shot_points.len() >= 2 {
                        let (a, b) = (self.shot_points[0], self.shot_points[1]);
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
            self.shot_active = false;
            self.shot_points.clear();
            ctx.request_repaint();
            match res {
                Some([x, y, w, h]) => {
                    *self.shot_status.lock().unwrap() = screenshot::ShotStatus::Working;
                    screenshot::grab(x as i32, y as i32, w, h, ctx.clone(), self.shot_status.clone());
                }
                None => *self.shot_status.lock().unwrap() = screenshot::ShotStatus::Cancelled,
            }
        }
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

        // Снимок области: запрос от кнопки «Скрин» или SIGUSR2 (Tartarus) открывает
        // прозрачный оверлей разметки; пока он активен — рисуем его каждый кадр.
        if self.shot_request.swap(false, Ordering::Relaxed) && !self.shot_active {
            self.shot_active = true;
            self.shot_points.clear();
            *self.shot_status.lock().unwrap() = screenshot::ShotStatus::Marking;
        }
        if self.shot_active {
            self.show_shot_overlay(ctx);
        }

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

                // Кол + Скрин в одной линии, по 50% ширины каждому. Слева — запись
                // звонка (две дорожки WAV + транскрипт), справа — снимок области экрана
                // через Spectacle. Каналы могли смениться выше — приводим запись к колу.
                self.reconcile_call_recording();
                let mut call_toggle = false;
                let active_name = self.active_call.as_ref().map(|c| c.name.clone());
                let mut name_buf = self.call_name_input.clone();

                let mut shoot = false;
                let shot_line = {
                    use screenshot::ShotStatus::*;
                    match &*self.shot_status.lock().unwrap() {
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
                                    !self.shot_active,
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
                    self.shot_request.store(true, Ordering::Relaxed);
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
                    section_collapsible(ui, "🤖 Автопилот", |ui| {
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
                        // «Ответить HR»: текст рекрутёра из буфера → LLM (профиль на выбор)
                        // → ответ обратно в буфер. Генерация в фоне, пока идёт — спиннер.
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
                    section_collapsible(ui, "📊 Показатели", |ui| {
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

                // Кнопки-маркеры транскрипции (RT-сигналы Tartarus): обрабатываем каждое новое
                // нажатие как старт/стоп записи участка. На «стоп» — текст участка в буфер обмена;
                // сами диапазоны-маркеры хранятся в markers_* и рисуются в draw_transcript.
                let c_mic = self.mark_mic.load(Ordering::Relaxed);
                let mic_finals = self.mic.as_ref().and_then(|m| m.transcript()).map(|(f, _)| f);
                if let Some(txt) = apply_mark_presses(
                    &mut self.markers_mic,
                    &mut self.mark_mic_seen,
                    c_mic,
                    mic_finals.as_deref(),
                ) {
                    clipboard_set(txt);
                }
                let c_zoom = self.mark_zoom.load(Ordering::Relaxed);
                let zoom_finals = self.zoom.as_ref().and_then(|m| m.transcript()).map(|(f, _)| f);
                if let Some(txt) = apply_mark_presses(
                    &mut self.markers_zoom,
                    &mut self.mark_zoom_seen,
                    c_zoom,
                    zoom_finals.as_deref(),
                ) {
                    clipboard_set(txt);
                }

                // Осциллограммы активных каналов (у каждого — своя подпись и цвет).
                if self.mic.is_some() || self.zoom.is_some() {
                    section(ui, "📈 Осциллограммы", |ui| {
                        if let Some(mon) = &self.mic {
                            mon.snapshot(&mut self.scope);
                            let color = egui::Color32::from_rgb(120, 210, 150);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("🎤 Микрофон").size(11.0).color(color),
                                );
                                marker_recording_badge(ui, &self.markers_mic);
                            });
                            draw_scope(ui, &self.scope, color);
                            draw_transcript(ui, mon.transcript(), color, "mic", &self.markers_mic);
                        }
                        if let Some(mon) = &self.zoom {
                            mon.snapshot(&mut self.scope);
                            let color = egui::Color32::from_rgb(130, 180, 250);
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("🔊 Zoom/Телемост").size(11.0).color(color),
                                );
                                marker_recording_badge(ui, &self.markers_zoom);
                            });
                            draw_scope(ui, &self.scope, color);
                            draw_transcript(ui, mon.transcript(), color, "zoom", &self.markers_zoom);
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
) -> Option<R> {
    section_impl(ui, title, false, add_contents)
}

/// Как [`section`], но с иконкой сворачивания справа от заголовка. Клик по заголовку
/// или иконке скрывает/показывает содержимое; состояние запоминается на время сессии.
fn section_collapsible<R>(
    ui: &mut egui::Ui,
    title: &str,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    section_impl(ui, title, true, add_contents)
}

fn section_impl<R>(
    ui: &mut egui::Ui,
    title: &str,
    collapsible: bool,
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

            if !collapsible {
                ui.label(title_rich);
                ui.add_space(4.0);
                return Some(add_contents(ui));
            }

            let id = ui.make_persistent_id(("section_collapsed", title));
            let mut collapsed = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
            let toggled = ui
                .horizontal(|ui| {
                    let icon = if collapsed { "▸" } else { "▾" };
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
                collapsed = !collapsed;
                ui.data_mut(|d| d.insert_temp(id, collapsed));
            }
            if collapsed {
                None
            } else {
                ui.add_space(4.0);
                Some(add_contents(ui))
            }
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

/// Мигающий индикатор активной записи маркера (кнопка Tartarus нажата один раз, ждём второго).
/// Рисуется в строке заголовка канала, чтобы был виден независимо от прокрутки транскрипта.
/// Пусто, если запись маркера сейчас не идёт.
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

/// Показать онлайн-транскрипцию под осциллографом канала: накопленный текст в
/// прокручиваемой области (остаётся на месте, можно выделить и скопировать), текущую
/// незавершённую гипотезу — приглушённо отдельной строкой. `data` = None — распознавание
/// для канала не запущено (нет venv/модели) → ничего не рисуем. `id_salt` разводит
/// состояние прокрутки/выделения двух каналов (микрофон/Zoom). `markers` — участки,
/// отмеченные кнопкой с макро-клавиатуры (Tartarus): их фон подсвечивается цветом канала.
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
    // Прокручиваемая область фиксированной высоты: держимся низа (следим за новым текстом),
    // но стоит прокрутить вверх для выделения — остаёмся на месте, текст не «уезжает».
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .max_height(140.0)
        .auto_shrink([false, true])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if !finals.is_empty() {
                // Выделяемый текст. По факту read-only: правки в scratch-копию не сохраняем
                // (перезаписываем каждый кадр из finals). Как только выделение завершено
                // (отпустили мышь) — выделенное сразу в буфер обмена, без Ctrl+C.
                // Диапазоны-маркеры (кнопка Tartarus) подсвечиваются фоном через кастомный
                // layouter: завершённые участки + активная запись, растущая до конца текста.
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
            // Текущая (незавершённая) гипотеза — приглушённо, отдельной строкой, не копируем.
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

/// Положить текст в системный буфер обмена через `wl-copy`. Почему не `egui`/`ctx.copy_text`:
/// на Wayland eframe ставит буфер через smithay-clipboard, а тот работает только когда окно
/// в фокусе (нужен input-serial). Копия по кнопке Tartarus прилетает, когда виджет НЕ в фокусе,
/// поэтому egui-копия молча не проходит. `wl-copy` создаёт свой data-source и не зависит от
/// фокуса. Пишем в отдельном потоке, чтобы не блокировать UI (wl-copy форкает демон-держатель).
fn clipboard_set(text: String) {
    std::thread::spawn(move || {
        use std::io::Write;
        use std::process::{Command, Stdio};
        if let Ok(mut child) = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
    });
}

/// Обработать накопившиеся нажатия кнопки-маркера канала (счётчик сигнального потока минус
/// уже учтённые). Каждое нажатие — старт/стоп записи участка; на «стоп» возвращает текст
/// участка для копирования в буфер. `finals` — накопленный текст канала (None — канал выключен).
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
    // Предохранитель от рассинхрона/переполнения: не прокручиваем абсурдное число нажатий.
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

/// Собрать `LayoutJob` для транскрипта: весь текст цветом `color` шрифтом 20, а байтовые
/// диапазоны `ranges` (маркеры + активная запись) — с фоном `hl`. `wrap` — ширина переноса,
/// которую даёт layouter egui. Границы диапазонов снапаются к границам символов (панико-безопасно).
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
    // Диагностика прямого захвата через KWin (см. kwin_shot): печатает, авторизован
    // ли бинарь и что вернул CaptureArea. Выходит.
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

    // Саморегистрация для права на снимок области (KWin CaptureArea) — best-effort.
    screenshot::ensure_registered();

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

#[cfg(test)]
mod marker_tests {
    use super::{apply_mark_presses, MarkerState};

    #[test]
    fn start_then_stop_makes_span() {
        let mut m = MarkerState::default();
        assert_eq!(m.toggle(5), None); // старт
        assert_eq!(m.active_start, Some(5));
        assert_eq!(m.toggle(12), Some((5, 12))); // стоп
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
        assert_eq!(m.toggle(4), None); // стоп на той же позиции — участка нет
        assert!(m.spans.is_empty());
    }

    #[test]
    fn presses_start_grow_stop_copies_grown_slice() {
        let mut m = MarkerState::default();
        let mut seen = 0u32;
        let t1 = "привет"; // 12 байт
        assert_eq!(apply_mark_presses(&mut m, &mut seen, 1, Some(t1)), None);
        assert_eq!(m.active_start, Some(12));
        let t2 = "привет мир"; // 19 байт; участок [12,19) == " мир"
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
        assert_eq!(apply_mark_presses(&mut m, &mut seen, 1, None), None); // старт
        assert_eq!(apply_mark_presses(&mut m, &mut seen, 2, None), None); // стоп на пустом
        assert!(m.spans.is_empty());
    }
}
