# Непрерывный цикл автопилота — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Один тумблер «🔄 Цикл» гоняет автопилот по замкнутому конвейеру Apply→Chat→Scan→Enrich(≤2ч)→Apply без простоя.

**Architecture:** Оркестрация остаётся в виджете (`App` в `src/main.rs`): `autopilot.want: Option<Phase>` + `reconcile_pilot()` спавнит/убивает CLI-подпроцесс под фазу. Цикл — надстройка `cycle: bool`, которая в точках, где сейчас выставляется `want=None`, вместо остановки переводит на следующую стадию. Enrich капается таймером `ENRICH_WINDOW` или завершается сам при пустом пуле — что раньше.

**Tech Stack:** Rust, egui/eframe. Существующие чистые хелперы `next_eligible_in_order` / `decide_apply_chain` и тест-модуль `apply_chain_tests` в `src/main.rs`.

## Global Constraints

- Комментарии в коде запрещены (CLAUDE.md): ни `//`, ни `///`, ни `/* */`. Не возвращать удалённые.
- UI-строки — на русском.
- Ветка одна — `master`, новых веток не создавать.
- Длительность окна обогащения — хардкод-константа, без конфига/UI: `const ENRICH_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);`
- Профили и их порядок: `PILOT_PROFILES` = `[("fullstack",…),("back",…),("llm",…)]`.
- Проверка сборки: `cargo build`. Проверка тестов: `cargo test`.

---

### Task 1: Чистые хелперы решений + тесты

**Files:**
- Modify: `src/main.rs` (добавить две free-функции после `decide_apply_chain`, ~строка 86; тесты в модуль `apply_chain_tests`, ~строка 3374)

**Interfaces:**
- Consumes: `next_eligible_in_order(order: &[&str], from: &str, eligible: impl Fn(&str) -> bool) -> Option<String>` (уже есть).
- Produces:
  - `next_chat_profile(order: &[&str], from: &str, chat_done: &HashSet<String>) -> Option<String>`
  - `enrich_window_over(now: Instant, until: Option<Instant>) -> bool`

- [ ] **Step 1: Написать падающие тесты**

В `src/main.rs`, в модуле `apply_chain_tests`, расширить строку импорта и добавить тесты. Изменить:

```rust
    use super::{decide_apply_chain, next_eligible_in_order, ApplyChain};
    use std::collections::HashSet;
```

на:

```rust
    use super::{
        decide_apply_chain, enrich_window_over, next_chat_profile, next_eligible_in_order,
        ApplyChain,
    };
    use std::collections::HashSet;
    use std::time::{Duration, Instant};
```

И перед закрывающей `}` модуля добавить:

```rust
    #[test]
    fn chat_next_wraps_and_skips_done() {
        let done: HashSet<String> = ["back".to_string()].into_iter().collect();
        assert_eq!(
            next_chat_profile(ORDER, "fullstack", &done).as_deref(),
            Some("llm")
        );
    }

    #[test]
    fn chat_next_none_when_all_done() {
        let done: HashSet<String> = ORDER.iter().map(|s| s.to_string()).collect();
        assert_eq!(next_chat_profile(ORDER, "fullstack", &done), None);
    }

    #[test]
    fn enrich_window_none_never_over() {
        assert!(!enrich_window_over(Instant::now(), None));
    }

    #[test]
    fn enrich_window_open_before_deadline_closed_after() {
        let now = Instant::now();
        assert!(!enrich_window_over(now, Some(now + Duration::from_secs(60))));
        assert!(enrich_window_over(now, Some(now - Duration::from_secs(1))));
    }
```

- [ ] **Step 2: Прогнать — убедиться, что не компилируется/падает**

Run: `cargo test next_chat_profile enrich_window 2>&1 | tail -20`
Expected: FAIL — `cannot find function next_chat_profile` / `enrich_window_over`.

- [ ] **Step 3: Реализовать хелперы**

В `src/main.rs` сразу после функции `decide_apply_chain` (после её `}`, ~строка 86) добавить:

```rust
fn next_chat_profile(order: &[&str], from: &str, chat_done: &HashSet<String>) -> Option<String> {
    next_eligible_in_order(order, from, |key| !chat_done.contains(key))
}

fn enrich_window_over(now: Instant, until: Option<Instant>) -> bool {
    until.is_some_and(|u| now >= u)
}
```

