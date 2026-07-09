
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone)]
pub struct MouthBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone)]
pub struct AvatarCfg {
    pub svg_path: PathBuf,
    pub device: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub mouth: MouthBox,
    pub scope_color: [u8; 3],
    pub scope_gain: f32,
}

impl Default for AvatarCfg {
    fn default() -> Self {
        let svg_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Pictures")
            .join("panda.svg");
        Self {
            svg_path,
            device: PathBuf::from("/dev/video10"),
            width: 640,
            height: 480,
            fps: 30,
            mouth: MouthBox { x: 150, y: 330, w: 340, h: 90 },
            scope_color: [220, 30, 20],
            scope_gain: 6.0,
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub json_path: PathBuf,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub bg_alpha: u8,
    pub click_through: bool,
    pub auto_hide_on_share: bool,
    pub detect_poll: Duration,
    pub autopilot_dir: PathBuf,
    pub autopilot_bin: PathBuf,
    pub avatar: AvatarCfg,
}

impl Default for Config {
    fn default() -> Self {
        let json_path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("health-widget")
            .join("metrics.json");
        let autopilot_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("projects")
            .join("work-autopilot");
        let autopilot_bin = autopilot_dir.join(".venv").join("bin").join("autopilot");
        Self {
            json_path,
            x: 40.0,
            y: 60.0,
            width: 260.0,
            height: 200.0,
            bg_alpha: 220,
            click_through: false,
            auto_hide_on_share: false,
            detect_poll: Duration::from_millis(1000),
            autopilot_dir,
            autopilot_bin,
            avatar: AvatarCfg::default(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config::default();
        if let Ok(v) = std::env::var("HEALTH_JSON") {
            c.json_path = PathBuf::from(v);
        }
        if let Some(v) = env_f32("HEALTH_X") {
            c.x = v;
        }
        if let Some(v) = env_f32("HEALTH_Y") {
            c.y = v;
        }
        if let Some(v) = env_f32("HEALTH_W") {
            c.width = v;
        }
        if let Some(v) = env_f32("HEALTH_H") {
            c.height = v;
        }
        if let Some(v) = env_f32("HEALTH_BG_ALPHA") {
            c.bg_alpha = v.clamp(0.0, 255.0) as u8;
        }
        if let Some(v) = env_bool("HEALTH_CLICK_THROUGH") {
            c.click_through = v;
        }
        if let Some(v) = env_bool("HEALTH_AUTO_HIDE") {
            c.auto_hide_on_share = v;
        }
        if let Ok(v) = std::env::var("HEALTH_AUTOPILOT_DIR") {
            c.autopilot_dir = PathBuf::from(v);
            c.autopilot_bin = c.autopilot_dir.join(".venv").join("bin").join("autopilot");
        }
        if let Ok(v) = std::env::var("HEALTH_AUTOPILOT_BIN") {
            c.autopilot_bin = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("HEALTH_AVATAR_SVG") {
            c.avatar.svg_path = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("HEALTH_AVATAR_DEVICE") {
            c.avatar.device = PathBuf::from(v);
        }
        if let Some(v) = env_f32("HEALTH_AVATAR_GAIN") {
            c.avatar.scope_gain = v;
        }
        c
    }
}

fn env_f32(k: &str) -> Option<f32> {
    std::env::var(k).ok()?.trim().parse().ok()
}

fn env_bool(k: &str) -> Option<bool> {
    let v = std::env::var(k).ok()?;
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatar_defaults_are_sane() {
        let c = AvatarCfg::default();
        assert_eq!(c.device, std::path::PathBuf::from("/dev/video10"));
        assert_eq!(c.width, 640);
        assert_eq!(c.height, 480);
        assert_eq!(c.fps, 30);
        assert_eq!(c.scope_color, [220, 30, 20]);
        assert!(c.mouth.w > 0 && c.mouth.h > 0);
        assert!(c.mouth.x + c.mouth.w <= c.width);
        assert!(c.mouth.y + c.mouth.h <= c.height);
    }
}
