
use std::sync::mpsc::Receiver;

use egui_term::{BackendSettings, PtyEvent, TerminalBackend, TerminalView};

pub struct Terminal {
    backend: TerminalBackend,
    pty_rx: Receiver<(u64, PtyEvent)>,
}

impl Terminal {
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
