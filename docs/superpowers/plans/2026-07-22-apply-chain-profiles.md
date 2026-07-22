# Автопилот: цепочка аккаунтов по завершении откликов — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Когда процесс `autopilot --apply` завершается сам (исчерпав пул для аккаунта), виджет переключается на следующий профиль и продолжает отклики; останавливается только когда полный круг по профилям не дал ни одного нового отклика или все упёрлись в дневной лимит.

**Architecture:** Правка целиком в `src/main.rs`. Две чистые функции (`next_eligible_in_order`, `decide_apply_chain`) несут всю логику и тестируются без процессов/ФС. Метод `next_eligible_profile` оборачивает первую поверх файлов `stats-<profile>.json` и переиспользуется в существующей `maybe_rotate_profile`. Обработчик выхода процесса (`update`, ~line 2620) получает новую ветку для `Phase::Apply`, использующую поле-детектор `apply_idle`.

**Tech Stack:** Rust, egui/eframe, `cargo test`.

## Global Constraints

- Никаких комментариев в исходниках (`//`, `///`, `/* */`) — CLAUDE.md.
- Строки UI/статусов — на русском.
- Ветка одна — `master`, git-ветки не создавать.
- Профили: `PILOT_PROFILES = [("fullstack",…),("back",…),("llm",…)]` (`src/main.rs:49`).
- `APPLY_BATCH_SIZE = 42` (`src/main.rs:55`).
- `maybe_rotate_profile` менять только на переход к общему хелперу — без смены поведения.

---

### Task 1: Чистые хелперы `next_eligible_in_order` и `decide_apply_chain`

**Files:**
- Modify: `src/main.rs` — импорт `HashSet` (рядом со строкой 3); enum + две функции после `APPLY_BATCH_SIZE` (после `src/main.rs:55`); тест-модуль в конце файла (после `fn main`, после `src/main.rs:3105+`).

**Interfaces:**
- Produces:
  - `enum ApplyChain { Switch(String), Stop }`
  - `fn next_eligible_in_order(order: &[&str], from: &str, eligible: impl Fn(&str) -> bool) -> Option<String>`
  - `fn decide_apply_chain(next: Option<&str>, apply_idle: &HashSet<String>) -> ApplyChain`

- [ ] **Step 1: Добавить импорт `HashSet`**

В шапку `src/main.rs` (после `use std::sync::Arc;`, строка 3) добавить:

```rust
use std::collections::HashSet;
```

- [ ] **Step 2: Написать чистые функции**

Вставить после строки `const APPLY_BATCH_SIZE: i64 = 42;` (`src/main.rs:55`):

```rust
#[derive(Debug, PartialEq, Eq)]
enum ApplyChain {
    Switch(String),
    Stop,
}

fn next_eligible_in_order(
    order: &[&str],
    from: &str,
    eligible: impl Fn(&str) -> bool,
) -> Option<String> {
    let n = order.len();
    if n == 0 {
        return None;
    }
    let start = order.iter().position(|k| *k == from).unwrap_or(0);
    (1..=n).find_map(|i| {
        let key = order[(start + i) % n];
        eligible(key).then(|| key.to_string())
    })
}

fn decide_apply_chain(next: Option<&str>, apply_idle: &HashSet<String>) -> ApplyChain {
    match next {
        Some(n) if !apply_idle.contains(n) => ApplyChain::Switch(n.to_string()),
        _ => ApplyChain::Stop,
    }
}
```

- [ ] **Step 3: Написать падающие тесты**

Добавить в конец `src/main.rs` (после `fn main`):

