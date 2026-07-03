//! Чтение метрик из JSON. Поддерживаются два формата:
//!
//! 1) Структурированный:
//!    { "title": "Здоровье", "metrics": [ {"label": "Пульс", "value": "62 bpm"}, ... ] }
//!
//! 2) Плоский объект ключ→значение:
//!    { "пульс": 62, "сон": "7ч 10м", "шаги": 8420 }

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

/// Загрузить метрики; при ошибке вернуть пустой набор (виджет покажет «нет данных»).
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
    // Сначала пробуем структурированный формат.
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

    // Иначе — плоский объект ключ→значение (порядок ключей сохраняется через preserve? нет —
    // serde_json::Map по умолчанию BTreeMap-порядок = сортировка по ключу; для стабильности этого достаточно).
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

/// Аккуратно превратить JSON-значение в строку (без кавычек у строк).
fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "—".to_string(),
        other => other.to_string(),
    }
}
