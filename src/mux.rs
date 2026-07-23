use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

pub fn glue_all(active_id: Option<i64>, status: Arc<Mutex<GlueStatus>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let dirs = pending(active_id);
        let total = dirs.len();
        let set = |s: GlueStatus| {
            *status.lock().unwrap() = s;
            ctx.request_repaint();
        };
        if total == 0 {
            return set(GlueStatus::Done(0));
        }
        let mut done = 0usize;
        for (i, dir) in dirs.iter().enumerate() {
            set(GlueStatus::Working { done: i, total });
            match glue_dir(dir) {
                Ok(()) => done += 1,
                Err(e) if e == "нет ffmpeg" => return set(GlueStatus::Failed(e)),
                Err(e) => crate::telemetry::error("mux.fail", &e),
            }
        }
        set(GlueStatus::Done(done));
    });
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
        if !has_audio && !has_any_frame(&path.join("frames")) {
            continue;
        }
        out.push((id, path));
    }
    out.sort_by_key(|(id, _)| *id);
    out.into_iter().map(|(_, p)| p).collect()
}

fn has_any_frame(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.path().extension().and_then(|x| x.to_str()) == Some("jpg")
    })
}

fn glue_dir(dir: &Path) -> Result<(), String> {
    let mic = dir.join("mic.wav");
    let zoom = dir.join("zoom.wav");
    let frames = dir.join("frames");
    let out = dir.join("combined.mp4");
    let part = dir.join("combined.mp4.part");

    let has_mic = mic.exists();
    let has_zoom = zoom.exists();
    let has_video = has_any_frame(&frames);
    let has_audio = has_mic || has_zoom;

    if !has_audio && !has_video {
        return Err("нет ни кадров, ни аудиодорожек".into());
    }

    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"]);

    let both = has_mic && has_zoom;
    let single = if has_mic { &mic } else { &zoom };

    if has_video {
        cmd.args(["-framerate", "1", "-pattern_type", "glob"])
            .arg("-i")
            .arg(frames.join("*.jpg"));
    }
    if has_audio {
        if both {
            cmd.arg("-i").arg(&mic).arg("-i").arg(&zoom);
        } else {
            cmd.arg("-i").arg(single);
        }
    }

    if both {
        let a = if has_video { "[1:a][2:a]" } else { "[0:a][1:a]" };
        cmd.arg("-filter_complex")
            .arg(format!("{a}amix=inputs=2:normalize=0[a]"));
    }

    if has_video {
        cmd.arg("-map").arg("0:v");
    }
    if has_audio {
        let m = if both {
            "[a]".to_string()
        } else if has_video {
            "1:a".to_string()
        } else {
            "0:a".to_string()
        };
        cmd.arg("-map").arg(m);
    }
    if has_video {
        cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    }
    if has_audio {
        cmd.args(["-c:a", "aac"]);
    }
    cmd.arg(&part);

    let output = cmd.output().map_err(|_| "нет ffmpeg".to_string())?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&part);
        let err = String::from_utf8_lossy(&output.stderr);
        let tail = err.lines().last().unwrap_or("ffmpeg упал").to_string();
        return Err(tail);
    }

    std::fs::rename(&part, &out).map_err(|e| format!("rename: {e}"))?;
    let _ = std::fs::remove_dir_all(&frames);
    Ok(())
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

    fn mkframes(root: &Path, id: &str) {
        let d = root.join(id).join("frames");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("000001.jpg"), b"x").unwrap();
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

    #[test]
    fn pending_includes_frames_and_filters_rest() {
        let tmp = std::env::temp_dir().join(format!("hw-mux-f-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        mkcall(&tmp, "1", &["mic.wav", "zoom.wav"]);
        mkcall(&tmp, "2", &["mic.wav", "zoom.wav", "combined.mp4"]);
        mkcall(&tmp, "3", &["screen.mkv"]);
        mkcall(&tmp, "4", &["zoom.wav"]);
        mkcall(&tmp, "5", &["mic.wav"]);
        mkframes(&tmp, "6");
        mkframes(&tmp, "7");
        fs::write(tmp.join("7").join("combined.mp4"), b"x").unwrap();

        let got = select_pending(&tmp, Some(5));
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["1", "4", "6"]);

        let _ = fs::remove_dir_all(&tmp);
    }
}