```rust
#[cfg(test)]
mod apply_chain_tests {
    use super::{decide_apply_chain, next_eligible_in_order, ApplyChain};
    use std::collections::HashSet;

    const ORDER: &[&str] = &["fullstack", "back", "llm"];

    #[test]
    fn next_wraps_to_following_profile() {
        let next = next_eligible_in_order(ORDER, "fullstack", |_| true);
        assert_eq!(next.as_deref(), Some("back"));
    }

    #[test]
    fn next_skips_ineligible() {
        let next = next_eligible_in_order(ORDER, "fullstack", |k| k != "back");
        assert_eq!(next.as_deref(), Some("llm"));
    }

    #[test]
    fn next_returns_self_when_only_self_eligible() {
        let next = next_eligible_in_order(ORDER, "fullstack", |k| k == "fullstack");
        assert_eq!(next.as_deref(), Some("fullstack"));
    }

    #[test]
    fn next_none_when_all_ineligible() {
        let next = next_eligible_in_order(ORDER, "fullstack", |_| false);
        assert_eq!(next, None);
    }

    #[test]
    fn next_none_on_empty_order() {
        let next = next_eligible_in_order(&[], "fullstack", |_| true);
        assert_eq!(next, None);
    }

    #[test]
    fn decide_switch_when_next_fresh() {
        let idle = HashSet::new();
        assert_eq!(
            decide_apply_chain(Some("back"), &idle),
            ApplyChain::Switch("back".to_string())
        );
    }

    #[test]
    fn decide_stop_when_next_none() {
        let idle = HashSet::new();
        assert_eq!(decide_apply_chain(None, &idle), ApplyChain::Stop);
    }

    #[test]
    fn decide_stop_when_next_already_idle() {
        let idle: HashSet<String> = ["back".to_string()].into_iter().collect();
        assert_eq!(decide_apply_chain(Some("back"), &idle), ApplyChain::Stop);
    }

    #[test]
    fn single_profile_exhausted_stops_after_one_exit() {
        let mut idle = HashSet::new();
        idle.insert("fullstack".to_string());
        let next = next_eligible_in_order(&["fullstack"], "fullstack", |_| true);
        assert_eq!(decide_apply_chain(next.as_deref(), &idle), ApplyChain::Stop);
    }
}
```

- [ ] **Step 4: Проверить, что тесты проходят (функции уже реализованы в Step 2)**

Run: `cargo test apply_chain_tests`
Expected: PASS — 9 тестов.

- [ ] **Step 5: Коммит**

```bash
git add src/main.rs
git commit -m "feat(pilot): чистые хелперы выбора профиля и решения цепочки откликов"
```

---

### Task 2: Поле `apply_idle`, метод `next_eligible_profile`, перевод `maybe_rotate_profile` на хелпер

**Files:**
- Modify: `src/main.rs` — поле в `AutopilotState` (`src/main.rs:88-99`); инициализация (`src/main.rs:465-476`); новый метод рядом с `maybe_rotate_profile` (`src/main.rs:745`); тело `maybe_rotate_profile` (`src/main.rs:758-772`).

**Interfaces:**
- Consumes: `next_eligible_in_order` (Task 1).
- Produces:
  - Поле `AutopilotState.apply_idle: HashSet<String>`.
  - Метод `fn next_eligible_profile(&self, from: &str) -> Option<String>`.

- [ ] **Step 1: Добавить поле в структуру**

В `AutopilotState` (`src/main.rs:88-99`), после строки `batch_baseline: i64,`:

```rust
    apply_idle: HashSet<String>,
```

- [ ] **Step 2: Инициализировать поле**

В конструкторе (`src/main.rs:465-476`), после строки `batch_baseline: 0,`:

```rust
                apply_idle: HashSet::new(),
```

- [ ] **Step 3: Добавить метод `next_eligible_profile`**

Вставить непосредственно перед `fn maybe_rotate_profile(&mut self)` (`src/main.rs:745`):

```rust
    fn next_eligible_profile(&self, from: &str) -> Option<String> {
        let order: Vec<&str> = PILOT_PROFILES.iter().map(|(k, _)| *k).collect();
        next_eligible_in_order(&order, from, |key| {
            let path = profile_stats_path(&self.cfg.autopilot_dir, key);
            pilot_stats::load(&path)
                .map(|s| s.daily_limit <= 0 || s.applied_today < s.daily_limit)
                .unwrap_or(true)
        })
    }
```

