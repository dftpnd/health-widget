use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

pub type Slot = Arc<Mutex<Option<Result<String, String>>>>;

const SYSTEM_PROMPT: &str = "У нас игра: я на техническом собеседовании. \
Отвечай быстро и по делу, максимально в контексте моего опыта из резюме ниже. \
Если по опыту ответа нет — отвечай по делу и кратко.";

pub fn ask(ctx: egui::Context, autopilot_dir: PathBuf, profile: String, question: String) -> Slot {
    let slot: Slot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    std::thread::spawn(move || {
        let res = run(&autopilot_dir, &profile, &question);
        if let Ok(mut g) = out.lock() {
            *g = Some(res);
        }
        ctx.request_repaint();
    });
    slot
}

fn run(autopilot_dir: &Path, profile: &str, question: &str) -> Result<String, String> {
    let env = read_env(&autopilot_dir.join(".env"));
    let key = env.get("LLM_API_KEY").cloned().unwrap_or_default();
    if key.is_empty() || key == "EMPTY" {
        return Err("нет LLM_API_KEY в .env автопилота".into());
    }
    let base = env
        .get("LLM_BASE_URL")
        .cloned()
        .unwrap_or_else(|| "https://api.deepseek.com/v1".into());
    let model = env
        .get("LLM_MODEL")
        .cloned()
        .unwrap_or_else(|| "deepseek-chat".into());
    let resume = profile_resume(autopilot_dir, profile).unwrap_or_default();

    let system = if resume.is_empty() {
        SYSTEM_PROMPT.to_string()
    } else {
        format!("{SYSTEM_PROMPT}\n\nМоё резюме:\n{resume}")
    };
    let payload = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": question }
        ],
        "stream": false
    });
    let url = format!("{}/chat/completions", base.trim_end_matches('/'));
    let out = Command::new("curl")
        .arg("-sS")
        .args(["-X", "POST"])
        .arg(&url)
        .args(["-H", "Content-Type: application/json"])
        .arg("-H")
        .arg(format!("Authorization: Bearer {key}"))
        .arg("-d")
        .arg(payload.to_string())
        .output()
        .map_err(|e| format!("curl не запустился: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        let head: String = String::from_utf8_lossy(&out.stdout).chars().take(200).collect();
        format!("ответ не JSON: {head}")
    })?;
    if let Some(msg) = body
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Err(format!("DeepSeek: {msg}"));
    }
    body.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "пустой ответ DeepSeek".to_string())
}

fn read_env(path: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            map.insert(k.trim().to_string(), v);
        }
    }
    map
}

fn profile_resume(autopilot_dir: &Path, profile: &str) -> Option<String> {
    let overlay = autopilot_dir
        .join("config")
        .join("profiles")
        .join(format!("{profile}.yaml"));
    if let Some(r) = read_block_scalar(&overlay, "profile") {
        return Some(r);
    }
    read_block_scalar(&autopilot_dir.join("config").join("config.yaml"), "profile")
}

fn read_block_scalar(path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    extract_block_scalar(&text, key)
}

fn extract_block_scalar(text: &str, key: &str) -> Option<String> {
    let header = format!("{key}:");
    let mut found = false;
    let mut body: Vec<String> = Vec::new();
    for line in text.lines() {
        if !found {
            if line.starts_with(&header) && line[header.len()..].trim_start().starts_with('|') {
                found = true;
            }
            continue;
        }
        if line.trim().is_empty() {
            body.push(String::new());
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            body.push(line.to_string());
        } else {
            break;
        }
    }
    if !found {
        return None;
    }
    let indent = body
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let joined = body
        .iter()
        .map(|l| if l.len() >= indent { l[indent..].to_string() } else { l.clone() })
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = joined.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::extract_block_scalar;

    #[test]
    fn pulls_block_scalar_and_dedents() {
        let yaml = "contacts: |\n  один\nprofile: |\n  Иван Иванов — разработчик.\n\n  Опыт 10 лет.\ngroups: []\n";
        let got = extract_block_scalar(yaml, "profile").unwrap();
        assert_eq!(got, "Иван Иванов — разработчик.\n\nОпыт 10 лет.");
    }

    #[test]
    fn missing_key_is_none() {
        assert_eq!(extract_block_scalar("site: {}\n", "profile"), None);
    }
}
