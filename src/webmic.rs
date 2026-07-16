
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::transcribe::{Feeder, Transcriber, Transcript};
use crate::transcript_log::TranscriptLog;

pub const PORT: u16 = 8787;
const IDLE_STT_STOP: Duration = Duration::from_secs(30);
const CLIENT_ACTIVE: Duration = Duration::from_secs(2);
const MAX_BODY: usize = 2 * 1024 * 1024;
const LINES_CAP: usize = 200;
pub const MSG_MAX: usize = 64 * 1024;
const POSTS_CAP: usize = 30;

pub enum Post {
    Text(u64, String),
    Image(u64, egui::ColorImage),
}

#[derive(Default)]
pub struct Shared {
    pub lines: VecDeque<String>,
    pub partial: String,
    pub stt_on: bool,
    pub last_audio: Option<Instant>,
    pub posts: VecDeque<Post>,
    pub zoom: Option<Arc<Mutex<Transcript>>>,
    next_post_id: u64,
    cleared_lines: Option<VecDeque<String>>,
    cleared_posts: Option<VecDeque<Post>>,
}

impl Shared {
    pub fn client_active(&self) -> bool {
        self.last_audio.is_some_and(|t| t.elapsed() < CLIENT_ACTIVE)
    }

    pub fn push_post(&mut self, make: impl FnOnce(u64) -> Post) {
        self.next_post_id += 1;
        if self.posts.len() >= POSTS_CAP {
            self.posts.pop_front();
        }
        self.posts.push_back(make(self.next_post_id));
    }

    pub fn clear_said(&mut self) {
        if !self.lines.is_empty() {
            self.cleared_lines = Some(std::mem::take(&mut self.lines));
        }
        self.partial.clear();
    }

    pub fn undo_said(&mut self) {
        if let Some(mut old) = self.cleared_lines.take() {
            old.extend(self.lines.drain(..));
            while old.len() > LINES_CAP {
                old.pop_front();
            }
            self.lines = old;
        }
    }

    pub fn clear_sent(&mut self) {
        if !self.posts.is_empty() {
            self.cleared_posts = Some(std::mem::take(&mut self.posts));
        }
    }

    pub fn undo_sent(&mut self) {
        if let Some(mut old) = self.cleared_posts.take() {
            old.extend(self.posts.drain(..));
            while old.len() > POSTS_CAP {
                old.pop_front();
            }
            self.posts = old;
        }
    }
}

pub struct WebMic {
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    token: Option<String>,
    thread: Option<JoinHandle<()>>,
}

