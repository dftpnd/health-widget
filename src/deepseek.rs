use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

pub type Slot = Arc<Mutex<Option<Result<String, String>>>>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    DeepSeek,
    OpenAi,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::DeepSeek => "DS",
            Provider::OpenAi => "OAI",
        }
    }

    pub fn toggled(self) -> Provider {
        match self {
            Provider::DeepSeek => Provider::OpenAi,
            Provider::OpenAi => Provider::DeepSeek,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Provider::DeepSeek => "deepseek",
            Provider::OpenAi => "openai",
        }
    }

    pub fn from_str(s: &str) -> Provider {
        if s == "openai" {
            Provider::OpenAi
        } else {
            Provider::DeepSeek
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn ask(
    ctx: egui::Context,
    autopilot_dir: PathBuf,
    provider: Provider,
    system: String,
    history: Vec<(bool, String)>,
) -> Slot {
    let slot: Slot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    std::thread::spawn(move || {
        let res = run(&autopilot_dir, provider, &system, &history);
        if let Ok(mut g) = out.lock() {
            *g = Some(res);
        }
        ctx.request_repaint();
    });
    slot
}

fn run(
    autopilot_dir: &Path,
    provider: Provider,
    system: &str,
    history: &[(bool, String)],
) -> Result<String, String> {
    let env = read_env(&autopilot_dir.join(".env"));
    let (key_var, base_var, model_var, default_base, default_model, provider_name) = match provider {
        Provider::DeepSeek => (
            "LLM_API_KEY",
            "LLM_BASE_URL",
            "LLM_MODEL",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            "DeepSeek",
        ),
        Provider::OpenAi => (
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "OPENAI_MODEL",
            "https://api.openai.com/v1",
            "gpt-4o-mini",
            "OpenAI",
        ),
    };
    let key = env.get(key_var).cloned().unwrap_or_default();
    if key.is_empty() || key == "EMPTY" {
        return Err(format!("нет {key_var} в .env автопилота"));
    }
    let base = env
        .get(base_var)
        .cloned()
        .unwrap_or_else(|| default_base.to_string());
    let model = env
        .get(model_var)
        .cloned()
        .unwrap_or_else(|| default_model.to_string());
    let system = system.trim();
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(serde_json::json!({ "role": "system", "content": system }));
    }
    for (is_user, text) in history {
        let role = if *is_user { "user" } else { "assistant" };
        messages.push(serde_json::json!({ "role": role, "content": text }));
    }
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
        return Err(format!("{provider_name}: {msg}"));
    }
    body.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("пустой ответ {provider_name}"))
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
