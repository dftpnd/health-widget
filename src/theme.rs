use egui::Color32;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Dark,
    Light,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub border: Color32,
    pub grid: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub dim: Color32,
    pub ok: Color32,
    pub info: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub focus: Color32,
}

impl Theme {
    pub fn toggled(self) -> Self {
        match self {
            Theme::Dark => Theme::Light,
            Theme::Light => Theme::Dark,
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Theme::Dark => "🌞",
            Theme::Light => "🌙",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Theme::Dark => "Светлая тема",
            Theme::Light => "Тёмная тема",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "dark",
            Theme::Light => "light",
        }
    }

    pub fn from_label(s: &str) -> Self {
        match s {
            "light" => Theme::Light,
            _ => Theme::Dark,
        }
    }

    pub fn egui_visuals(self) -> egui::Visuals {
        match self {
            Theme::Dark => egui::Visuals::dark(),
            Theme::Light => egui::Visuals::light(),
        }
    }

    pub fn palette(self, bg_alpha: u8) -> Palette {
        match self {
            Theme::Dark => Palette {
                bg: Color32::from_rgba_unmultiplied(18, 18, 22, bg_alpha),
                panel: Color32::from_rgb(10, 12, 16),
                card: Color32::from_rgba_unmultiplied(255, 255, 255, 6),
                border: Color32::from_rgb(58, 63, 78),
                grid: Color32::from_rgb(40, 44, 52),
                accent: Color32::from_rgb(180, 200, 255),
                text: Color32::from_rgb(205, 210, 220),
                muted: Color32::from_rgb(135, 141, 152),
                dim: Color32::from_rgb(90, 96, 108),
                ok: Color32::from_rgb(120, 210, 150),
                info: Color32::from_rgb(130, 180, 250),
                warn: Color32::from_rgb(210, 200, 120),
                err: Color32::from_rgb(230, 120, 120),
                focus: Color32::from_rgb(26, 115, 232),
            },
            Theme::Light => Palette {
                bg: Color32::from_rgba_unmultiplied(245, 246, 248, bg_alpha),
                panel: Color32::from_rgb(226, 229, 234),
                card: Color32::from_rgba_unmultiplied(0, 0, 0, 12),
                border: Color32::from_rgb(196, 202, 212),
                grid: Color32::from_rgb(206, 211, 219),
                accent: Color32::from_rgb(38, 90, 200),
                text: Color32::from_rgb(28, 32, 40),
                muted: Color32::from_rgb(92, 102, 116),
                dim: Color32::from_rgb(140, 148, 160),
                ok: Color32::from_rgb(30, 140, 72),
                info: Color32::from_rgb(40, 108, 210),
                warn: Color32::from_rgb(158, 120, 12),
                err: Color32::from_rgb(200, 56, 56),
                focus: Color32::from_rgb(26, 115, 232),
            },
        }
    }
}
