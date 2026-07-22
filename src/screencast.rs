use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{Array, ObjectPath, OwnedObjectPath, OwnedValue, Structure, Value};

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SCREENCAST_IFACE: &str = "org.freedesktop.portal.ScreenCast";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";

const SOURCE_MONITOR: u32 = 1;
const CURSOR_EMBEDDED: u32 = 2;
const PERSIST_PERMANENT: u32 = 2;

static TOKEN_SEQ: AtomicU64 = AtomicU64::new(0);

pub struct ScreenRecorder {
    child: Child,
    _remote: zbus::zvariant::OwnedFd,
    path: PathBuf,
}

impl ScreenRecorder {
    pub fn start(dir: &Path) -> Result<Self, String> {
        let path = dir.join("screen.mkv");
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir: {e}"))?;

        let conn = Connection::session().map_err(|e| format!("session bus: {e}"))?;
        let session = create_session(&conn)?;
        select_sources(&conn, &session, load_token().as_deref())?;
        let start = start_cast(&conn, &session)?;
        if let Some(tok) = start.restore_token {
            save_token(&tok);
        }
        let remote = open_remote(&conn, &session)?;
        let fd = remote.as_raw_fd();
        clear_cloexec(fd);

        let child = spawn_gst(fd, start.node, &path)?;
        telemetry_ok(&path, start.node);
        Ok(Self { child, _remote: remote, path })
    }

    pub fn stop(mut self) {
        self.terminate();
    }

    fn terminate(&mut self) {
        let pid = self.child.id() as i32;
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }
        let _ = self.child.wait();
        crate::telemetry::event(
            "screencast.stop",
            serde_json::json!({ "file": self.path.to_string_lossy() }),
        );
    }
}

impl Drop for ScreenRecorder {
    fn drop(&mut self) {
        self.terminate();
    }
}

struct Cast {
    node: u32,
    restore_token: Option<String>,
}

fn create_session(conn: &Connection) -> Result<OwnedObjectPath, String> {
    let handle = next_token();
    let session_token = next_token();
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(handle.clone()));
    opts.insert("session_handle_token", Value::from(session_token));
    let results = await_portal(conn, &handle, move || {
        conn.call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(SCREENCAST_IFACE),
            "CreateSession",
            &(opts,),
        )
    })?;
    let handle = results
        .get("session_handle")
        .and_then(as_string)
        .ok_or("нет session_handle")?;
    ObjectPath::try_from(handle)
        .map(OwnedObjectPath::from)
        .map_err(|e| format!("session path: {e}"))
}

fn select_sources(
    conn: &Connection,
    session: &OwnedObjectPath,
    restore: Option<&str>,
) -> Result<(), String> {
    let handle = next_token();
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(handle.clone()));
    opts.insert("types", Value::from(SOURCE_MONITOR));
    opts.insert("multiple", Value::from(false));
    opts.insert("cursor_mode", Value::from(CURSOR_EMBEDDED));
    opts.insert("persist_mode", Value::from(PERSIST_PERMANENT));
    if let Some(tok) = restore {
        opts.insert("restore_token", Value::from(tok.to_string()));
    }
    let session = session.clone();
    await_portal(conn, &handle, move || {
        conn.call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(SCREENCAST_IFACE),
            "SelectSources",
            &(session, opts),
        )
    })?;
    Ok(())
}

fn start_cast(conn: &Connection, session: &OwnedObjectPath) -> Result<Cast, String> {
    let handle = next_token();
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(handle.clone()));
    let session = session.clone();
    let results = await_portal(conn, &handle, move || {
        conn.call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(SCREENCAST_IFACE),
            "Start",
            &(session, "", opts),
        )
    })?;
    let node = results
        .get("streams")
        .and_then(first_node)
        .ok_or("нет node в streams")?;
    let restore_token = results.get("restore_token").and_then(as_string);
    Ok(Cast { node, restore_token })
}

fn open_remote(
    conn: &Connection,
    session: &OwnedObjectPath,
) -> Result<zbus::zvariant::OwnedFd, String> {
    let opts: HashMap<&str, Value> = HashMap::new();
    let reply = conn
        .call_method(
            Some(PORTAL_DEST),
            PORTAL_PATH,
            Some(SCREENCAST_IFACE),
            "OpenPipeWireRemote",
            &(session, opts),
        )
        .map_err(|e| format!("OpenPipeWireRemote: {e}"))?;
    reply
        .body()
        .deserialize()
        .map_err(|e| format!("fd decode: {e}"))
}

