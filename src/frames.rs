pub const TARGET_OUT_WIDTH: u32 = 2500;

const FALLBACK_W_LG: u32 = 4096;
const FALLBACK_H_LG: u32 = 1728;
const FALLBACK_SCALE: f64 = 1.25;

fn even(v: u32) -> u32 {
    v & !1
}

pub fn band(w_lg: u32, h_lg: u32, target_lg: u32) -> (i32, i32, u32, u32) {
    let w = even(target_lg.min(w_lg));
    let h = even(h_lg);
    let x = even(w_lg.saturating_sub(w) / 2);
    (x as i32, 0, w, h)
}

pub fn geometry() -> (u32, u32, f64) {
    kscreen_logical().unwrap_or((FALLBACK_W_LG, FALLBACK_H_LG, FALLBACK_SCALE))
}

fn kscreen_logical() -> Option<(u32, u32, f64)> {
    let out = std::process::Command::new("kscreen-doctor").arg("-j").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    for o in v.get("outputs")?.as_array()? {
        if o.get("enabled").and_then(|e| e.as_bool()) != Some(true) {
            continue;
        }
        let scale = o.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0);
        if scale <= 0.0 {
            continue;
        }
        let mode_id = o.get("currentModeId").and_then(|m| m.as_str())?;
        for m in o.get("modes")?.as_array()? {
            if m.get("id").and_then(|i| i.as_str()) != Some(mode_id) {
                continue;
            }
            let size = m.get("size")?;
            let w = size.get("width").and_then(|x| x.as_f64())?;
            let h = size.get("height").and_then(|x| x.as_f64())?;
            return Some(((w / scale) as u32, (h / scale) as u32, scale));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_is_centered_even_and_clamped() {
        assert_eq!(band(4096, 1728, 2000), (1048, 0, 2000, 1728));
        assert_eq!(band(4096, 1728, 2001), (1048, 0, 2000, 1728));
        assert_eq!(band(4096, 1728, 9000), (0, 0, 4096, 1728));
        assert_eq!(band(1001, 999, 2000), (0, 0, 1000, 998));
    }
}
