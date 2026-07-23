use std::path::{Path, PathBuf};

#[derive(Clone)]
pub enum GlueStatus {
    Idle,
    Working { done: usize, total: usize },
    Done(usize),
    Failed(String),
}

pub fn pending(active_id: Option<i64>) -> Vec<PathBuf> {
    match crate::transcript_log::calls_dir() {
        Some(root) => select_pending(&root, active_id),
        None => Vec::new(),
    }
}

fn select_pending(root: &Path, active_id: Option<i64>) -> Vec<PathBuf> {
    let mut out: Vec<(i64, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        if Some(id) == active_id {
            continue;
        }
        if path.join("combined.mp4").exists() {
            continue;
        }
        let has_audio =
            path.join("mic.wav").exists() || path.join("zoom.wav").exists();
        if !has_audio {
            continue;
        }
        out.push((id, path));
    }
    out.sort_by_key(|(id, _)| *id);
    out.into_iter().map(|(_, p)| p).collect()
}

fn glue_dir(dir: &Path) -> Result<(), String> {
    let mic = dir.join("mic.wav");
    let zoom = dir.join("zoom.wav");
    let screen = dir.join("screen.mkv");
    let out = dir.join("combined.mp4");
    let part = dir.join("combined.mp4.part");

    let has_mic = mic.exists();
    let has_zoom = zoom.exists();
    let has_video = std::fs::metadata(&screen).map(|m| m.len() > 0).unwrap_or(false);

    if !has_mic && !has_zoom {
        return Err("нет аудиодорожек".into());
    }

    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"]);

    let both = has_mic && has_zoom;
    let single = if has_mic { &mic } else { &zoom };

    if has_video {
        cmd.arg("-i").arg(&screen);
    }
    if both {
        cmd.arg("-i").arg(&mic).arg("-i").arg(&zoom);
    } else {
        cmd.arg("-i").arg(single);
    }

    if both {
        let a = if has_video { "[1:a][2:a]" } else { "[0:a][1:a]" };
        cmd.arg("-filter_complex")
            .arg(format!("{a}amix=inputs=2:normalize=0[a]"));
    }

    if has_video {
        cmd.arg("-map").arg("0:v");
        cmd.arg("-map").arg(if both { "[a]" } else { "1:a" });
        cmd.args(["-c:v", "copy"]);
    } else {
        cmd.arg("-map").arg(if both { "[a]" } else { "0:a" });
    }
    cmd.args(["-c:a", "aac"]);
    cmd.arg(&part);

    let output = cmd
        .output()
        .map_err(|_| "нет ffmpeg".to_string())?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&part);
        let err = String::from_utf8_lossy(&output.stderr);
        let tail = err.lines().last().unwrap_or("ffmpeg упал").to_string();
        return Err(tail);
    }

    std::fs::rename(&part, &out).map_err(|e| format!("rename: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn mkcall(root: &Path, id: &str, files: &[&str]) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        for f in files {
            fs::write(dir.join(f), b"x").unwrap();
        }
    }

    #[test]
    fn pending_filters_active_glued_and_empty() {
        let tmp = std::env::temp_dir().join(format!("hw-mux-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        mkcall(&tmp, "1", &["mic.wav", "zoom.wav"]);
        mkcall(&tmp, "2", &["mic.wav", "zoom.wav", "combined.mp4"]);
        mkcall(&tmp, "3", &["screen.mkv"]);
        mkcall(&tmp, "4", &["zoom.wav"]);
        mkcall(&tmp, "5", &["mic.wav"]);
        fs::write(tmp.join("notanumber"), b"x").unwrap();

        let got = select_pending(&tmp, Some(5));
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["1", "4"]);

        let _ = fs::remove_dir_all(&tmp);
    }
}
