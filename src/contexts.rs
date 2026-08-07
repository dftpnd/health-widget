use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct Item {
    pub name: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Contexts {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub selected: String,
    #[serde(default)]
    pub items: Vec<Item>,
}

impl Contexts {
    pub fn selected_text(&self) -> Option<&str> {
        self.items
            .iter()
            .find(|i| i.name == self.selected)
            .map(|i| i.text.as_str())
            .filter(|t| !t.trim().is_empty())
    }

    pub fn active_text(&self) -> Option<&str> {
        if self.enabled {
            self.selected_text()
        } else {
            None
        }
    }

    pub fn free_name(&self, wanted: &str) -> String {
        let base = wanted.trim();
        let base = if base.is_empty() { "контекст" } else { base };
        if !self.items.iter().any(|i| i.name == base) {
            return base.to_string();
        }
        for n in 2..1000 {
            let candidate = format!("{base} {n}");
            if !self.items.iter().any(|i| i.name == candidate) {
                return candidate;
            }
        }
        base.to_string()
    }

    pub fn upsert(&mut self, name: String, text: String) {
        match self.items.iter_mut().find(|i| i.name == name) {
            Some(item) => item.text = text,
            None => self.items.push(Item {
                name: name.clone(),
                text,
            }),
        }
        self.selected = name;
    }

    pub fn remove(&mut self, name: &str) {
        self.items.retain(|i| i.name != name);
        if self.selected == name {
            self.selected = self.items.first().map(|i| i.name.clone()).unwrap_or_default();
        }
    }
}

pub const CONTEXT_HEADER: &str = "Контекст вопроса:";

pub fn compose_system(prompt: &str, context: Option<&str>) -> String {
    let prompt = prompt.trim();
    match context.map(str::trim).filter(|c| !c.is_empty()) {
        None => prompt.to_string(),
        Some(context) if prompt.is_empty() => format!("{CONTEXT_HEADER}\n{context}"),
        Some(context) => format!("{prompt}\n\n{CONTEXT_HEADER}\n{context}"),
    }
}

fn path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".health-widget-contexts.json")
}

pub fn load() -> Contexts {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(c: &Contexts) {
    if let Ok(s) = serde_json::to_string_pretty(c) {
        let _ = std::fs::write(path(), s);
    }
}

#[cfg(test)]
mod tests {
    use super::Contexts;

    fn with_one() -> Contexts {
        let mut c = Contexts::default();
        c.upsert("проект".into(), "мы пишем виджет".into());
        c
    }

    #[test]
    fn disabled_switch_hides_context() {
        let mut c = with_one();
        assert_eq!(c.active_text(), None);
        c.enabled = true;
        assert_eq!(c.active_text(), Some("мы пишем виджет"));
    }

    #[test]
    fn upsert_replaces_and_selects() {
        let mut c = with_one();
        c.upsert("другой".into(), "текст".into());
        c.upsert("проект".into(), "новый текст".into());
        assert_eq!(c.items.len(), 2);
        assert_eq!(c.selected, "проект");
        c.enabled = true;
        assert_eq!(c.active_text(), Some("новый текст"));
    }

    #[test]
    fn remove_moves_selection() {
        let mut c = with_one();
        c.upsert("второй".into(), "два".into());
        c.remove("второй");
        assert_eq!(c.selected, "проект");
        c.remove("проект");
        assert!(c.items.is_empty());
        assert_eq!(c.selected, "");
        c.enabled = true;
        assert_eq!(c.active_text(), None);
    }

    #[test]
    fn empty_context_not_attached() {
        let mut c = Contexts::default();
        c.enabled = true;
        c.upsert("пустой".into(), "   ".into());
        assert_eq!(c.active_text(), None);
    }

    #[test]
    fn compose_keeps_prompt_alone_without_context() {
        assert_eq!(super::compose_system("ты бот", None), "ты бот");
        assert_eq!(super::compose_system("ты бот", Some("  ")), "ты бот");
    }

    #[test]
    fn compose_glues_prompt_and_context() {
        assert_eq!(
            super::compose_system("ты бот", Some("мы пишем виджет")),
            "ты бот\n\nКонтекст вопроса:\nмы пишем виджет"
        );
        assert_eq!(
            super::compose_system("", Some("мы пишем виджет")),
            "Контекст вопроса:\nмы пишем виджет"
        );
    }

    #[test]
    fn free_name_avoids_collision() {
        let c = with_one();
        assert_eq!(c.free_name("проект"), "проект 2");
        assert_eq!(c.free_name(" "), "контекст");
    }
}
