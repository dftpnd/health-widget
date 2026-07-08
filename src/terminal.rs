//! Встроенный терминал в боковой колонке виджета. Тонкая обёртка над egui_term:
//! PTY, VT-парсинг, рендер сетки, ввод и ресайз даёт крейт (бэкенд — alacritty_terminal).
//! Держим один shell на время жизни колонки; при выходе shell поднимаем заново при
//! следующей отрисовке. Внутри крейт держит фоновый event-loop PTY и сам просит перерисовку
//! через переданный `egui::Context`, поэтому нам не нужен свой поток чтения.

use std::sync::mpsc::Receiver;

use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

/// Состояние терминала колонки. Владеет бэкендом (PTY + VT-модель крейта) и приёмником
/// событий PTY (нужен, чтобы поймать выход shell и перезапустить).
pub struct Terminal {
    backend: TerminalBackend,
    pty_rx: Receiver<(u64, PtyEvent)>,
}

impl Terminal {
    /// Поднять shell в PTY. Shell из `$SHELL` (иначе `/usr/bin/zsh`), рабочая директория — `$HOME`.
    pub fn new(ctx: &egui::Context) -> Self {
        let (pty_tx, pty_rx) = std::sync::mpsc::channel();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/usr/bin/zsh".to_string());
        let settings = BackendSettings {
            shell,
            working_directory: dirs::home_dir(),
            ..Default::default()
        };
        let backend = TerminalBackend::new(0, ctx.clone(), pty_tx, settings)
            .expect("не удалось поднять PTY терминала");
        Self { backend, pty_rx }
    }

    /// Отрисовать терминал на всю площадь `ui`. Сначала разгребаем события PTY: если shell
    /// вышел — пересоздаём бэкенд (новый shell). Затем добавляем виджет крейта; он сам
    /// обрабатывает ввод, фокус и ресайз под размер `ui`.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut exited = false;
        while let Ok((_id, ev)) = self.pty_rx.try_recv() {
            if matches!(ev, PtyEvent::Exit) {
                exited = true;
            }
        }
        if exited {
            let ctx = ui.ctx().clone();
            *self = Terminal::new(&ctx);
        }
        let view = TerminalView::new(ui, &mut self.backend).set_focus(true);
        ui.add(view);
    }
}
