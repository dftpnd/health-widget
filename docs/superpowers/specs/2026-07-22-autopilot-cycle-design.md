# Непрерывный цикл автопилота

## Проблема

Автопилот простаивает. После завершения откликов (`Apply`), чатов (`Chat`) или
обогащения (`Enrich`) виджет-оркестратор ставит `autopilot.want = None` и встаёт.
Единственная существующая цепочка — `Scan → Enrich` и обход аккаунтов внутри
`Apply`. Нужен замкнутый конвейер, который непрерывно занимает автопилот полезной
работой, а «мёртвое» время между кругами тратит на дообогащение пула.

## Цель

Один тумблер запускает бесконечный цикл:

```
Apply (все аккаунты) → Chat (все аккаунты) → Scan (ScanAll) → Enrich (≤2ч) → Apply → …
```

Enrich держится до истечения окна 2ч ИЛИ до естественного завершения (пул пуст) —
что раньше. Затем круг начинается заново.

## Архитектура

Оркестрация остаётся в виджете (`App` в `src/main.rs`): `autopilot.want:
Option<Phase>` + `reconcile_pilot()` спавнит/убивает CLI-подпроцесс автопилота под
фазу. Цикл — это надстройка, которая превращает нынешние «Stop»-переходы в
«переход к следующей стадии».

### Состояние

В `AutopilotState` (`src/main.rs`) добавляются поля:

- `cycle: bool` — цикл включён;
- `chat_done: HashSet<String>` — профили, чьи чаты пройдены в текущем чат-круге;
- `enrich_until: Option<Instant>` — дедлайн окна обогащения.

Инициализация в конструкторе: `cycle=false`, `chat_done` пуст, `enrich_until=None`.

Константа рядом с `APPLY_BATCH_SIZE`:

```rust
const ENRICH_WINDOW: Duration = Duration::from_secs(2 * 60 * 60);
```

### UI — тумблер «🔄 Цикл»

Отдельная строка в секции «🤖 Автопилот» (`draw_autopilot`). Флаг `toggle_cycle`
обрабатывается после блока рисования:

- **включить** → `cycle=true`; стартуем стадию Apply через `enter_apply_lap()`;
- **выключить** → `cycle=false`, `want=None`, `enrich_until=None`, `reconcile_pilot()`.

Любой ручной `new_want` (Чат/Отклики/Скан/Обогатить/Стоп/Выключить) дополнительно
сбрасывает `cycle=false` — ручной режим перебивает цикл.

### Переходы стадий

Принцип: **в cycle-режиме «Stop» = «следующая стадия»**. Точки перехода — те же,
где сейчас выставляется `want=None`.

| Событие | Не-cycle (как сейчас) | Cycle |
|---|---|---|
| Apply-chain `Stop` (finish-handler) | `want=None` | `enter_chat_lap()` |
| `maybe_rotate_profile` None (все лимиты) | `want=None` | `enter_chat_lap()` |
| Chat завершился | `want=None`, «автопилот остановлен» | `advance_chat()` |
| Scan/ScanAll завершился | → Enrich | → Enrich + `enrich_until = now + ENRICH_WINDOW` |
| Enrich завершился сам (пул пуст) | `want=None`, «обогащение завершено» | `enter_apply_lap()` |
| `now ≥ enrich_until` (проверка в `update()`) | — | стоп-проц Enrich → `enter_apply_lap()` |

### Хелперы-методы `App`

- `enter_apply_lap()`: `chat_done.clear()`; `enrich_until=None`;
  `batch_baseline = applied_today`; `apply_idle.clear()`; `want=Apply`;
  `reconcile_pilot()`.
- `enter_chat_lap()`: `chat_done.clear()`; `want=Chat` (текущий профиль — старт
  круга); `reconcile_pilot()`.
- `advance_chat()`: `chat_done.insert(cur)`; выбор следующего профиля через
  `next_chat_profile(order, cur, &chat_done)`; `Some` → сменить профиль +
  `reconcile_pilot()`; `None` → `enter_scan()`.
- `enter_scan()`: `want=ScanAll`; `reconcile_pilot()`.

Окно обогащения (`enrich_until`) выставляется в существующей ветке finish-handler,
где скан-завершение уводит в Enrich, — под условием `cycle`.

### Чистые функции (тестируемые)

- `next_chat_profile(order: &[&str], from: &str, chat_done: &HashSet<String>) ->
  Option<String>` — тонкая обёртка над уже покрытым тестами
  `next_eligible_in_order` с предикатом `!chat_done.contains(key)`.
- `enrich_window_over(now: Instant, until: Option<Instant>) -> bool` —
  `until.is_some_and(|u| now >= u)`.

### Статус

На каждом переходе в `self.autopilot.status` пишется короткая метка стадии
(«🔄 цикл: отклики», «🔄 цикл: чаты», «🔄 цикл: скан», «🔄 обогащение»). Показывается
штатным лейблом статуса, когда у процесса нет свежей строки лога.

## Периодичность

Доп. планирование не нужно: `update()` вызывается минимум раз в 500 мс
(`ctx.request_repaint_after(500)` в конце `update`). Опрос завершения подпроцесса
(`proc.alive()`) и проверка `enrich_window_over` идут в том же такте.

## Тестирование

- `next_chat_profile`: обход по кругу с пропуском пройденных, пустой набор,
  все пройдены → `None`.
- `enrich_window_over`: `None` → false; до дедлайна → false; после → true.
- Таблица переходов покрыта косвенно через уже существующие тесты
  `decide_apply_chain` / `next_eligible_in_order`; тяжёлую машину состояний не
  строим.

## Границы (YAGNI)

- Длительность окна — хардкод-константа, без конфига/UI.
- Нет минимального «дуэлла» на быстрых кругах: Apply/Chat/Scan — реальная
  браузерная работа, естественно пейсят цикл.
- Комментарии в коде не пишем (CLAUDE.md).
