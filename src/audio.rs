//! Захват аудио для осциллограммы — без dev-библиотек (alsa/pulse/pipewire-sys в системе нет).
//!
//! Подход тот же, что у `detect.rs` с `pw-dump`: шеллим готовый инструмент PipeWire.
//! `pw-record ... -` пишет сырой моно-PCM (f32 LE) в stdout, фоновый поток читает пайп и
//! складывает сэмплы в кольцевой буфер фиксированной длины; UI берёт снимок буфера каждый кадр.
//!
//! Каналы: микрофон — источник по умолчанию (`start(None)`); звук созвона (собеседники) —
//! monitor вывода по умолчанию (`start(default_monitor())`).

use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// Сколько последних сэмплов держим для отрисовки волны.
const CAP: usize = 2048;
const RATE: &str = "44100";

/// Имя monitor-источника вывода по умолчанию — это «то, что слышно» (Zoom/Телемост и т.п.).
/// None — если не удалось определить sink по умолчанию.
pub fn default_monitor() -> Option<String> {
    let out = Command::new("pactl").args(["get-default-sink"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sink = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sink.is_empty() {
        return None;
    }
    Some(format!("{sink}.monitor"))
}

/// Устройство/поток для выбора в UI. `target` — аргумент `pw-record --target`:
/// имя источника (для микрофона) либо node id (для потока приложения).
pub struct Device {
    pub target: String,
    pub label: String,
}

fn run_pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Реальные микрофоны/входы (Monitor of Sink = n/a), с человекочитаемым описанием.
pub fn list_mics() -> Vec<Device> {
    let text = match run_pactl(&["list", "sources"]) {
        Some(t) => t,
        None => return Vec::new(),
    };
    text.split("Source #").skip(1).filter_map(parse_mic).collect()
}

fn parse_mic(block: &str) -> Option<Device> {
    let mut name = None;
    let mut desc = None;
    let mut is_monitor = false;
    for line in block.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Name: ") {
            name = Some(v.to_string());
        } else if let Some(v) = t.strip_prefix("Description: ") {
            desc = Some(v.to_string());
        } else if let Some(v) = t.strip_prefix("Monitor of Sink: ") {
            if v != "n/a" {
                is_monitor = true;
            }
        }
    }
    if is_monitor {
        return None; // только реальные входы, мониторы — не сюда
    }
    let name = name?;
    let label = format!("🎤 {}", desc.unwrap_or_else(|| name.clone()));
    Some(Device { target: name, label })
}

/// Потоки приложений, которые прямо сейчас играют звук (Discord/Zoom/браузер…).
/// target — node id (эфемерный, меняется при перезапуске приложения → нужен ⟳).
pub fn list_programs() -> Vec<Device> {
    let out = match Command::new("pw-dump").output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let objs: Vec<serde_json::Value> = match serde_json::from_slice(&out) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut res = Vec::new();
    for o in &objs {
        if o.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let props = match o.get("info").and_then(|i| i.get("props")) {
            Some(p) => p,
            None => continue,
        };
        if props.get("media.class").and_then(|c| c.as_str()) != Some("Stream/Output/Audio") {
            continue;
        }
        // ВАЖНО: pw-record --target хочет object.serial (или node.name), а НЕ числовой id —
        // с id он молча падает на источник по умолчанию (микрофон) и пишет тишину.
        let target = match props.get("object.serial") {
            Some(v) => match v.as_u64() {
                Some(n) => n.to_string(),
                None => match v.as_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                },
            },
            None => continue,
        };
        let app = props
            .get("application.name")
            .and_then(|s| s.as_str())
            .or_else(|| props.get("node.name").and_then(|s| s.as_str()))
            .unwrap_or("?");
        let media = props
            .get("media.name")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let label = if media.is_empty() {
            format!("🔊 {app}")
        } else {
            format!("🔊 {app} — {media}")
        };
        res.push(Device { target, label });
    }
    res
}

pub struct AudioMonitor {
    samples: Arc<Mutex<VecDeque<f32>>>,
    child: Child,
}

impl AudioMonitor {
    /// Запустить захват из `target` (None — источник по умолчанию = микрофон).
    /// None-возврат — если `pw-record` недоступен или не стартовал.
    pub fn start(target: Option<&str>) -> Option<Self> {
        let mut cmd = Command::new("pw-record");
        cmd.args(["--rate", RATE, "--channels", "1", "--format", "f32"]);
        if let Some(t) = target {
            cmd.args(["--target", t]);
        }
        let mut child = cmd
            .arg("-")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;

        let mut stdout = child.stdout.take()?;
        let samples = Arc::new(Mutex::new(VecDeque::with_capacity(CAP)));
        let buf = samples.clone();

        std::thread::spawn(move || {
            let mut acc: Vec<u8> = Vec::with_capacity(8192);
            let mut raw = [0u8; 4096];
            loop {
                match stdout.read(&mut raw) {
                    Ok(0) | Err(_) => break, // пайп закрылся — процесс умер
                    Ok(n) => {
                        acc.extend_from_slice(&raw[..n]);
                        let full = acc.len() / 4 * 4; // только целые f32
                        if full == 0 {
                            continue;
                        }
                        if let Ok(mut g) = buf.lock() {
                            let mut i = 0;
                            while i < full {
                                let v = f32::from_le_bytes([acc[i], acc[i + 1], acc[i + 2], acc[i + 3]]);
                                if g.len() >= CAP {
                                    g.pop_front();
                                }
                                g.push_back(v);
                                i += 4;
                            }
                        }
                        acc.drain(..full); // остаток (неполный сэмпл) переносим на след. чтение
                    }
                }
            }
        });

        Some(Self { samples, child })
    }

    /// Скопировать текущее содержимое буфера в `out` (переиспользуемый вектор — без аллокаций).
    pub fn snapshot(&self, out: &mut Vec<f32>) {
        out.clear();
        if let Ok(g) = self.samples.lock() {
            out.extend(g.iter().copied());
        }
    }
}

impl Drop for AudioMonitor {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
