use std::time::Duration;

use crate::theme::Palette;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mood {
    Idle,
    Reading,
    Thinking,
    Sending,
    Waiting,
    Happy,
    Alarm,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Remark {
    pub mood: Mood,
    pub text: String,
}

impl Mood {
    fn color(self, pal: &Palette) -> egui::Color32 {
        match self {
            Mood::Idle => pal.dim,
            Mood::Reading => pal.info,
            Mood::Thinking => pal.accent,
            Mood::Sending => pal.info,
            Mood::Waiting => pal.warn,
            Mood::Happy => pal.ok,
            Mood::Alarm => pal.err,
        }
    }

    fn talks(self) -> bool {
        !matches!(self, Mood::Idle | Mood::Waiting)
    }

    fn icon(self) -> &'static str {
        match self {
            Mood::Idle => "💤",
            Mood::Reading => "📄",
            Mood::Thinking => "💭",
            Mood::Sending => "📨",
            Mood::Waiting => "⏳",
            Mood::Happy => "✅",
            Mood::Alarm => "⚠",
        }
    }
}

fn is_icon(c: char) -> bool {
    (c as u32) > 0x2000 && !c.is_alphanumeric() && !c.is_whitespace() && c != '«' && c != '—'
}

fn without_lead_icon(text: &str) -> String {
    let mut rest = text.trim_start();
    loop {
        let mut chars = rest.chars();
        match chars.next() {
            Some(c) if is_icon(c) => rest = chars.as_str().trim_start(),
            _ => return rest.to_string(),
        }
    }
}

fn renderable(ui: &egui::Ui, font: &egui::FontId, text: &str) -> String {
    ui.fonts(|f| {
        text.chars()
            .filter(|c| c.is_whitespace() || f.has_glyphs(font, c.encode_utf8(&mut [0u8; 4])))
            .collect()
    })
}

fn with_icon(ui: &egui::Ui, font: &egui::FontId, mood: Mood, text: &str) -> String {
    let icon = mood.icon();
    let icon = if renderable(ui, font, icon).is_empty() {
        "•"
    } else {
        icon
    };
    format!("{icon} {}", renderable(ui, font, &without_lead_icon(text)))
}

pub fn remark(line: &str) -> Remark {
    let l = line.to_lowercase();
    let say = |mood: Mood, text: &str| Remark { mood, text: text.to_string() };
    if l.contains("капч") {
        return say(Mood::Alarm, "Капча! Реши её в браузере — я жду");
    }
    if l.contains("пауза") || l.contains("сплю") {
        return say(Mood::Waiting, &strip_marks(line));
    }
    if l.contains("открываю вакансию") {
        return say(Mood::Reading, &after_colon(line, "Читаю вакансию"));
    }
    if l.contains("открываю форму") {
        return say(Mood::Reading, &after_colon(line, "Открываю форму отклика"));
    }
    if l.contains("пишу письмо") {
        return say(Mood::Thinking, &after_colon(line, "Сочиняю письмо"));
    }
    if l.contains("правлю письмо") {
        return say(Mood::Thinking, &after_colon(line, "Причёсываю письмо"));
    }
    if l.contains("вопрос") {
        return say(Mood::Thinking, "Отвечаю на вопросы работодателя");
    }
    if l.contains("отправляю отклик") {
        return say(Mood::Sending, &after_colon(line, "Отправляю отклик"));
    }
    if l.contains("откликнулся") || l.contains("обогащено") {
        return say(Mood::Happy, &strip_marks(line));
    }
    if l.contains("скан") || l.contains("выдаче") {
        return say(Mood::Reading, &strip_marks(line));
    }
    if l.contains("лимит") || l.contains("недоступна") || l.contains("пропускаю") {
        return say(Mood::Waiting, &strip_marks(line));
    }
    if l.contains("ошибка") || l.contains("не удалось") {
        return say(Mood::Alarm, &strip_marks(line));
    }
    if line.trim().is_empty() {
        return say(Mood::Idle, "Стою без дела");
    }
    say(Mood::Thinking, &strip_marks(line))
}

fn after_colon(line: &str, prefix: &str) -> String {
    match line.split_once(": ") {
        Some((_, what)) if !what.trim().is_empty() => format!("{prefix}: {}", what.trim()),
        _ => prefix.to_string(),
    }
}

fn strip_marks(line: &str) -> String {
    line.trim().to_string()
}

pub fn human_gap(minutes: i64) -> String {
    let m = minutes.max(0);
    if m < 60 {
        format!("{m}м")
    } else if m % 60 == 0 {
        format!("{}ч", m / 60)
    } else {
        format!("{}ч{:02}м", m / 60, m % 60)
    }
}

