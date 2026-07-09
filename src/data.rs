
use std::path::Path;
use std::time::SystemTime;

use serde::Deserialize;

pub struct Metrics {
    pub title: Option<String>,
}

#[derive(Deserialize)]
struct StructuredDoc {
    title: Option<String>,
}

pub fn load(path: &Path) -> (Metrics, Option<SystemTime>) {
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok();

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (Metrics { title: None }, mtime),
    };

    let metrics = parse(&text).unwrap_or(Metrics {
        title: Some("ошибка JSON".to_string()),
    });

    (metrics, mtime)
}

fn parse(text: &str) -> Option<Metrics> {
    serde_json::from_str::<StructuredDoc>(text)
        .ok()
        .map(|doc| Metrics { title: doc.title })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_format_keeps_title() {
        let m = parse(r#"{"title":"Здоровье","metrics":[
            {"label":"Пульс","value":"62 bpm"}
        ]}"#)
        .expect("должно распарситься");
        assert_eq!(m.title.as_deref(), Some("Здоровье"));
    }

    #[test]
    fn flat_object_has_no_title() {
        let m = parse(r#"{"steps":8420,"hr":62}"#).expect("плоский объект");
        assert!(m.title.is_none());
    }

    #[test]
    fn invalid_json_returns_none() {
        assert!(parse("не json вовсе").is_none());
        assert!(parse("[1,2,3]").is_none());
    }
}