- [ ] **Step 4: Прогнать тесты — зелёные**

Run: `cargo test 2>&1 | tail -20`
Expected: PASS, все тесты `apply_chain_tests` зелёные, включая 4 новых.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(pilot): чистые хелперы next_chat_profile/enrich_window_over"
```

---

### Task 2: Состояние цикла + константа окна

**Files:**
- Modify: `src/main.rs` (константа ~строка 57; поля `AutopilotState` ~строка 131; инициализация в конструкторе ~строка 509)

**Interfaces:**
- Produces: поля `AutopilotState.cycle: bool`, `AutopilotState.chat_done: HashSet<String>`, `AutopilotState.enrich_until: Option<Instant>`; `const ENRICH_WINDOW: Duration`.

- [ ] **Step 1: Добавить константу**

В `src/main.rs` после `const APPLY_BATCH_SIZE: i64 = 42;` (строка 57) добавить:

```rust
const ENRICH_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);
```

- [ ] **Step 2: Добавить поля в `AutopilotState`**

В `struct AutopilotState { … }` после строки `notify_on: bool,` (строка 131) добавить:

```rust
    cycle: bool,
    chat_done: HashSet<String>,
    enrich_until: Option<Instant>,
```

- [ ] **Step 3: Инициализировать поля в конструкторе**

В блоке `autopilot: AutopilotState { … }` после строки `notify_on: pilot_notify_on,` (строка 509) добавить:

```rust
                cycle: false,
                chat_done: HashSet::new(),
                enrich_until: None,
```

- [ ] **Step 4: Проверить сборку**

Run: `cargo build 2>&1 | tail -20`
Expected: собирается. Возможны предупреждения `field is never read` для `cycle`/`chat_done`/`enrich_until` — это ок, снимутся в Task 3.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(pilot): состояние цикла (cycle/chat_done/enrich_until) + ENRICH_WINDOW"
```

---

### Task 3: Движок цикла — методы переходов и их разводка

**Files:**
- Modify: `src/main.rs` (методы после `maybe_rotate_profile`, ~строка 854; ветка `None` в `maybe_rotate_profile` строки 847–853; вызов в `update` ~строка 2631; finish-handler строки 2687–2760)

**Interfaces:**
- Consumes: `next_chat_profile`, `enrich_window_over`, `ENRICH_WINDOW`, поля из Task 2, существующие `reconcile_pilot`, `PILOT_PROFILES`, `pilot::Phase`.
- Produces: методы `App::enter_apply_lap`, `App::enter_chat_lap`, `App::enter_scan`, `App::advance_chat`, `App::maybe_end_enrich_window`.

- [ ] **Step 1: Добавить методы переходов**

В `impl App`, сразу после закрывающей `}` метода `maybe_rotate_profile` (после строки 854), добавить:

```rust
    fn enter_apply_lap(&mut self) {
        self.autopilot.chat_done.clear();
        self.autopilot.enrich_until = None;
        self.autopilot.batch_baseline = self
            .autopilot
            .stats
            .as_ref()
            .map(|s| s.applied_today)
            .unwrap_or(0);
        self.autopilot.apply_idle.clear();
        self.autopilot.want = Some(pilot::Phase::Apply);
        self.autopilot.status = "🔄 цикл: отклики".to_string();
        self.reconcile_pilot();
    }

    fn enter_chat_lap(&mut self) {
        self.autopilot.chat_done.clear();
        self.autopilot.want = Some(pilot::Phase::Chat);
        self.autopilot.status = "🔄 цикл: чаты".to_string();
        self.reconcile_pilot();
    }

    fn enter_scan(&mut self) {
        self.autopilot.want = Some(pilot::Phase::ScanAll);
        self.autopilot.status = "🔄 цикл: скан".to_string();
        self.reconcile_pilot();
    }

    fn advance_chat(&mut self) {
        let cur = self.autopilot.profile.clone();
        self.autopilot.chat_done.insert(cur.clone());
        let order: Vec<&str> = PILOT_PROFILES.iter().map(|(k, _)| *k).collect();
        match next_chat_profile(&order, &cur, &self.autopilot.chat_done) {
            Some(next) => {
                self.autopilot.profile = next;
                self.autopilot.scan_mtime = None;
                self.autopilot.stats = None;
                self.autopilot.stats_mtime = None;
                self.reconcile_pilot();
            }
            None => self.enter_scan(),
        }
    }

    fn maybe_end_enrich_window(&mut self) {
        if !self.autopilot.cycle || self.autopilot.want != Some(pilot::Phase::Enrich) {
            return;
        }
        if enrich_window_over(Instant::now(), self.autopilot.enrich_until) {
            telemetry::event(
                "pilot.enrich_window",
                serde_json::json!({ "reason": "timeout" }),
            );
            self.enter_apply_lap();
        }
    }
```