- [ ] **Step 4: Перевести `maybe_rotate_profile` на метод**

Заменить блок `src/main.rs:758-772`:

```rust
        let cur = self.autopilot.profile.clone();
        let order: Vec<String> = PILOT_PROFILES.iter().map(|(k, _)| k.to_string()).collect();
        let start = order.iter().position(|k| *k == cur).unwrap_or(0);
        let n = order.len();
        // Идём по кругу от следующего профиля; последний шаг (i == n) — снова cur,
        // это позволяет продолжить тот же профиль новым батчем, если круг замкнулся
        // и свободен только он сам.
        let next = (1..=n).find_map(|i| {
            let key = &order[(start + i) % n];
            let path = profile_stats_path(&self.cfg.autopilot_dir, key);
            let eligible = pilot_stats::load(&path)
                .map(|s| s.daily_limit <= 0 || s.applied_today < s.daily_limit)
                .unwrap_or(true);
            eligible.then(|| key.clone())
        });
```

на:

```rust
        let cur = self.autopilot.profile.clone();
        let next = self.next_eligible_profile(&cur);
```

- [ ] **Step 5: Собрать и прогнать тесты**

Run: `cargo test`
Expected: PASS — сборка проходит, все тесты (включая `apply_chain_tests`) зелёные. Поведение `maybe_rotate_profile` не изменилось.

- [ ] **Step 6: Коммит**

```bash
git add src/main.rs
git commit -m "refactor(pilot): apply_idle + общий выбор профиля в maybe_rotate_profile"
```

---

### Task 3: Цепочка аккаунтов в обработчике выхода `--apply`

**Files:**
- Modify: `src/main.rs` — ветка выхода процесса (`src/main.rs:2620-2645`); сброс `apply_idle` при включении откликов из UI (`src/main.rs:2306-2313`).

**Interfaces:**
- Consumes: `decide_apply_chain`, `ApplyChain` (Task 1); `next_eligible_profile`, `apply_idle` (Task 2).

- [ ] **Step 1: Добавить ветку `Phase::Apply` в обработчик выхода**

В `src/main.rs`, блок начинается на строке 2620. Текущий код:

```rust
        if self.autopilot.proc.as_mut().is_some_and(|p| !p.alive()) {
            let finished = self.autopilot.proc.as_ref().map(|p| p.phase().clone());
            self.autopilot.proc = None;
            if matches!(
                finished,
                Some(pilot::Phase::Scan(_)) | Some(pilot::Phase::ScanAll)
            ) {
                telemetry::event(
                    "pilot.exit",
                    serde_json::json!({ "reason": "скан завершён", "chain": "enrich" }),
                );
                self.autopilot.want = Some(pilot::Phase::Enrich);
                self.reconcile_pilot();
                if self.autopilot.proc.is_some() {
                    self.autopilot.status = "скан завершён — дообогащаю пул".to_string();
                }
            } else {
                self.autopilot.want = None;
                let msg = match finished {
                    Some(pilot::Phase::Enrich) => "обогащение завершено",
                    _ => "автопилот остановлен",
                };
                self.autopilot.status = msg.to_string();
                telemetry::event("pilot.exit", serde_json::json!({ "reason": msg }));
            }
        }
```

Вставить новую ветку между блоком скана и финальным `else`. Заменить строку `            } else {` (открывающую финальный `else`, `src/main.rs:2636`) на:

