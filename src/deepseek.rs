use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

pub type Slot = Arc<Mutex<Option<Result<String, String>>>>;

pub fn ask(ctx: egui::Context, autopilot_dir: PathBuf, system: String, question: String) -> Slot {
    let slot: Slot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    std::thread::spawn(move || {
        let res = run(&autopilot_dir, &system, &question);
        if let Ok(mut g) = out.lock() {
            *g = Some(res);
        }
        ctx.request_repaint();
    });
    slot
}

fn run(autopilot_dir: &Path, system: &str, question: &str) -> Result<String, String> {
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
    let system = system.trim();
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(serde_json::json!({ "role": "system", "content": system }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": question }));
    let payload = serde_json::json!({
        "model": model,
        "messages": messages,
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