pub fn human_age(age: Duration) -> String {
    let s = age.as_secs();
    if s < 60 {
        format!("{s}с")
    } else {
        format!("{}м{:02}с", s / 60, s % 60)
    }
}

pub fn draw(ui: &mut egui::Ui, pal: &Palette, r: &Remark, age: Duration, raw: &str) {
    const HEAD_W: f32 = 42.0;
    const HEAD_H: f32 = 38.0;
    const GAP: f32 = 6.0;
    const PAD: f32 = 6.0;

    let color = r.mood.color(pal);
    let avail = ui.available_width().max(HEAD_W + GAP + 60.0);
    let bubble_w = avail - HEAD_W - GAP;
    let font = egui::FontId::proportional(10.0);
    let mut text = with_icon(ui, &font, r.mood, &r.text);
    if age.as_secs() >= 3 {
        text.push_str(&format!("  · {}", human_age(age)));
    }
    let galley = ui.fonts(|f| f.layout(text, font, pal.text, bubble_w - PAD * 2.0));
    let h = (galley.size().y + PAD * 2.0).max(HEAD_H);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(avail, h), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let t = ui.input(|i| i.time);

    let head = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.center().y - HEAD_H / 2.0),
        egui::vec2(HEAD_W, HEAD_H),
    );
    painter.rect_filled(head, 9.0, pal.card);
    painter.rect_stroke(head, 9.0, egui::Stroke::new(1.0, color), egui::StrokeKind::Inside);

    let ant_base = egui::pos2(head.center().x, head.top());
    let ant_top = egui::pos2(head.center().x, head.top() - 5.0);
    painter.line_segment([ant_base, ant_top], egui::Stroke::new(1.0, color));
    let blink = ((t * 2.2).sin() * 0.5 + 0.5) as f32;
    painter.circle_filled(ant_top, 2.2, color.gamma_multiply(0.35 + 0.65 * blink));

    let eye_y = head.top() + 14.0;
    let eye_dx = 8.5;
    let shut = (t % 3.4) < 0.12;
    for dx in [-eye_dx, eye_dx] {
        let c = egui::pos2(head.center().x + dx, eye_y);
        match r.mood {
            Mood::Alarm => {
                painter.line_segment(
                    [c + egui::vec2(-3.0, -3.0), c + egui::vec2(3.0, 3.0)],
                    egui::Stroke::new(1.4, color),
                );
                painter.line_segment(
                    [c + egui::vec2(3.0, -3.0), c + egui::vec2(-3.0, 3.0)],
                    egui::Stroke::new(1.4, color),
                );
            }
            Mood::Happy => {
                painter.line_segment(
                    [c + egui::vec2(-3.2, 1.0), c + egui::vec2(0.0, -2.4)],
                    egui::Stroke::new(1.4, color),
                );
                painter.line_segment(
                    [c + egui::vec2(0.0, -2.4), c + egui::vec2(3.2, 1.0)],
                    egui::Stroke::new(1.4, color),
                );
            }
            _ if shut => {
                painter.line_segment(
                    [c + egui::vec2(-3.0, 0.0), c + egui::vec2(3.0, 0.0)],
                    egui::Stroke::new(1.4, color),
                );
            }
            Mood::Thinking => {
                let look = ((t * 1.3).sin() * 1.6) as f32;
                painter.circle_filled(c, 3.0, pal.dim.gamma_multiply(0.5));
                painter.circle_filled(c + egui::vec2(look, -1.0), 1.6, color);
            }
            _ => {
                painter.circle_filled(c, 2.6, color);
            }
        }
    }

    let mouth_y = head.bottom() - 10.0;
    match r.mood {
        Mood::Happy => {
            let c = egui::pos2(head.center().x, mouth_y - 1.0);
            painter.line_segment(
                [c + egui::vec2(-6.0, 0.0), c + egui::vec2(-2.0, 3.0)],
                egui::Stroke::new(1.6, color),
            );
            painter.line_segment(
                [c + egui::vec2(-2.0, 3.0), c + egui::vec2(2.0, 3.0)],
                egui::Stroke::new(1.6, color),
            );
            painter.line_segment(
                [c + egui::vec2(2.0, 3.0), c + egui::vec2(6.0, 0.0)],
                egui::Stroke::new(1.6, color),
            );
        }
        Mood::Waiting => {
            painter.line_segment(
                [
                    egui::pos2(head.center().x - 5.0, mouth_y),
                    egui::pos2(head.center().x + 5.0, mouth_y),
                ],
                egui::Stroke::new(1.6, color),
            );
        }
        _ => {
            let open = if r.mood.talks() {
                2.0 + 3.5 * ((t * 7.0).sin().abs() as f32)
            } else {
                2.0
            };
            let m = egui::Rect::from_center_size(
                egui::pos2(head.center().x, mouth_y),
                egui::vec2(13.0, open),
            );
            painter.rect_filled(m, 2.0, color);
        }
    }

    let bubble = egui::Rect::from_min_size(
        egui::pos2(rect.left() + HEAD_W + GAP, rect.top()),
        egui::vec2(bubble_w, h),
    );
    painter.rect_filled(bubble, 7.0, pal.card);
    painter.rect_stroke(bubble, 7.0, egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(bubble.left(), bubble.center().y - 4.0),
            egui::pos2(bubble.left() - GAP + 1.0, bubble.center().y),
            egui::pos2(bubble.left(), bubble.center().y + 4.0),
        ],
        pal.card,
        egui::Stroke::new(1.0, pal.border),
    ));
    painter.galley(
        egui::pos2(bubble.left() + PAD, bubble.center().y - galley.size().y / 2.0),
        galley,
        pal.text,
    );

    if r.mood.talks() {
        ui.ctx().request_repaint_after(Duration::from_millis(80));
    }
    if !raw.is_empty() {
        resp.on_hover_text(raw);
    }
}

