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
mod state;
mod winctl;

use config::Config;
use data::Metrics;

/// Размер угловой ручки ресайза (в точках).
const GRIP: f32 = 16.0;
/// Внутренние поля рамки виджета (в точках) — совпадают с Frame::inner_margin.
const MARGIN: f32 = 12.0;

/// Общее состояние между потоками и UI.
struct Shared {
    /// Хочет ли пользователь видеть виджет (тумблер по SIGUSR1).
    user_visible: AtomicBool,
    /// Идёт ли захват экрана (ставит фоновый детектор).
    sharing_active: AtomicBool,
    /// Позиция окна по данным KWin (Wayland не отдаёт её клиенту; опрашивает фоновый поток).
    pos: std::sync::Mutex<Option<(i32, i32)>>,
}

struct App {
    cfg: Config,
    shared: Arc<Shared>,
    metrics: Metrics,
    /// Кэш времени модификации json для авто-reload.
    last_mtime: Option<std::time::SystemTime>,
    /// Текущее фактическое состояние видимости окна (чтобы не слать команду каждый кадр).
    currently_visible: bool,
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

        // Восстанавливаем ранее включённые каналы (микрофон — с сохранённым устройством;
        // программа эфемерна, поэтому zoom-канал стартует с «весь вывод» = monitor по умолчанию).
        let mic = if st.mic_on {
            audio::AudioMonitor::start(st.mic_target.as_deref())
        } else {
            None
        };
        let zoom = if st.zoom_on {
            audio::AudioMonitor::start(audio::default_monitor().as_deref())
        } else {
            None
        };

        Self {
            cfg,
            shared,
            metrics,
            last_mtime,
            currently_visible: true,
            mic,
            zoom,
            mic_target: st.mic_target.clone(),
            prog_target: None,
            mics: audio::list_mics(),
            programs: audio::list_programs(),
            scope: Vec::with_capacity(2048),
            pinned: st.pinned,
            last_saved: st.clone(),
            prev_state: st.clone(),
            stable_since: Instant::now(),
        }
    }

    /// Запустить zoom-канал: выбранная программа (node id) или, если не выбрана, весь вывод.
    fn start_program(&self) -> Option<audio::AudioMonitor> {
        let target = self.prog_target.clone().or_else(audio::default_monitor);
        audio::AudioMonitor::start(target.as_deref())
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
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Полностью прозрачный фон окна — сам виджет рисует свою подложку.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.maybe_reload();

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
                let mut new_mic: Option<Option<String>> = None;
                let mut new_prog: Option<Option<String>> = None;
                let mut refresh = false;

                // Строка 1 — микрофон: тумблер + выбор устройства.
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(mic_on, "🎤")
                        .on_hover_text("Слушать микрофон")
                        .clicked()
                    {
                        toggle_mic = true;
                    }
                    let cur = device_label(&mic_target, &self.mics, "🎤 по умолчанию");
                    egui::ComboBox::from_id_salt("mic-src")
                        .width(150.0)
                        .selected_text(egui::RichText::new(cur).size(11.0))
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(mic_target.is_none(), "🎤 по умолчанию").clicked() {
                                new_mic = Some(None);
                            }
                            for d in &self.mics {
                                let sel = mic_target.as_deref() == Some(d.target.as_str());
                                if ui.selectable_label(sel, &d.label).clicked() {
                                    new_mic = Some(Some(d.target.clone()));
                                }
                            }
                        });
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
                    let cur = device_label(&prog_target, &self.programs, "🔊 весь вывод");
                    egui::ComboBox::from_id_salt("prog-src")
                        .width(150.0)
                        .selected_text(egui::RichText::new(cur).size(11.0))
                        .show_ui(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                                if ui.selectable_label(prog_target.is_none(), "🔊 весь вывод").clicked() {
                                    new_prog = Some(None);
                                }
                                for d in &self.programs {
                                    let sel = prog_target.as_deref() == Some(d.target.as_str());
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

                // Применяем изменения выбора/тумблеров.
                if refresh {
                    self.mics = audio::list_mics();
                    self.programs = audio::list_programs();
                }
                if let Some(sel) = new_mic {
                    self.mic_target = sel;
                    if self.mic.is_some() {
                        self.mic = audio::AudioMonitor::start(self.mic_target.as_deref());
                    }
                }
                if let Some(sel) = new_prog {
                    self.prog_target = sel;
                    if self.zoom.is_some() {
                        self.zoom = self.start_program();
                    }
                }
                if toggle_mic {
                    self.mic = if self.mic.is_some() {
                        None
                    } else {
                        audio::AudioMonitor::start(self.mic_target.as_deref())
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

                for m in &self.metrics.items {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&m.label)
                                .color(egui::Color32::from_rgb(150, 150, 160)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(&m.value)
                                    .strong()
                                    .color(egui::Color32::from_rgb(235, 235, 240)),
                            );
                        });
                    });
                }
                if self.metrics.items.is_empty() && self.metrics.title.is_none() {
                    ui.label(
                        egui::RichText::new(format!("нет данных: {}", self.cfg.json_path.display()))
                            .color(egui::Color32::from_rgb(200, 120, 120)),
                    );
                }

                // Осциллограммы активных каналов (у каждого — своя подпись и цвет).
                if let Some(mon) = &self.mic {
                    mon.snapshot(&mut self.scope);
                    let color = egui::Color32::from_rgb(120, 210, 150);
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("🎤 Микрофон").size(11.0).color(color));
                    draw_scope(ui, &self.scope, color);
                }
                if let Some(mon) = &self.zoom {
                    mon.snapshot(&mut self.scope);
                    let color = egui::Color32::from_rgb(130, 180, 250);
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new("🔊 Zoom/Телемост").size(11.0).color(color));
                    draw_scope(ui, &self.scope, color);
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

        // Осциллографу нужен плавный кадр (~30fps), иначе хватает редкого тика для авто-reload.
        let scope_active = self.mic.is_some() || self.zoom.is_some();
        let interval = if want_visible && scope_active { 33 } else { 500 };
        ctx.request_repaint_after(Duration::from_millis(interval));
    }
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
