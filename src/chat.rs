use crate::theme::Palette;
use egui::RichText;

pub enum Role {
    Me,
    Bot,
    Note,
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
    unstuck: bool,
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

    pub fn push_note(&mut self, text: String) {
        let same_as_last = self
            .messages
            .last()
            .is_some_and(|m| matches!(m.role, Role::Note) && m.text == text);
        if same_as_last {
            return;
        }
        self.messages.push(Msg {
            role: Role::Note,
            text,
        });
    }

    pub fn has_dialogue(&self) -> bool {
        self.messages.iter().any(|m| !matches!(m.role, Role::Note))
    }

    pub fn set_pending(&mut self, pending: bool) {
        self.pending = pending;
    }

    pub fn history(&self) -> Vec<(bool, String)> {
        self.messages
            .iter()
            .filter(|m| !matches!(m.role, Role::Note))
            .map(|m| (matches!(m.role, Role::Me), m.text.clone()))
            .collect()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.pending = false;
        self.input.clear();
    }

    pub fn ui(&mut self, p: Palette, ui: &mut egui::Ui) -> Option<String> {
        self.ui_capped(p, ui, 360.0)
    }

    pub fn ui_capped(&mut self, p: Palette, ui: &mut egui::Ui, max_height: f32) -> Option<String> {
        self.ui_scrolled(p, ui, max_height, 0.0)
    }

    pub fn ui_scrolled(
        &mut self,
        p: Palette,
        ui: &mut egui::Ui,
        max_height: f32,
        wheel: f32,
    ) -> Option<String> {
        if self.messages.is_empty() && !self.pending {
            ui.label(
                RichText::new("нет сообщений")
                    .size(16.0)
                    .italics()
                    .color(p.dim),
            );
        } else {
            if wheel > 0.0 {
                self.unstuck = true;
            }
            let out = egui::ScrollArea::vertical()
                .id_salt("chat_log")
                .max_height(max_height)
                .auto_shrink([false, true])
                .animated(false)
                .stick_to_bottom(!self.unstuck)
                .show(ui, |ui| {
                    if wheel != 0.0 {
                        ui.scroll_with_delta(egui::vec2(0.0, wheel));
                    }
                    ui.set_min_width(ui.available_width());
                    for msg in &self.messages {
                        let (who, color) = match msg.role {
                            Role::Me => ("я", p.ok),
                            Role::Bot => ("бот", p.info),
                            Role::Note => ("виджет", p.err),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(RichText::new(format!("{who}:")).size(22.0).strong().color(color));
                            ui.label(
                                RichText::new(&msg.text)
                                    .size(22.0)
                                    .color(p.text),
                            );
                        });
                    }
                    if self.pending {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(12.0));
                            ui.label(
                                RichText::new("думаю…")
                                    .size(16.0)
                                    .color(p.muted),
                            );
                        });
                    }
                });
            let max = (out.content_size.y - out.inner_rect.height()).max(0.0);
            if out.state.offset.y >= max - 4.0 {
                self.unstuck = false;
            }
        }
        self.input_row(ui)
    }

    fn input_row(&mut self, ui: &mut egui::Ui) -> Option<String> {
        ui.add_space(4.0);
        let mut clicked = false;
        let mut resp = None;
        ui.horizontal(|ui| {
            let clicked_btn = ui.button(RichText::new("▶").size(22.0)).clicked();
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text("спросить…")
                    .font(egui::FontId::proportional(17.5))
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

#[cfg(test)]
mod tests {
    use super::Chat;

    #[test]
    fn notes_stay_out_of_history() {
        let mut chat = Chat::default();
        chat.push_user("привет".into());
        chat.push_note("⚠ нечего отправлять".into());
        chat.push_bot("ответ".into());
        assert_eq!(
            chat.history(),
            vec![(true, "привет".to_string()), (false, "ответ".to_string())]
        );
    }

    #[test]
    fn same_note_twice_pushes_once() {
        let mut chat = Chat::default();
        chat.push_note("⚠ нечего отправлять".into());
        chat.push_note("⚠ нечего отправлять".into());
        assert!(!chat.has_dialogue());
        assert_eq!(chat.messages.len(), 1);
    }

    #[test]
    fn dialogue_seen_only_past_notes() {
        let mut chat = Chat::default();
        chat.push_note("⚠ канал молчит".into());
        assert!(!chat.has_dialogue());
        chat.push_user("вопрос".into());
        assert!(chat.has_dialogue());
    }
}
