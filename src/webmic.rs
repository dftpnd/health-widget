
use std::collections::VecDeque;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::transcribe::{Feeder, Transcriber};
use crate::transcript_log::TranscriptLog;

pub const PORT: u16 = 8787;
const IDLE_STT_STOP: Duration = Duration::from_secs(30);
const CLIENT_ACTIVE: Duration = Duration::from_secs(2);
const MAX_BODY: usize = 2 * 1024 * 1024;
const LINES_CAP: usize = 200;

#[derive(Default)]
pub struct Shared {
    pub lines: VecDeque<String>,
    pub partial: String,
    pub stt_on: bool,
    pub last_audio: Option<Instant>,
}

impl Shared {
    pub fn client_active(&self) -> bool {
        self.last_audio.is_some_and(|t| t.elapsed() < CLIENT_ACTIVE)
    }
}

pub struct WebMic {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    token: String,
    thread: Option<JoinHandle<()>>,
}

impl WebMic {
    pub fn start(
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
    ) -> Result<Self, String> {
        let token = ensure_token()?;
        let (cert, key) = ensure_cert()?;
        let server = tiny_http::Server::https(
            ("0.0.0.0", PORT),
            tiny_http::SslConfig { certificate: cert, private_key: key },
        )
        .map_err(|e| format!("порт {PORT}: {e}"))?;
        let root = web_root();
        let stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(Shared::default()));
        let thread = {
            let stop = stop.clone();
            let shared = shared.clone();
            let token = token.clone();
            std::thread::spawn(move || {
                serve_loop(server, root, channel, log, stop, shared, token);
            })
        };
        crate::telemetry::event("webmic.start", serde_json::json!({ "port": PORT }));
        Ok(Self { stop, shared, token, thread: Some(thread) })
    }

    pub fn shared(&self) -> Arc<Mutex<Shared>> {
        self.shared.clone()
    }
}

impl Drop for WebMic {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        crate::telemetry::event("webmic.stop", serde_json::json!({}));
    }
}

fn serve_loop(
    server: tiny_http::Server,
    root: Option<PathBuf>,
    channel: &'static str,
    log: Option<Arc<TranscriptLog>>,
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    token: String,
) {
    let mut stt: Option<(Transcriber, Feeder)> = None;
    let mut last_audio: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        if stt.is_some()
            && last_audio.is_some_and(|t| t.elapsed() > IDLE_STT_STOP)
        {
            stt = None;
            if let Ok(mut g) = shared.lock() {
                g.stt_on = false;
                g.partial.clear();
            }
        }
        let req = match server.recv_timeout(Duration::from_millis(300)) {
            Ok(Some(r)) => r,
            Ok(None) => continue,
            Err(_) => break,
        };
        handle_request(req, &root, channel, &log, &shared, &mut stt, &mut last_audio, &token);
    }
}

fn handle_request(
    mut req: tiny_http::Request,
    root: &Option<PathBuf>,
    channel: &'static str,
    log: &Option<Arc<TranscriptLog>>,
    shared: &Arc<Mutex<Shared>>,
    stt: &mut Option<(Transcriber, Feeder)>,
    last_audio: &mut Option<Instant>,
    token: &str,
) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();
    if needs_token(&path) && !token_ok(&url, token) {
        let _ = req.respond(tiny_http::Response::empty(403));
        return;
    }
    if req.method() == &tiny_http::Method::Post && path == "/api/audio" {
        let mut body = Vec::new();
        let _ = req
            .as_reader()
            .take(MAX_BODY as u64)
            .read_to_end(&mut body);
        let reply = on_audio(&url, &body, channel, log, shared, stt, last_audio);
        let resp = tiny_http::Response::from_string(reply.to_string()).with_header(
            tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
        );
        let _ = req.respond(resp);
        return;
    }
    let (code, body, mime) = match static_file(root, &path) {
        Some((data, mime)) => (200, data, mime),
        None => (404, b"404".to_vec(), "text/plain"),
    };
    let resp = tiny_http::Response::from_data(body)
        .with_status_code(code)
        .with_header(tiny_http::Header::from_bytes("Content-Type", mime).unwrap());
    let _ = req.respond(resp);
}

