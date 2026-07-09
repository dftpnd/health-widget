
use std::path::Path;
use std::time::SystemTime;

use serde::Deserialize;

pub struct Metrics {
    pub title: Option<String>,
    pub items: Vec<Metric>,
}

pub struct Metric {
    pub label: String,
    pub value: String,
}

#[derive(Deserialize)]
struct StructuredDoc {
    title: Option<String>,
    metrics: Vec<RawMetric>,
}

#[derive(Deserialize)]
struct RawMetric {
    label: String,
    value: serde_json::Value,
}

pub fn load(path: &Path) -> (Metrics, Option<SystemTime>) {
    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok();

    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => {
            return (
                Metrics {
                    title: None,
                    items: vec![],
                },
                mtime,
            )
        }
    };

    let metrics = parse(&text).unwrap_or(Metrics {
        title: Some("ошибка JSON".to_string()),
        items: vec![],
    });

    (metrics, mtime)
}

fn parse(text: &str) -> Option<Metrics> {
    if let Ok(doc) = serde_json::from_str::<StructuredDoc>(text) {
        return Some(Metrics {
            title: doc.title,
            items: doc
                .metrics
                .into_iter()
                .map(|m| Metric {
                    label: m.label,
                    value: value_to_string(&m.value),
                })
                .collect(),
        });
    }

    if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(text) {
        return Some(Metrics {
            title: None,
            items: map
                .into_iter()
                .map(|(k, v)| Metric {
                    label: k,
                    value: value_to_string(&v),
                })
                .collect(),
        });
    }

    None
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "—".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_format_keeps_title_and_order() {
        let m = parse(r#"{"title":"Здоровье","metrics":[
            {"label":"Пульс","value":"62 bpm"},
            {"label":"Шаги","value":8420}
        ]}"#)
        .expect("должно распарситься");
        assert_eq!(m.title.as_deref(), Some("Здоровье"));
        assert_eq!(m.items.len(), 2);
        assert_eq!(m.items[0].label, "Пульс");
        assert_eq!(m.items[0].value, "62 bpm");
        assert_eq!(m.items[1].value, "8420");
    }

    #[test]
    fn flat_object_has_no_title_and_sorts_keys() {
        let m = parse(r#"{"steps":8420,"hr":62}"#).expect("плоский объект");
        assert!(m.title.is_none());
        let labels: Vec<&str> = m.items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["hr", "steps"]);
    }

    #[test]
    fn null_value_becomes_dash() {
        assert_eq!(value_to_string(&serde_json::Value::Null), "—");
    }

    #[test]
    fn string_value_has_no_quotes() {
        let v = serde_json::json!("7ч 10м");
        assert_eq!(value_to_string(&v), "7ч 10м");
    }

    #[test]
    fn invalid_json_returns_none() {
        assert!(parse("не json вовсе").is_none());
        assert!(parse("[1,2,3]").is_none());
    }
}
