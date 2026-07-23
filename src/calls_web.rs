use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::transcript_log::CallMeta;

pub const PORT: u16 = 8788;

const PAGE: &str = include_str!("calls_web.html");

#[derive(Clone)]
pub enum WebStatus {
    Idle,
    Running,
    Failed(String),
}

type MetaSource = fn() -> HashMap<i64, CallMeta>;

struct Row {
    id: i64,
    name: String,
    started: String,
    ended: String,
    size: u64,
}

fn scan_calls(root: &Path, meta: &HashMap<i64, CallMeta>) -> Vec<Row> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        let size = match std::fs::metadata(path.join("combined.mp4")) {
            Ok(m) if m.len() > 0 => m.len(),
            _ => continue,
        };
        let row = match meta.get(&id) {
            Some(m) => Row {
                id,
                name: m.name.clone(),
                started: m.started.clone(),
                ended: m.ended.clone(),
                size,
            },
            None => Row {
                id,
                name: format!("#{id}"),
                started: String::new(),
                ended: String::new(),
                size,
            },
        };
        out.push(row);
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

fn rows_json(rows: &[Row]) -> String {
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "name": r.name,
                "started": r.started,
                "ended": r.ended,
                "size": r.size,
            })
        })
        .collect();
    serde_json::Value::Array(items).to_string()
}

fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (from, to) = spec.split_once('-')?;
    let (from, to) = (from.trim(), to.trim());
    let (start, end) = if from.is_empty() {
        let want: u64 = to.parse().ok()?;
        if want == 0 {
            return None;
        }
        (len.saturating_sub(want), len - 1)
    } else {
        let start: u64 = from.parse().ok()?;
        let end = if to.is_empty() {
            len - 1
        } else {
            to.parse::<u64>().ok()?.min(len - 1)
        };
        (start, end)
    };
    if start > end || start >= len {
        return None;
    }
    Some((start, end))
}

pub fn open(status: Arc<Mutex<WebStatus>>, ctx: egui::Context) {
    let running = matches!(&*status.lock().unwrap(), WebStatus::Running);
    if !running {
        let Some(root) = crate::transcript_log::calls_dir() else {
            *status.lock().unwrap() = WebStatus::Failed("нет папки колов".to_string());
            ctx.request_repaint();
            return;
        };
        match tiny_http::Server::http(("127.0.0.1", PORT)) {
            Ok(server) => {
                std::thread::spawn(move || {
                    serve_loop(server, root, crate::transcript_log::call_meta)
                });
                *status.lock().unwrap() = WebStatus::Running;
                crate::telemetry::event("calls_web.start", serde_json::json!({ "port": PORT }));
            }
            Err(e) => {
                crate::telemetry::error("calls_web.bind", &e.to_string());
                *status.lock().unwrap() = WebStatus::Failed(format!("порт {PORT} занят"));
                ctx.request_repaint();
                return;
            }
        }
        ctx.request_repaint();
    }
    let _ = std::process::Command::new("xdg-open")
        .arg(format!("http://127.0.0.1:{PORT}/"))
        .spawn();
}

fn serve_loop(server: tiny_http::Server, root: PathBuf, meta: MetaSource) {
    for req in server.incoming_requests() {
        handle(req, &root, meta);
    }
}

fn handle(req: tiny_http::Request, root: &Path, meta: MetaSource) {
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    if path == "/" {
        let resp = tiny_http::Response::from_string(PAGE).with_header(
            tiny_http::Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
        );
        let _ = req.respond(resp);
        return;
    }
    if path == "/api/calls" {
        let body = rows_json(&scan_calls(root, &meta()));
        let resp = tiny_http::Response::from_string(body).with_header(
            tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
        );
        let _ = req.respond(resp);
        return;
    }
    if let Some(rest) = path.strip_prefix("/video/") {
        if let Ok(id) = rest.parse::<i64>() {
            respond_video(req, &root.join(id.to_string()).join("combined.mp4"));
            return;
        }
    }
    let _ = req.respond(tiny_http::Response::empty(404));
}