fn on_audio(
    url: &str,
    body: &[u8],
    channel: &'static str,
    log: &Option<Arc<TranscriptLog>>,
    shared: &Arc<Mutex<Shared>>,
    stt: &mut Option<(Transcriber, Feeder)>,
    last_audio: &mut Option<Instant>,
) -> serde_json::Value {
    let samples = pcm_from_le(body);
    if !samples.is_empty() {
        *last_audio = Some(Instant::now());
        if stt.is_none() {
            *stt = Transcriber::start(rate_from_url(url), channel, log.clone());
        }
    }
    let mut finals: Vec<String> = Vec::new();
    let mut partial = String::new();
    let stt_alive = match stt.as_mut() {
        Some((t, f)) => {
            f.feed(&samples);
            if let Ok(mut q) = t.fresh_handle().lock() {
                finals.extend(q.drain(..));
            }
            partial = t.text().1;
            true
        }
        None => false,
    };
    if let Ok(mut g) = shared.lock() {
        g.stt_on = stt_alive;
        g.last_audio = *last_audio;
        g.partial = partial.clone();
        for l in &finals {
            if g.lines.len() >= LINES_CAP {
                g.lines.pop_front();
            }
            g.lines.push_back(l.clone());
        }
    }
    serde_json::json!({
        "finals": finals,
        "partial": partial,
        "stt": stt_alive,
    })
}

fn pcm_from_le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn needs_token(path: &str) -> bool {
    path == "/" || path == "/index.html" || path.starts_with("/api/")
}

fn token_ok(url: &str, token: &str) -> bool {
    query_param(url, "t").is_some_and(|t| t == token)
}

fn rate_from_url(url: &str) -> u32 {
    query_param(url, "rate")
        .and_then(|v| v.parse().ok())
        .filter(|r| (8000..=192_000).contains(r))
        .unwrap_or(48_000)
}

fn data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget"))
}

fn ensure_token() -> Result<String, String> {
    let dir = data_dir().ok_or_else(|| "нет data_dir".to_string())?;
    let path = dir.join("webmic-token");
    if let Ok(t) = std::fs::read_to_string(&path) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let out = Command::new("openssl")
        .args(["rand", "-hex", "16"])
        .output()
        .map_err(|e| format!("openssl: {e}"))?;
    if !out.status.success() {
        return Err("openssl rand не отработал".to_string());
    }
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if t.is_empty() {
        return Err("пустой токен".to_string());
    }
    std::fs::write(&path, &t).map_err(|e| e.to_string())?;
    Ok(t)
}

fn ensure_cert() -> Result<(Vec<u8>, Vec<u8>), String> {
    let dir = data_dir().ok_or_else(|| "нет data_dir".to_string())?;
    let cert = dir.join("webmic-cert.pem");
    let key = dir.join("webmic-key.pem");
    if !(cert.exists() && key.exists()) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let status = Command::new("openssl")
            .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650", "-subj", "/CN=health-widget", "-keyout"])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("openssl: {e}"))?;
        if !status.success() {
            return Err("openssl req не отработал".to_string());
        }
    }
    let c = std::fs::read(&cert).map_err(|e| e.to_string())?;
    let k = std::fs::read(&key).map_err(|e| e.to_string())?;
    Ok((c, k))
}

fn web_root() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        candidates.extend(
            exe.ancestors()
                .skip(1)
                .take(5)
                .map(|a| a.join("web").join("dist")),
        );
    }
    if let Some(d) = dirs::data_dir() {
        candidates.push(d.join("health-widget").join("web"));
    }
    candidates.into_iter().find(|p| p.join("index.html").exists())
}

fn static_file(root: &Option<PathBuf>, path: &str) -> Option<(Vec<u8>, &'static str)> {
    let root = root.as_ref()?;
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.split('/').any(|c| c == "..") {
        return None;
    }
    let full = root.join(rel);
    let data = std::fs::read(&full).ok()?;
    Some((data, content_type(rel)))
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub fn connect_hint() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    let ip = lan_ip().unwrap_or_else(|| "<ip-компа>".to_string());
    ssh_hint(&user, &ip, PORT)
}

fn ssh_hint(user: &str, ip: &str, port: u16) -> String {
    format!("ssh -N -L {port}:localhost:{port} {user}@{ip}\nhttp://localhost:{port}")
}

