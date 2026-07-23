use std::collections::HashMap;
use std::path::Path;

use crate::transcript_log::CallMeta;

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
}