impl WebMic {
    pub fn start(
        channel: &'static str,
        log: Option<Arc<TranscriptLog>>,
    ) -> Result<Self, String> {
        let with_token = std::env::var("HEALTH_WEBMIC_TOKEN").as_deref() == Ok("1");
        let token = if with_token { Some(ensure_token()?) } else { None };
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

    pub fn hint(&self) -> String {
        hosts()
            .iter()
            .map(|h| match &self.token {
                Some(t) => hint_url(h, t),
                None => format!("https://{h}:{PORT}/"),
            })
            .collect::<Vec<_>>()
            .join("\n")
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
    token: Option<String>,
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
        handle_request(req, &root, channel, &log, &shared, &stop, &mut stt, &mut last_audio, token.as_deref());
    }
}

fn handle_request(
    mut req: tiny_http::Request,
    root: &Option<PathBuf>,
    channel: &'static str,
    log: &Option<Arc<TranscriptLog>>,
    shared: &Arc<Mutex<Shared>>,
    stop: &Arc<AtomicBool>,
    stt: &mut Option<(Transcriber, Feeder)>,
    last_audio: &mut Option<Instant>,
    token: Option<&str>,
) {
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();
    if let Some(token) = token {
        if needs_token(&path) && !token_ok(&url, token) {
            let _ = req.respond(tiny_http::Response::empty(403));
            return;
        }
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
    if req.method() == &tiny_http::Method::Post && path == "/api/msg" {
        let mut body = Vec::new();
        let _ = req.as_reader().take(MSG_MAX as u64 + 1).read_to_end(&mut body);
        let code = match msg_from(&body) {
            Ok(text) => {
                if let Ok(mut g) = shared.lock() {
                    g.push_post(|id| Post::Text(id, text));
                }
                200
            }
            Err(c) => c,
        };
        respond_code(req, code);
        return;
    }
    if path == "/api/zoom" {
        let key = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Sec-WebSocket-Key"))
            .map(|h| h.value.as_str().trim().to_string());
        match key.and_then(|k| ws_accept(&k)) {
            Some(accept) => {
                let resp = tiny_http::Response::empty(101).with_header(
                    tiny_http::Header::from_bytes("Sec-WebSocket-Accept", accept).unwrap(),
                );
                let stream = req.upgrade("websocket", resp);
                let shared = shared.clone();
                let stop = stop.clone();
                std::thread::spawn(move || zoom_ws_loop(stream, shared, stop));
            }
            None => {
                let _ = req.respond(tiny_http::Response::empty(400));
            }
        }
        return;
    }
    if req.method() == &tiny_http::Method::Post && (path == "/api/clear" || path == "/api/undo") {
        let undo = path == "/api/undo";
        let code = match query_param(&url, "what").as_deref() {
            Some("said") => {
                if let Ok(mut g) = shared.lock() {
                    if undo {
                        g.undo_said();
                    } else {
                        g.clear_said();
                    }
                }
                200
            }
            Some("sent") => {
                if let Ok(mut g) = shared.lock() {
                    if undo {
                        g.undo_sent();
                    } else {
                        g.clear_sent();
                    }
                }
                200
            }
            _ => 400,
        };
        respond_code(req, code);
        return;
    }
    if req.method() == &tiny_http::Method::Post && path == "/api/img" {
        let ct = req
            .headers()
            .iter()
            .find(|h| h.field.equiv("Content-Type"))
            .map(|h| h.value.as_str().to_string())
            .unwrap_or_default();
        let mut body = Vec::new();
        let _ = req.as_reader().take(IMG_MAX as u64 + 1).read_to_end(&mut body);
        let code = match decode_image(&body, &ct) {
            Ok(px) => {
                if let Ok(mut g) = shared.lock() {
                    g.push_post(|id| Post::Image(id, px));
                }
                200
            }
            Err(c) => c,
        };
        respond_code(req, code);
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

const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const WS_TICK: Duration = Duration::from_millis(300);
const WS_HEARTBEAT_TICKS: u32 = 30;

#[derive(Default)]
struct ZoomSent {
    started: bool,
    on: bool,
    finals: String,
    partial: String,
}

fn zoom_full(on: bool, finals: &str, partial: &str) -> String {
    serde_json::json!({ "on": on, "full": finals, "partial": partial }).to_string()
}

fn zoom_msg(sent: &mut ZoomSent, on: bool, finals: &str, partial: &str) -> Option<String> {
    let first = !std::mem::replace(&mut sent.started, true);
    if !on {
        sent.finals.clear();
        sent.partial.clear();
        let was_on = std::mem::replace(&mut sent.on, false);
        return (first || was_on).then(|| serde_json::json!({ "on": false }).to_string());
    }
    if first || !sent.on || !finals.starts_with(sent.finals.as_str()) {
        sent.on = true;
        sent.finals = finals.to_string();
        sent.partial = partial.to_string();
        return Some(zoom_full(true, finals, partial));
    }
    let add = &finals[sent.finals.len()..];
    if add.is_empty() && partial == sent.partial {
        return None;
    }
    let msg = serde_json::json!({ "on": true, "add": add, "partial": partial }).to_string();
    sent.finals = finals.to_string();
    sent.partial = partial.to_string();
    Some(msg)
}

fn ws_text_frame(payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(payload.len() + 10);
    f.push(0x81);
    match payload.len() {
        n if n < 126 => f.push(n as u8),
        n if n <= 0xFFFF => {
            f.push(126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            f.push(127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    f.extend_from_slice(payload);
    f
}

fn b64(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = u32::from_be_bytes([0, c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)]);
        s.push(A[(n >> 18 & 63) as usize] as char);
        s.push(A[(n >> 12 & 63) as usize] as char);
        s.push(if c.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        s.push(if c.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    s
}

fn ws_accept(key: &str) -> Option<String> {
    let mut child = Command::new("openssl")
        .args(["dgst", "-sha1", "-binary"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child
        .stdin
        .take()?
        .write_all(format!("{key}{WS_GUID}").as_bytes())
        .ok()?;
    let out = child.wait_with_output().ok()?;
    (out.status.success() && out.stdout.len() == 20).then(|| b64(&out.stdout))
}

fn zoom_snapshot(shared: &Arc<Mutex<Shared>>) -> (bool, String, String) {
    let handle = shared.lock().ok().and_then(|g| g.zoom.clone());
    match handle.and_then(|h| h.lock().ok().map(|t| (t.finals.clone(), t.partial.clone()))) {
        Some((f, p)) => (true, f, p),
        None => (false, String::new(), String::new()),
    }
}

fn zoom_ws_loop(
    mut stream: Box<dyn tiny_http::ReadWrite + Send>,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
) {
    let mut sent = ZoomSent::default();
    let mut quiet = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let (on, finals, partial) = zoom_snapshot(&shared);
        let msg = zoom_msg(&mut sent, on, &finals, &partial);
        let out = match msg {
            Some(m) => Some(m),
            None if quiet >= WS_HEARTBEAT_TICKS => {
                Some(serde_json::json!({ "on": sent.on }).to_string())
            }
            None => None,
        };
        match out {
            Some(m) => {
                quiet = 0;
                let write = stream
                    .write_all(&ws_text_frame(m.as_bytes()))
                    .and_then(|_| stream.flush());
                if write.is_err() {
                    return;
                }
            }
            None => quiet += 1,
        }
        std::thread::sleep(WS_TICK);
    }
}

const IMG_MAX: usize = 8 * 1024 * 1024;
const IMG_FIT: u32 = 1600;

fn img_format(content_type: &str) -> Option<image::ImageFormat> {
    match content_type.split(';').next()?.trim() {
        "image/png" => Some(image::ImageFormat::Png),
        "image/jpeg" => Some(image::ImageFormat::Jpeg),
        "image/webp" => Some(image::ImageFormat::WebP),
        _ => None,
    }
}

fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w, h);
    }
    let k = (max as f64 / w as f64).min(max as f64 / h as f64);
    (
        ((w as f64 * k).round() as u32).max(1),
        ((h as f64 * k).round() as u32).max(1),
    )
}

fn decode_image(body: &[u8], content_type: &str) -> Result<egui::ColorImage, u16> {
    if body.len() > IMG_MAX {
        return Err(413);
    }
    let fmt = img_format(content_type).ok_or(415u16)?;
    let img = image::load_from_memory_with_format(body, fmt).map_err(|_| 415u16)?;
    let (tw, th) = fit(img.width(), img.height(), IMG_FIT);
    let img = if (tw, th) != (img.width(), img.height()) {
        img.resize(tw, th, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, &rgba))
}

fn msg_from(body: &[u8]) -> Result<String, u16> {
    if body.len() > MSG_MAX {
        return Err(413);
    }
    let text = std::str::from_utf8(body).map_err(|_| 400u16)?.trim().to_string();
    if text.is_empty() {
        return Err(400);
    }
    Ok(text)
}

fn respond_code(req: tiny_http::Request, code: u16) {
    let body = if code == 200 { r#"{"ok":true}"# } else { r#"{"ok":false}"# };
    let resp = tiny_http::Response::from_string(body)
        .with_status_code(code)
        .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    let _ = req.respond(resp);
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

fn hint_url(host: &str, token: &str) -> String {
    format!("https://{host}:{PORT}/?t={token}")
}

fn hosts() -> Vec<String> {
    if let Some(h) = std::env::var("HEALTH_WEBMIC_HOST").ok().filter(|h| !h.is_empty()) {
        return vec![h];
    }
    let mut v: Vec<String> = [vpn_ip(), public_ip().or_else(lan_ip)]
        .into_iter()
        .flatten()
        .collect();
    if v.is_empty() {
        v.push("<ip-компа>".to_string());
    }
    v
}

fn vpn_ip() -> Option<String> {
    let links = Command::new("ip")
        .args(["-o", "link", "show", "type", "wireguard"])
        .output()
        .ok()?;
    if !links.status.success() {
        return None;
    }
    let dev = wg_dev_from(&String::from_utf8_lossy(&links.stdout))?;
    let addrs = Command::new("ip")
        .args(["-4", "-o", "addr", "show", "dev", &dev])
        .output()
        .ok()?;
    if !addrs.status.success() {
        return None;
    }
    ip_from_addr_line(&String::from_utf8_lossy(&addrs.stdout))
}

fn wg_dev_from(text: &str) -> Option<String> {
    let dev = text.lines().next()?.split_whitespace().nth(1)?;
    Some(dev.trim_end_matches(':').to_string())
}

fn ip_from_addr_line(text: &str) -> Option<String> {
    let addr = text.lines().next()?.split_whitespace().nth(3)?;
    addr.split('/').next().map(|a| a.to_string())
}

fn public_ip() -> Option<String> {
    let out = Command::new("curl")
        .args(["-s", "--max-time", "2", "https://api.ipify.org"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!ip.is_empty() && ip.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':'))
        .then_some(ip)
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
    fn msg_parsing() {
        assert_eq!(msg_from("привет".as_bytes()), Ok("привет".to_string()));
        assert_eq!(msg_from(b"  \n "), Err(400));
        assert_eq!(msg_from(&[0xff, 0xfe]), Err(400));
        assert_eq!(msg_from(&vec![b'a'; MSG_MAX + 1]), Err(413));
    }

    #[test]
    fn posts_capped_at_30() {
        let mut sh = Shared::default();
        for i in 0..31 {
            sh.push_post(|id| Post::Text(id, format!("m{i}")));
        }
        assert_eq!(sh.posts.len(), 30);
        assert!(matches!(sh.posts.front(), Some(Post::Text(2, _))));
        assert!(matches!(sh.posts.back(), Some(Post::Text(31, _))));
    }

    #[test]
    fn clear_undo_said_keeps_fresh_lines() {
        let mut sh = Shared::default();
        sh.lines.push_back("раз".to_string());
        sh.lines.push_back("два".to_string());
        sh.partial = "хвост".to_string();
        sh.clear_said();
        assert!(sh.lines.is_empty());
        assert!(sh.partial.is_empty());
        sh.lines.push_back("три".to_string());
        sh.undo_said();
        assert_eq!(
            sh.lines.iter().map(String::as_str).collect::<Vec<_>>(),
            ["раз", "два", "три"]
        );
        sh.undo_said();
        assert_eq!(sh.lines.len(), 3);
    }

    #[test]
    fn clear_of_empty_feed_keeps_stash() {
        let mut sh = Shared::default();
        sh.lines.push_back("раз".to_string());
        sh.clear_said();
        sh.clear_said();
        sh.undo_said();
        assert_eq!(
            sh.lines.iter().map(String::as_str).collect::<Vec<_>>(),
            ["раз"]
        );
    }

    #[test]
    fn clear_undo_sent_restores_posts_before_new() {
        let mut sh = Shared::default();
        sh.push_post(|id| Post::Text(id, "раз".to_string()));
        sh.push_post(|id| Post::Text(id, "два".to_string()));
        sh.clear_sent();
        assert!(sh.posts.is_empty());
        sh.push_post(|id| Post::Text(id, "три".to_string()));
        sh.undo_sent();
        assert_eq!(sh.posts.len(), 3);
        assert!(matches!(sh.posts.front(), Some(Post::Text(1, _))));
        assert!(matches!(sh.posts.back(), Some(Post::Text(3, _))));
        sh.undo_sent();
        assert_eq!(sh.posts.len(), 3);
    }

    #[test]
    fn b64_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn ws_accept_rfc_vector() {
        assert_eq!(
            ws_accept("dGhlIHNhbXBsZSBub25jZQ==").as_deref(),
            Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
        );
    }

    #[test]
    fn ws_frame_lengths() {
        let small = ws_text_frame(&[b'a'; 5]);
        assert_eq!(&small[..2], &[0x81, 5]);
        assert_eq!(small.len(), 7);
        let mid = ws_text_frame(&[b'a'; 300]);
        assert_eq!(&mid[..4], &[0x81, 126, 1, 44]);
        assert_eq!(mid.len(), 304);
        let big = ws_text_frame(&[b'a'; 70_000]);
        assert_eq!(big[1], 127);
        assert_eq!(&big[2..10], &70_000u64.to_be_bytes());
        assert_eq!(big.len(), 70_010);
    }

    #[test]
    fn zoom_msg_protocol() {
        let mut sent = ZoomSent::default();
        assert_eq!(
            zoom_msg(&mut sent, false, "", "").as_deref(),
            Some(r#"{"on":false}"#)
        );
        assert_eq!(zoom_msg(&mut sent, false, "", ""), None);
        assert_eq!(
            zoom_msg(&mut sent, true, "привет", "ми").as_deref(),
            Some(r#"{"full":"привет","on":true,"partial":"ми"}"#)
        );
        assert_eq!(zoom_msg(&mut sent, true, "привет", "ми"), None);
        assert_eq!(
            zoom_msg(&mut sent, true, "привет мир", "").as_deref(),
            Some(r#"{"add":" мир","on":true,"partial":""}"#)
        );
        assert_eq!(
            zoom_msg(&mut sent, true, "обрезано", "").as_deref(),
            Some(r#"{"full":"обрезано","on":true,"partial":""}"#)
        );
        assert_eq!(
            zoom_msg(&mut sent, false, "", "").as_deref(),
            Some(r#"{"on":false}"#)
        );
        assert_eq!(
            zoom_msg(&mut sent, true, "", "").as_deref(),
            Some(r#"{"full":"","on":true,"partial":""}"#)
        );
    }

    #[test]
    fn zoom_msg_first_tick_on() {
        let mut sent = ZoomSent::default();
        assert_eq!(
            zoom_msg(&mut sent, true, "старт", "").as_deref(),
            Some(r#"{"full":"старт","on":true,"partial":""}"#)
        );
    }

    #[test]
    fn img_format_from_content_type() {
        assert_eq!(img_format("image/png"), Some(image::ImageFormat::Png));
        assert_eq!(img_format("image/jpeg; charset=utf-8"), Some(image::ImageFormat::Jpeg));
        assert_eq!(img_format("image/webp"), Some(image::ImageFormat::WebP));
        assert_eq!(img_format("text/html"), None);
    }

    #[test]
    fn fit_downscales_preserving_ratio() {
        assert_eq!(fit(4000, 3000, 1600), (1600, 1200));
        assert_eq!(fit(3000, 4000, 1600), (1200, 1600));
        assert_eq!(fit(800, 600, 1600), (800, 600));
    }

    #[test]
    fn decode_rejects_garbage_and_accepts_png() {
        assert_eq!(decode_image(b"mus", "image/png").err(), Some(415));
        assert_eq!(decode_image(b"x", "text/plain").err(), Some(415));
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let img = decode_image(&png, "image/png").unwrap();
        assert_eq!(img.size, [2, 2]);
    }

    #[test]
    fn wg_dev_parsed_from_link_line() {
        let out = "19: linux-ultra: <POINTOPOINT,NOARP,UP,LOWER_UP> mtu 1420 qdisc noqueue state UNKNOWN mode DEFAULT group default qlen 1000\\    link/none \n";
        assert_eq!(wg_dev_from(out), Some("linux-ultra".to_string()));
        assert_eq!(wg_dev_from(""), None);
    }

    #[test]
    fn ip_parsed_from_addr_line() {
        let out = "19: linux-ultra    inet 10.8.0.9/32 scope global linux-ultra\\       valid_lft forever preferred_lft forever\n";
        assert_eq!(ip_from_addr_line(out), Some("10.8.0.9".to_string()));
        assert_eq!(ip_from_addr_line(""), None);
    }

    #[test]
    fn hint_url_format() {
        assert_eq!(
            hint_url("203.0.113.7", "abc123"),
            "https://203.0.113.7:8787/?t=abc123"
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

    fn read_token() -> String {
        std::fs::read_to_string(dirs::data_dir().unwrap().join("health-widget").join("webmic-token"))
            .unwrap()
            .trim()
            .to_string()
    }

    fn post_pcm(bytes: &[u8], seq: usize, token: &str) -> String {
        let tmp = std::env::temp_dir().join(format!("hw-e2e-{seq}.f32"));
        std::fs::write(&tmp, bytes).unwrap();
        let resp = curl_text(&[
            "-sk",
            "-X",
            "POST",
            "--data-binary",
            &format!("@{}", tmp.display()),
            &format!("https://127.0.0.1:8787/api/audio?rate=44100&seq={seq}&t={token}"),
        ]);
        let _ = std::fs::remove_file(&tmp);
        resp
    }

    fn read_exact_n(r: &mut impl Read, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            match r.read(&mut buf[got..]) {
                Ok(0) | Err(_) => panic!("поток оборвался на {got}/{n}"),
                Ok(k) => got += k,
            }
        }
        buf
    }

    fn read_ws_text(r: &mut impl Read) -> String {
        let head = read_exact_n(r, 2);
        assert_eq!(head[0], 0x81);
        let len = match head[1] {
            126 => u16::from_be_bytes(read_exact_n(r, 2).try_into().unwrap()) as usize,
            127 => u64::from_be_bytes(read_exact_n(r, 8).try_into().unwrap()) as usize,
            n => n as usize,
        };
        String::from_utf8(read_exact_n(r, len)).unwrap()
    }

    #[test]
    #[ignore]
    fn e2e_zoom_ws() {
        std::env::set_var("HEALTH_WEBMIC_TOKEN", "1");
        let wm = WebMic::start("🌐 веб", None).expect("сервер не поднялся");
        let token = read_token();

        let no_key = curl_text(&[
            "-sk", "-o", "/dev/null", "-w", "%{http_code}",
            &format!("https://127.0.0.1:8787/api/zoom?t={token}"),
        ]);
        assert_eq!(no_key, "400");

        let zoom = Arc::new(Mutex::new(crate::transcribe::Transcript::default()));
        wm.shared().lock().unwrap().zoom = Some(zoom.clone());

        let mut child = Command::new("openssl")
            .args(["s_client", "-quiet", "-connect", "127.0.0.1:8787"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("openssl s_client");
        let mut sin = child.stdin.take().unwrap();
        let mut sout = child.stdout.take().unwrap();
        write!(
            sin,
            "GET /api/zoom?t={token} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();

        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(sout.read(&mut byte).unwrap(), 1, "хедеры оборвались");
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head);
        assert!(head.starts_with("HTTP/1.1 101"), "нет 101: {head}");
        assert!(
            head.contains("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
            "нет accept-ключа: {head}"
        );

        assert_eq!(
            read_ws_text(&mut sout),
            r#"{"full":"","on":true,"partial":""}"#
        );

        zoom.lock().unwrap().finals = "привет".to_string();
        assert_eq!(
            read_ws_text(&mut sout),
            r#"{"add":"привет","on":true,"partial":""}"#
        );

        wm.shared().lock().unwrap().zoom = None;
        assert_eq!(read_ws_text(&mut sout), r#"{"on":false}"#);

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    #[ignore]
    fn e2e_clear_undo_roundtrip() {
        std::env::set_var("HEALTH_WEBMIC_TOKEN", "1");
        let wm = WebMic::start("🌐 веб", None).expect("сервер не поднялся");
        let token = read_token();
        let post_code = |path: &str| {
            curl_text(&[
                "-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST",
                &format!("https://127.0.0.1:8787{path}&t={token}"),
            ])
        };
        for text in ["раз", "два"] {
            let code = curl_text(&[
                "-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST",
                "--data-binary", text, "-H", "Content-Type: text/plain",
                &format!("https://127.0.0.1:8787/api/msg?t={token}"),
            ]);
            assert_eq!(code, "200");
        }
        if let Ok(mut g) = wm.shared().lock() {
            g.lines.push_back("сказано".to_string());
        }
        assert_eq!(post_code("/api/clear?what=sent"), "200");
        assert!(wm.shared().lock().unwrap().posts.is_empty());
        assert_eq!(post_code("/api/undo?what=sent"), "200");
        assert_eq!(wm.shared().lock().unwrap().posts.len(), 2);
        assert_eq!(post_code("/api/clear?what=said"), "200");
        assert!(wm.shared().lock().unwrap().lines.is_empty());
        assert_eq!(post_code("/api/undo?what=said"), "200");
        assert_eq!(wm.shared().lock().unwrap().lines.len(), 1);
        assert_eq!(post_code("/api/clear?what=huh"), "400");
        let no_token = curl_text(&[
            "-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST",
            "https://127.0.0.1:8787/api/clear?what=sent",
        ]);
        assert_eq!(no_token, "403");
    }

    #[test]
    #[ignore]
    fn e2e_speech_to_finals() {
        let Ok(pcm_path) = std::env::var("HW_E2E_PCM") else {
            return;
        };
        std::env::set_var("HEALTH_WEBMIC_TOKEN", "1");
        let _wm = WebMic::start("🌐 веб", None).expect("сервер не поднялся");

        let token = read_token();
        let index = curl_text(&["-sk", &format!("https://127.0.0.1:8787/?t={token}")]);
        assert!(index.contains("id=\"root\""), "статика не отдаётся: {index}");
        let denied = curl_text(&["-sk", "-o", "/dev/null", "-w", "%{http_code}", "https://127.0.0.1:8787/"]);
        assert_eq!(denied, "403");
        let miss = curl_text(&["-sk", &format!("https://127.0.0.1:8787/../etc/passwd?t={token}")]);
        assert_eq!(miss, "404");

        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let tmp_png = std::env::temp_dir().join("hw-e2e.png");
        std::fs::write(&tmp_png, &png).unwrap();
        let msg_code = curl_text(&[
            "-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST",
            "--data-binary", "привет", "-H", "Content-Type: text/plain",
            &format!("https://127.0.0.1:8787/api/msg?t={token}"),
        ]);
        assert_eq!(msg_code, "200");
        let img_code = curl_text(&[
            "-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST",
            "--data-binary", &format!("@{}", tmp_png.display()),
            "-H", "Content-Type: image/png",
            &format!("https://127.0.0.1:8787/api/img?t={token}"),
        ]);
        assert_eq!(img_code, "200");
        let bad = curl_text(&[
            "-sk", "-o", "/dev/null", "-w", "%{http_code}", "-X", "POST",
            "--data-binary", "мусор", "-H", "Content-Type: image/png",
            &format!("https://127.0.0.1:8787/api/img?t={token}"),
        ]);
        assert_eq!(bad, "415");
        assert_eq!(_wm.shared().lock().unwrap().posts.len(), 2);
        let _ = std::fs::remove_file(&tmp_png);

        let data = std::fs::read(&pcm_path).expect("нет PCM-файла");
        let mut finals: Vec<String> = Vec::new();
        let mut seq = 0;
        for c in data.chunks(44100 * 4) {
            collect_finals(&post_pcm(c, seq, &token), &mut finals);
            seq += 1;
            std::thread::sleep(Duration::from_millis(300));
        }
        let silence = vec![0u8; 44100 * 4];
        for _ in 0..60 {
            collect_finals(&post_pcm(&silence, seq, &token), &mut finals);
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