- [ ] **Step 2: Развести таймер обогащения в `update`**

В `update`, сразу после строки `self.maybe_rotate_profile();` (строка 2631) добавить:

```rust
        self.maybe_end_enrich_window();
```

- [ ] **Step 3: Ветка «все лимиты» в `maybe_rotate_profile` → в цикле идём в чаты**

В `maybe_rotate_profile` заменить блок `None => { … }` (строки 847–853):

```rust
            None => {
                self.autopilot.want = None;
                self.autopilot.status =
                    "все профили исчерпали дневной лимит откликов".to_string();
                self.reconcile_pilot();
            }
```

на:

```rust
            None => {
                if self.autopilot.cycle {
                    self.enter_chat_lap();
                } else {
                    self.autopilot.want = None;
                    self.autopilot.status =
                        "все профили исчерпали дневной лимит откликов".to_string();
                    self.reconcile_pilot();
                }
            }
```

- [ ] **Step 4: Разводка finish-handler — скан→enrich выставляет таймер**

В finish-handler заменить блок (строки 2698–2702):

```rust
                self.autopilot.want = Some(pilot::Phase::Enrich);
                self.reconcile_pilot();
                if self.autopilot.proc.is_some() {
                    self.autopilot.status = "скан завершён — дообогащаю пул".to_string();
                }
```

на:

```rust
                self.autopilot.want = Some(pilot::Phase::Enrich);
                self.reconcile_pilot();
                if self.autopilot.proc.is_some() {
                    if self.autopilot.cycle {
                        self.autopilot.enrich_until = Some(Instant::now() + ENRICH_WINDOW);
                    }
                    self.autopilot.status = "скан завершён — дообогащаю пул".to_string();
                }
```

- [ ] **Step 5: Разводка finish-handler — Apply-Stop в цикле → чаты**

В ветке `ApplyChain::Stop` (строки 2745–2749) заменить:

```rust
                    ApplyChain::Stop => {
                        self.autopilot.want = None;
                        self.autopilot.status =
                            "все аккаунты обработали пул откликов".to_string();
                    }
```

на:

```rust
                    ApplyChain::Stop => {
                        if self.autopilot.cycle {
                            self.enter_chat_lap();
                        } else {
                            self.autopilot.want = None;
                            self.autopilot.status =
                                "все аккаунты обработали пул откликов".to_string();
                        }
                    }
```

- [ ] **Step 6: Разводка finish-handler — Chat→scan, Enrich→apply в цикле**

Заменить финальный `else`-блок (строки 2751–2759):

```rust
            } else {
                self.autopilot.want = None;
                let msg = match finished {
                    Some(pilot::Phase::Enrich) => "обогащение завершено",
                    _ => "автопилот остановлен",
                };
                self.autopilot.status = msg.to_string();
                telemetry::event("pilot.exit", serde_json::json!({ "reason": msg }));
            }
```

на:

```rust
            } else if finished == Some(pilot::Phase::Chat) && self.autopilot.cycle {
                self.advance_chat();
            } else if finished == Some(pilot::Phase::Enrich) && self.autopilot.cycle {
                self.enter_apply_lap();
            } else {
                self.autopilot.want = None;
                let msg = match finished {
                    Some(pilot::Phase::Enrich) => "обогащение завершено",
                    _ => "автопилот остановлен",
                };
                self.autopilot.status = msg.to_string();
                telemetry::event("pilot.exit", serde_json::json!({ "reason": msg }));
            }
```

- [ ] **Step 7: Проверить сборку и тесты**

