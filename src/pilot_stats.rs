//! Чтение сводки автопилота (`work-autopilot/data/stats.json`) для блока «Автопилот».
//!
//! Файл пишет сам автопилот после каждого прохода (см. Store.write_summary). Виджет
//! только читает; отсутствие/битость файла — не ошибка (просто не показываем счётчики).

use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize, Clone, Default)]
pub struct PilotStats {
    /// Всего успешных откликов за всё время.
    #[serde(default)]
    pub applied_total: i64,
    /// Успешных откликов сегодня.
    #[serde(default)]
    pub applied_today: i64,
    /// Всего чатов, по которым что-то сделали (ответили/вышли/заполнили форму).
    #[serde(default)]
    pub chats_acted: i64,
}

/// Прочитать сводку профиля из `stats-<profile>.json` по пути. None — нет/битый.
pub fn load(path: &Path) -> Option<PilotStats> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
