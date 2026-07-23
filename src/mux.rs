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