fn respond_video(req: tiny_http::Request, path: &Path) {
    let Ok(mut file) = std::fs::File::open(path) else {
        let _ = req.respond(tiny_http::Response::empty(404));
        return;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let ctype = tiny_http::Header::from_bytes("Content-Type", "video/mp4").unwrap();
    let accept = tiny_http::Header::from_bytes("Accept-Ranges", "bytes").unwrap();
    let spec = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .map(|h| h.value.as_str().to_string());
    let Some(spec) = spec else {
        let resp = tiny_http::Response::from_file(file)
            .with_header(ctype)
            .with_header(accept);
        let _ = req.respond(resp);
        return;
    };
    let Some((start, end)) = parse_range(&spec, len) else {
        let resp = tiny_http::Response::empty(416).with_header(
            tiny_http::Header::from_bytes("Content-Range", format!("bytes */{len}")).unwrap(),
        );
        let _ = req.respond(resp);
        return;
    };
    if file.seek(SeekFrom::Start(start)).is_err() {
        let _ = req.respond(tiny_http::Response::empty(500));
        return;
    }
    let count = end - start + 1;
    let crange = tiny_http::Header::from_bytes(
        "Content-Range",
        format!("bytes {start}-{end}/{len}"),
    )
    .unwrap();
    let resp = tiny_http::Response::new(
        tiny_http::StatusCode(206),
        vec![ctype, accept, crange],
        file.take(count),
        Some(count as usize),
        None,
    );
    let _ = req.respond(resp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn range_forms_parse_into_inclusive_bounds() {
        assert_eq!(parse_range("bytes=0-499", 1000), Some((0, 499)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-500", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=0-99999", 1000), Some((0, 999)));
    }

    #[test]
    fn broken_or_unsatisfiable_ranges_are_rejected() {
        assert_eq!(parse_range("байты пожалуйста", 1000), None);
        assert_eq!(parse_range("bytes=abc-def", 1000), None);
        assert_eq!(parse_range("bytes=999999-", 1000), None);
        assert_eq!(parse_range("bytes=0-10,20-30", 1000), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }

    fn mkcall(root: &std::path::Path, id: &str, size: usize) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("combined.mp4"), vec![b'x'; size]).unwrap();
    }

    #[test]
    fn scan_keeps_only_calls_with_nonempty_video() {
        let tmp = std::env::temp_dir().join(format!("hw-web-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        mkcall(&tmp, "1", 10);
        mkcall(&tmp, "3", 0);
        std::fs::create_dir_all(tmp.join("4")).unwrap();
        std::fs::write(tmp.join("4").join("mic.wav"), b"x").unwrap();
        mkcall(&tmp, "9", 20);
        std::fs::create_dir_all(tmp.join("notanumber")).unwrap();

        let mut meta = HashMap::new();
        meta.insert(
            9,
            crate::transcript_log::CallMeta {
                name: "Скрининг Ozon".to_string(),
                started: "2026-07-23 14:05:11".to_string(),
                ended: "2026-07-23 14:51:02".to_string(),
            },
        );

        let rows = scan_calls(&tmp, &meta);
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![9, 1]);
        assert_eq!(rows[0].name, "Скрининг Ozon");
        assert_eq!(rows[0].size, 20);
        assert_eq!(rows[1].name, "#1");
        assert_eq!(rows[1].started, "");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rows_serialize_to_json_array() {
        let rows = vec![Row {
            id: 12,
            name: "Кол".to_string(),
            started: "2026-07-23 14:05:11".to_string(),
            ended: String::new(),
            size: 50331648,
        }];
        let v: serde_json::Value = serde_json::from_str(&rows_json(&rows)).unwrap();
        assert_eq!(v[0]["id"], 12);
        assert_eq!(v[0]["name"], "Кол");
        assert_eq!(v[0]["size"], 50331648u64);
        assert_eq!(v[0]["ended"], "");
    }

    fn no_meta() -> HashMap<i64, CallMeta> {
        HashMap::new()
    }

    fn curl(args: &[&str]) -> String {
        let out = std::process::Command::new("curl")
            .args(args)
            .output()
            .expect("curl");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn server_serves_page_list_and_ranged_video() {
        let tmp = std::env::temp_dir().join(format!("hw-web-srv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("7")).unwrap();
        let body: Vec<u8> = (0..1000u32).map(|i| b'a' + (i % 26) as u8).collect();
        std::fs::write(tmp.join("7").join("combined.mp4"), &body).unwrap();

        let server = tiny_http::Server::http(("127.0.0.1", 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let root = tmp.clone();
        std::thread::spawn(move || serve_loop(server, root, no_meta));

        let base = format!("http://127.0.0.1:{port}");

        let page = curl(&["-s", &base]);
        assert!(page.contains("<video"), "страница: {page}");

        let json = curl(&["-s", &format!("{base}/api/calls")]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v[0]["id"], 7);
        assert_eq!(v[0]["size"], 1000);
        assert_eq!(v[0]["name"], "#7");

        let code = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}",
            "-H", "Range: bytes=0-9", &format!("{base}/video/7"),
        ]);
        assert_eq!(code, "206");

        let chunk = curl(&["-s", "-H", "Range: bytes=0-9", &format!("{base}/video/7")]);
        assert_eq!(chunk.as_bytes(), &body[..10]);

        let whole = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}:%{size_download}",
            &format!("{base}/video/7"),
        ]);
        assert_eq!(whole, "200:1000");

        let unsat = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}",
            "-H", "Range: bytes=99999-", &format!("{base}/video/7"),
        ]);
        assert_eq!(unsat, "416");

        let missing = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}", &format!("{base}/video/999"),
        ]);
        assert_eq!(missing, "404");

        let encoded_traversal = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}", &format!("{base}/video/%2E%2E%2Fetc%2Fpasswd"),
        ]);
        assert_eq!(encoded_traversal, "404");

        let non_numeric_id = curl(&[
            "-s", "-o", "/dev/null", "-w", "%{http_code}", &format!("{base}/video/7abc"),
        ]);
        assert_eq!(non_numeric_id, "404");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