```rust
            } else if finished == Some(pilot::Phase::Apply)
                && self.autopilot.want == Some(pilot::Phase::Apply)
            {
                let cur = self.autopilot.profile.clone();
                let made = self
                    .autopilot
                    .stats
                    .as_ref()
                    .map(|s| s.applied_today - self.autopilot.batch_baseline)
                    .unwrap_or(0);
                if made > 0 {
                    self.autopilot.apply_idle.clear();
                } else {
                    self.autopilot.apply_idle.insert(cur.clone());
                }
                let next = self.next_eligible_profile(&cur);
                let decision = decide_apply_chain(next.as_deref(), &self.autopilot.apply_idle);
                let reason = match (&decision, next.is_none()) {
                    (ApplyChain::Stop, true) => "all_limited",
                    (ApplyChain::Stop, false) => "idle_lap",
                    (ApplyChain::Switch(_), _) => "switch",
                };
                telemetry::event(
                    "pilot.apply_chain",
                    serde_json::json!({
                        "from": cur,
                        "made": made,
                        "next": next,
                        "reason": reason,
                    }),
                );
                match decision {
                    ApplyChain::Switch(next_profile) => {
                        let path = profile_stats_path(&self.cfg.autopilot_dir, &next_profile);
                        self.autopilot.batch_baseline =
                            pilot_stats::load(&path).map(|s| s.applied_today).unwrap_or(0);
                        self.autopilot.profile = next_profile;
                        self.autopilot.scan_mtime = None;
                        self.autopilot.stats = None;
                        self.autopilot.stats_mtime = None;
                        self.reconcile_pilot();
                    }
                    ApplyChain::Stop => {
                        self.autopilot.want = None;
                        self.autopilot.status =
                            "все аккаунты обработали пул откликов".to_string();
                    }
                }
            } else {
```

(Финальный `else { … }` с `Enrich`/`автопилот остановлен` остаётся без изменений — он теперь ловит `Enrich` и ручной стоп.)

- [ ] **Step 2: Сбрасывать `apply_idle` при включении откликов из UI**

В блоке `if let Some(w) = new_want {` (`src/main.rs:2306-2313`), внутри `if w == Some(Phase::Apply) {`, после установки `batch_baseline`:

```rust
            if let Some(w) = new_want {
                if w == Some(Phase::Apply) {
                    self.autopilot.batch_baseline =
                        self.autopilot.stats.as_ref().map(|s| s.applied_today).unwrap_or(0);
                    self.autopilot.apply_idle.clear();
                }
                self.autopilot.want = w;
                self.reconcile_pilot();
            }
```

- [ ] **Step 3: Собрать и прогнать тесты**

Run: `cargo test`
Expected: PASS — сборка без ошибок и предупреждений о неиспользуемых `ApplyChain`/`decide_apply_chain`/`apply_idle`; все тесты зелёные.

- [ ] **Step 4: Проверка сборки релиза**

Run: `cargo build`
Expected: успешная сборка.

- [ ] **Step 5: Коммит**

```bash
git add src/main.rs
git commit -m "feat(pilot): по завершении откликов аккаунта переход к следующему"
```

---

## Проверка вручную (после Task 3)

- Запустить виджет, включить «Отклики». Когда у активного профиля исчерпается пул (процесс `--apply` завершится), в логах телеметрии появляется `pilot.apply_chain` с `reason: "switch"` и виджет продолжает отклики под следующим профилем.
- Когда все профили за круг не дали новых откликов — статус «все аккаунты обработали пул откликов», автопилот остановлен, событие с `reason: "idle_lap"` (или `"all_limited"`, если все упёрлись в дневной лимит).
- Ручной «Стоп» по-прежнему даёт «автопилот остановлен».

## Self-Review

- **Покрытие спеки:** поле `apply_idle` — Task 2; `next_eligible_profile` + рефактор `maybe_rotate_profile` — Task 2; `decide_apply_chain`/`ApplyChain` + цепочка в обработчике выхода — Task 1+3; сброс `apply_idle` из UI — Task 3; телеметрия `pilot.apply_chain` — Task 3; юнит-тесты — Task 1. Все разделы спеки покрыты.
- **Типы:** `next_eligible_in_order`/`next_eligible_profile` → `Option<String>`; `decide_apply_chain(Option<&str>, &HashSet<String>) -> ApplyChain`; имена согласованы между задачами.
- **Плейсхолдеров нет:** весь код приведён целиком.