pub fn draw_history(ui: &mut egui::Ui, pal: &Palette, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    ui.add_space(3.0);
    let font = egui::FontId::proportional(9.0);
    for (i, line) in lines.iter().enumerate() {
        let r = remark(line);
        let fade = 1.0 - 0.09 * i as f32;
        let icon = if renderable(ui, &font, r.mood.icon()).is_empty() {
            "•".to_string()
        } else {
            r.mood.icon().to_string()
        };
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = ui.available_width();
        job.append(
            &format!("{icon} "),
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: r.mood.color(pal).gamma_multiply(fade),
                ..Default::default()
            },
        );
        job.append(
            &renderable(ui, &font, &without_lead_icon(&r.text)),
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: pal.muted.gamma_multiply(fade),
                ..Default::default()
            },
        );
        ui.label(job).on_hover_text(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_steps_to_moods() {
        assert_eq!(remark("🤖 Пишу письмо (LLM): Go dev").mood, Mood::Thinking);
        assert_eq!(remark("📄 Открываю вакансию: Go dev").mood, Mood::Reading);
        assert_eq!(
            remark("⏳ Пауза 42с перед следующей вакансией (анти-бот)").mood,
            Mood::Waiting
        );
        assert_eq!(
            remark("😴 Проход чата закончен (обработано 1). Сплю 30м до следующего.").mood,
            Mood::Waiting
        );
        assert_eq!(remark("✅ Откликнулся: Go dev — https://hh.ru/x").mood, Mood::Happy);
        assert_eq!(
            remark("🔒 hh показал капчу — реши её в открытой вкладке, жду тебя…").mood,
            Mood::Alarm
        );
        assert_eq!(remark("").mood, Mood::Idle);
    }

    #[test]
    fn keeps_vacancy_title_in_remark() {
        assert_eq!(
            remark("📄 Открываю вакансию: Go dev").text,
            "Читаю вакансию: Go dev"
        );
        assert_eq!(remark("📝 Открываю форму отклика").text, "Открываю форму отклика");
    }

    #[test]
    fn strips_leading_icons_only() {
        assert_eq!(without_lead_icon("🚪 Отказ — вышел из чата"), "Отказ — вышел из чата");
        assert_eq!(without_lead_icon("✅ Откликнулся: Go dev"), "Откликнулся: Go dev");
        assert_eq!(without_lead_icon("hh: вышел из чата"), "hh: вышел из чата");
        assert_eq!(
            without_lead_icon("Чат [все ≤2д]: просмотрено 32"),
            "Чат [все ≤2д]: просмотрено 32"
        );
        assert_eq!(without_lead_icon("→ score=0.712: Go dev"), "score=0.712: Go dev");
    }

    #[test]
    fn human_gap_formats_hours() {
        assert_eq!(human_gap(45), "45м");
        assert_eq!(human_gap(360), "6ч");
        assert_eq!(human_gap(95), "1ч35м");
    }

    #[test]
    fn human_age_formats_minutes() {
        assert_eq!(human_age(Duration::from_secs(9)), "9с");
        assert_eq!(human_age(Duration::from_secs(75)), "1м15с");
    }
}
