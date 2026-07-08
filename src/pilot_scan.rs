//! Чтение статуса скана автопилота (`work-autopilot/data/scan.json`) для блока
//! «Автопилот»: список групп поиска и число новых (ещё не отработанных) вакансий
//! в очереди по каждой.
//!
//! Файл пишет автопилот: `scan-status` (без браузера), а также фазы скана и откликов
//! после каждого прохода. Виджет только читает; отсутствие/битость — не ошибка
//! (тогда кнопок групп нет, пока автопилот не создаст файл).

use std::path::Path;

use serde::Deserialize;

/// Группа поиска и число новых вакансий в очереди на неё.
#[derive(Deserialize, Clone)]
pub struct ScanGroup {
    pub name: String,
    /// Ждут отклика (новые, ещё не обрабатывали).
    #[serde(default)]
    pub pending: i64,
}

#[derive(Deserialize, Clone, Default)]
pub struct ScanStatus {
    #[serde(default)]
    pub groups: Vec<ScanGroup>,
    /// Сколько вакансий пула ещё не обогащены (полное описание + дата + вектор).
    /// Это цель кнопки «Дообогатить» в блоке «Автопилот».
    #[serde(default)]
    pub unenriched: i64,
}

/// Прочитать `scan.json` (общий пул вакансий по группам) по пути. None — нет/битый.
pub fn load(path: &Path) -> Option<ScanStatus> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