fn await_portal<F>(
    conn: &Connection,
    token: &str,
    call: F,
) -> Result<HashMap<String, OwnedValue>, String>
where
    F: FnOnce() -> zbus::Result<zbus::Message>,
{
    let sender = unique_sender(conn)?;
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let proxy = Proxy::new(conn, PORTAL_DEST, path, REQUEST_IFACE)
        .map_err(|e| format!("request proxy: {e}"))?;
    let mut signals = proxy
        .receive_signal("Response")
        .map_err(|e| format!("subscribe: {e}"))?;
    let reply = call().map_err(|e| format!("portal call: {e}"))?;
    let _req: OwnedObjectPath = reply
        .body()
        .deserialize()
        .map_err(|e| format!("request path: {e}"))?;
    let msg = signals.next().ok_or("портал молчит")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(|e| format!("response decode: {e}"))?;
    if code != 0 {
        return Err(format!("портал отклонил (код {code})"));
    }
    Ok(results)
}

fn unique_sender(conn: &Connection) -> Result<String, String> {
    let name = conn.unique_name().ok_or("нет unique name")?;
    Ok(name.as_str().trim_start_matches(':').replace('.', "_"))
}

fn next_token() -> String {
    let n = TOKEN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("hw{}_{n}", std::process::id())
}

fn as_string(v: &OwnedValue) -> Option<String> {
    if let Ok(s) = <&str>::try_from(v) {
        return Some(s.to_string());
    }
    <ObjectPath>::try_from(v.try_clone().ok()?)
        .ok()
        .map(|p| p.as_str().to_string())
}

fn first_node(v: &OwnedValue) -> Option<u32> {
    let arr: Array = v.downcast_ref().ok()?;
    let first = arr.first()?;
    let st: Structure = first.downcast_ref().ok()?;
    u32::try_from(st.fields().first()?).ok()
}

fn clear_cloexec(fd: i32) {
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, 0);
    }
}

fn spawn_gst(fd: i32, node: u32, path: &Path) -> Result<Child, String> {
    if which("gst-launch-1.0").is_none() {
        return Err("нет gst-launch-1.0".into());
    }
    let encoder = std::env::var("HEALTH_SCREEN_ENC").unwrap_or_else(|_| "nvh264enc".to_string());
    let location = format!("location={}", path.to_string_lossy());
    let mut args: Vec<String> = vec![
        "-e".into(),
        "matroskamux".into(),
        "name=mux".into(),
        "!".into(),
        "filesink".into(),
        location,
        "pipewiresrc".into(),
        format!("fd={fd}"),
        format!("path={node}"),
        "do-timestamp=true".into(),
        "!".into(),
        "videoconvert".into(),
        "!".into(),
        "video/x-raw,format=NV12".into(),
        "!".into(),
        encoder,
        "!".into(),
        "h264parse".into(),
        "!".into(),
        "queue".into(),
        "!".into(),
        "mux.".into(),
    ];
    let devices = audio_devices();
    if !devices.is_empty() {
        args.extend(
            [
                "audiomixer",
                "name=amix",
                "!",
                "audioconvert",
                "!",
                "opusenc",
                "!",
                "queue",
                "!",
                "mux.",
            ]
            .into_iter()
            .map(String::from),
        );
        for dev in devices {
            args.extend(
                [
                    "pulsesrc".into(),
                    format!("device={dev}"),
                    "!".into(),
                    "audioconvert".into(),
                    "!".into(),
                    "audioresample".into(),
                    "!".into(),
                    "amix.".into(),
                ]
                .into_iter(),
            );
        }
    }

    let log = path.with_extension("gst.log");
    let sink = std::fs::File::create(&log).map(Stdio::from).unwrap_or_else(|_| Stdio::null());
    Command::new("gst-launch-1.0")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(sink)
        .spawn()
        .map_err(|e| format!("gst spawn: {e}"))
}

fn audio_devices() -> Vec<String> {
    if std::env::var("HEALTH_SCREEN_NOAUDIO").as_deref() == Ok("1") {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Some(sink) = pactl(&["get-default-sink"]) {
        let sink = sink.trim();
        if !sink.is_empty() {
            out.push(format!("{sink}.monitor"));
        }
    }
    if let Some(src) = pactl(&["get-default-source"]) {
        let src = src.trim().to_string();
        if !src.is_empty() && !out.contains(&src) {
            out.push(src);
        }
    }
    out
}

fn pactl(args: &[&str]) -> Option<String> {
    let out = Command::new("pactl").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

fn token_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("health-widget").join("screencast.token"))
}

fn load_token() -> Option<String> {
    let raw = std::fs::read_to_string(token_path()?).ok()?;
    let tok = raw.trim().to_string();
    (!tok.is_empty()).then_some(tok)
}

fn save_token(tok: &str) {
    if let Some(p) = token_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, tok);
    }
}

fn telemetry_ok(path: &Path, node: u32) {
    crate::telemetry::event(
        "screencast.start",
        serde_json::json!({ "file": path.to_string_lossy(), "node": node }),
    );
}