fn lan_ip() -> Option<String> {
    let out = Command::new("hostname").arg("-I").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_decodes_le_floats() {
        let mut b = Vec::new();
        b.extend_from_slice(&0.5f32.to_le_bytes());
        b.extend_from_slice(&(-1.0f32).to_le_bytes());
        b.push(7);
        assert_eq!(pcm_from_le(&b), vec![0.5, -1.0]);
    }

    #[test]
    fn rate_parsed_from_query() {
        assert_eq!(rate_from_url("/api/audio?rate=44100&seq=3"), 44100);
        assert_eq!(rate_from_url("/api/audio?seq=3"), 48000);
        assert_eq!(rate_from_url("/api/audio?rate=999999"), 48000);
        assert_eq!(rate_from_url("/api/audio"), 48000);
    }

    #[test]
    fn token_required_paths() {
        assert!(needs_token("/"));
        assert!(needs_token("/index.html"));
        assert!(needs_token("/api/audio"));
        assert!(!needs_token("/worklet.js"));
        assert!(!needs_token("/assets/index-abc.js"));
    }

    #[test]
    fn token_check() {
        assert!(token_ok("/?t=secret", "secret"));
        assert!(token_ok("/api/audio?rate=1&t=secret", "secret"));
        assert!(!token_ok("/?t=wrong", "secret"));
        assert!(!token_ok("/", "secret"));
    }

    #[test]
    fn ssh_hint_format() {
        assert_eq!(
            ssh_hint("mgu", "192.168.1.5", 8787),
            "ssh -N -L 8787:localhost:8787 mgu@192.168.1.5\nhttp://localhost:8787"
        );
    }

    #[test]
    fn static_rejects_traversal() {
        let root = Some(std::env::temp_dir());
        assert!(static_file(&root, "/../etc/passwd").is_none());
        assert!(static_file(&root, "/a/../../etc/passwd").is_none());
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(content_type("assets/app.js"), "application/javascript");
        assert_eq!(content_type("x.bin"), "application/octet-stream");
    }

    fn curl_text(args: &[&str]) -> String {
        let out = Command::new("curl").args(args).output().expect("curl");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn collect_finals(resp: &str, finals: &mut Vec<String>) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(resp) else {
            return;
        };
        if let Some(a) = v.get("finals").and_then(|f| f.as_array()) {
            finals.extend(a.iter().filter_map(|s| s.as_str().map(str::to_string)));
        }
    }

    fn post_pcm(bytes: &[u8], seq: usize) -> String {
        let tmp = std::env::temp_dir().join(format!("hw-e2e-{seq}.f32"));
        std::fs::write(&tmp, bytes).unwrap();
        let resp = curl_text(&[
            "-s",
            "-X",
            "POST",
            "--data-binary",
            &format!("@{}", tmp.display()),
            &format!("http://127.0.0.1:8787/api/audio?rate=44100&seq={seq}"),
        ]);
        let _ = std::fs::remove_file(&tmp);
        resp
    }

    #[test]
    #[ignore]
    fn e2e_speech_to_finals() {
        let Ok(pcm_path) = std::env::var("HW_E2E_PCM") else {
            return;
        };
        let _wm = WebMic::start("🌐 веб", None).expect("сервер не поднялся");

        let index = curl_text(&["-s", "http://127.0.0.1:8787/"]);
        assert!(index.contains("id=\"root\""), "статика не отдаётся: {index}");
        let miss = curl_text(&["-s", "http://127.0.0.1:8787/../etc/passwd"]);
        assert_eq!(miss, "404");

        let data = std::fs::read(&pcm_path).expect("нет PCM-файла");
        let mut finals: Vec<String> = Vec::new();
        let mut seq = 0;
        for c in data.chunks(44100 * 4) {
            collect_finals(&post_pcm(c, seq), &mut finals);
            seq += 1;
            std::thread::sleep(Duration::from_millis(300));
        }
        let silence = vec![0u8; 44100 * 4];
        for _ in 0..60 {
            collect_finals(&post_pcm(&silence, seq), &mut finals);
            seq += 1;
            if !finals.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
        println!("finals: {finals:?}");
        assert!(!finals.is_empty(), "распознавание не дало ни одной реплики");
    }
}
