use egui::{Color32, RichText};

pub enum Role {
    Me,
    Bot,
}

pub struct Msg {
    pub role: Role,
    pub text: String,
}

#[derive(Default)]
pub struct Chat {
    messages: Vec<Msg>,
    pending: bool,
    input: String,
}

impl Chat {
    pub fn push_user(&mut self, text: String) {
        self.messages.push(Msg {
            role: Role::Me,
            text,
        });
    }

    pub fn push_bot(&mut self, text: String) {
        self.messages.push(Msg {
            role: Role::Bot,
            text,
        });
    }

    pub fn set_pending(&mut self, pending: bool) {
        self.pending = pending;
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.pending = false;
        self.input.clear();
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        if self.messages.is_empty() && !self.pending {
            ui.label(
                RichText::new("нет сообщений")
                    .size(11.0)
                    .italics()
                    .color(Color32::from_rgb(90, 96, 108)),
            );
        } else {
            egui::ScrollArea::vertical()
                .id_salt("chat_log")
                .max_height(360.0)
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    for msg in &self.messages {
                        let (who, color) = match msg.role {
                            Role::Me => ("я", Color32::from_rgb(120, 210, 150)),
                            Role::Bot => ("бот", Color32::from_rgb(130, 180, 250)),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(RichText::new(format!("{who}:")).size(17.0).strong().color(color));
                            ui.label(
                                RichText::new(&msg.text)
                                    .size(17.0)
                                    .color(Color32::from_rgb(205, 210, 220)),
                            );
                        });
                    }
                    if self.pending {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(12.0));
                            ui.label(
                                RichText::new("думаю…")
                                    .size(11.0)
                                    .color(Color32::from_rgb(140, 146, 158)),
                            );
                        });
                    }
                });
        }
        self.input_row(ui)
    }

    fn input_row(&mut self, ui: &mut egui::Ui) -> Option<String> {
        ui.add_space(4.0);
        let mut clicked = false;
        let mut resp = None;
        ui.horizontal(|ui| {
            let clicked_btn = ui.button(RichText::new("➤").size(17.0)).clicked();
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text("спросить…")
                    .desired_width(f32::INFINITY),
            );
            clicked = clicked_btn;
            resp = Some(edit);
        });
        let resp = resp.unwrap();
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if !enter && !clicked {
            return None;
        }
        resp.request_focus();
        let q = self.input.trim().to_string();
        self.input.clear();
        if q.is_empty() {
            None
        } else {
            Some(q)
        }
    }
}