Run: `cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20`
Expected: собирается без предупреждений о неиспользуемых полях/методах; тесты зелёные.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs
git commit -m "feat(pilot): движок цикла — переходы Apply→Chat→Scan→Enrich→Apply"
```

---

### Task 4: UI-тумблер «🔄 Цикл» + ручной override

**Files:**
- Modify: `src/main.rs` (`draw_autopilot`: флаг ~строка 2067, UI-строка ~строка 2105, обработка ~строки 2372–2380)

**Interfaces:**
- Consumes: `App::enter_apply_lap`, поля `cycle`/`enrich_until`, `reconcile_pilot`.

- [ ] **Step 1: Добавить флаг клика**

В `draw_autopilot`, рядом с `let mut toggle_pause = false;` (строка 2067) добавить:

```rust
            let mut toggle_cycle = false;
```

- [ ] **Step 2: Добавить строку тумблера**

В `draw_autopilot`, перед строкой `ui.add_space(2.0);` которая идёт непосредственно перед рядом «💬 Чат / 📨 Отклики» (строка 2104), добавить блок:

```rust
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.autopilot.cycle, "🔄 Цикл")
                        .on_hover_text(
                            "Непрерывно: отклики по всем аккаунтам → чаты по всем \
                             аккаунтам → скан → дообогащение (до 2ч), затем заново",
                        )
                        .clicked()
                    {
                        toggle_cycle = true;
                    }
                });
```

- [ ] **Step 3: Ручной override — любой `new_want` гасит цикл**

Заменить блок обработки `new_want` (строки 2372–2380):

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

на:

```rust
            if let Some(w) = new_want {
                self.autopilot.cycle = false;
                self.autopilot.enrich_until = None;
                if w == Some(Phase::Apply) {
                    self.autopilot.batch_baseline =
                        self.autopilot.stats.as_ref().map(|s| s.applied_today).unwrap_or(0);
                    self.autopilot.apply_idle.clear();
                }
                self.autopilot.want = w;
                self.reconcile_pilot();
            }
            if toggle_cycle {
                if self.autopilot.cycle {
                    self.autopilot.cycle = false;
                    self.autopilot.want = None;
                    self.autopilot.enrich_until = None;
                    self.autopilot.status = "цикл выключен".to_string();
                    self.reconcile_pilot();
                } else {
                    self.autopilot.cycle = true;
                    self.enter_apply_lap();
                }
            }
```

- [ ] **Step 4: Проверить сборку и тесты**

Run: `cargo build 2>&1 | tail -20 && cargo test 2>&1 | tail -20`
Expected: собирается без предупреждений; тесты зелёные.

- [ ] **Step 5: Ручной smoke (визуально)**

Run: `cargo build --release 2>&1 | tail -5`
Проверить глазами: в секции «🤖 Автопилот» появился тумблер «🔄 Цикл»; клик подсвечивает его и запускает отклики (статус «🔄 цикл: отклики»); повторный клик гасит (статус «цикл выключен»); клик по ручному «📨 Отклики»/«💬 Чат» снимает подсветку «🔄 Цикл».

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat(pilot): тумблер «🔄 Цикл» + ручной override гасит цикл"
```

---

## Self-Review

**Spec coverage:**
- Тумблер «🔄 Цикл» → Task 4. ✓
- Форма Apply→Chat→Scan→Enrich→повтор → Task 3 (переходы) + Task 4 (старт с Apply). ✓
- Enrich таймер 2ч ИЛИ пул пуст (что раньше) → Task 3 Step 4 (ставит `enrich_until`), `maybe_end_enrich_window` (таймер), ветка Enrich-finish (естественное завершение). ✓
- Хардкод-константа окна → Task 2 Step 1. ✓
- Chat-обход по аккаунтам → `advance_chat` + `next_chat_profile` (Task 1/3). ✓
- Ручной override гасит цикл → Task 4 Step 3. ✓
- Комментарии в коде отсутствуют во всех блоках. ✓

**Placeholder scan:** плейсхолдеров нет — все шаги содержат конкретный код и команды.

**Type consistency:** `next_chat_profile(&[&str], &str, &HashSet<String>) -> Option<String>` и `enrich_window_over(Instant, Option<Instant>) -> bool` совпадают в объявлении (Task 1), тестах (Task 1) и вызовах (Task 3). Методы `enter_apply_lap`/`enter_chat_lap`/`enter_scan`/`advance_chat`/`maybe_end_enrich_window` названы одинаково в объявлении (Task 3 Step 1) и вызовах (Task 3 Steps 2–6, Task 4). Поля `cycle`/`chat_done`/`enrich_until` согласованы между Task 2 и Task 3/4.
