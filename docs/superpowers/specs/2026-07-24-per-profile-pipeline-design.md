# Пер-профильный пайплайн вакансий, статистика и цикл

Дата: 2026-07-24
Статус: спроектировано, ждёт реализации

## Проблема и цель

Сейчас автопилот работает на **общем пуле** вакансий: скан один раз кладёт union всех
поисков в `queue`, а профили лишь берут из него релевантное своему резюме. Ротация в
цикле идёт по батчу откликов, чат — отдельным кругом, богатой статистики нет.

Цель — сделать пайплайн **пер-профильным** и наблюдаемым:

- у каждого профиля свой пул вакансий; поиск (скан) идёт под аккаунтом профиля по его
  собственным поисковым URL;
- при выборе профиля виджет показывает только его группы и его статистику;
- богатая статистика профиля: новые по группам (пока не обогащено), отклики, ответы в
  чате, протухшие, 403, капчи, заполненные гугл-формы — за текущий ход и за всё время;
- ранжирование (свежесть × релевантность к резюме) естественно становится пер-профильным;
- цикл ротирует профили по правилу «27 откликов → разобрать чат → следующий».

Единственный источник правды по состоянию — `data/autopilot.db`. Файлов состояния на
диске (`scan.json`, `stats-<profile>.json`) больше нет.

## Ключевые решения (из брейншторма)

1. **Полный пер-профильный скан.** Каждый профиль сканит под своим аккаунтом браузера
   только свои группы. Пул тегируется профилем; одна вакансия под двумя профилями —
   две независимые строки (свой вектор, свой дедуп).
2. **Свои URL у профиля.** Поисковые URL переезжают в оверлей профиля
   (`config/profiles/<name>.yaml → site.listings_urls`). Глобальный union в
   `config.yaml` убирается.
3. **Статистика «за текущий ход + всё время».** «Ход» = текущий заход профиля в цикле.
4. **Правило хода профиля:** отклики до **27**, затем чат — разобрать все непрочитанные
   с крышками **40 ответов** и **30 минут в чате**; потом эстафета следующему профилю.
5. **Порядок внутри хода:** сначала все 27 откликов, затем чат до крышек.
6. **Скан — раз в сутки на профиль.** Первый ход профиля за день делает скан+обогащение;
   последующие ходы того же дня работают по уже набранному пулу.
7. **Всё в БД, лёгкий мост.** Виджет читает статистику через новую подкоманду
   `autopilot summary`, печатающую JSON в stdout (только SQL, без импорта torch). Метку
   старта хода виджет держит в RAM и передаёт как `--since`.

## Модель цикла

```
Раз в сутки на первый ход профиля за день:
  scan(профиль, его группы)   # под аккаунтом профиля
  enrich(пул профиля)

Ход профиля (крутится весь день по кругу):
  apply-проходы   до 27 успешных откликов ИЛИ пул профиля исчерпан
  chat-проходы    разобрать непрочитанные; крышки: 40 ответов ИЛИ 30 минут в чате
  → следующий eligible-профиль
Круг не даёт прогресса / все в дневном лимите → цикл спит до следующего дня.
```

- Ротация целиком на стороне виджета (`src/main.rs`), как сейчас. Триггер меняется с
  «батч 27 / дневной лимит» на правило хода (см. ниже).
- `apply` и `chat` остаются отдельными фазами/процессами (одно окно на профиль); виджет
  чередует их в рамках хода: сначала добирает 27 откликов, потом чат до крышек.
- Пул исчерпан раньше 27 → apply-фаза завершается сама, ход переходит к чат-части (уже
  существующий механизм `applies_exhausted`).

### Константы (в одном месте, `main.rs`)

- `APPLY_TARGET = 27` — цель откликов за ход (заменяет `APPLY_BATCH_SIZE`).
- `CHAT_REPLY_CAP = 40` — крышка ответов в чате за ход.
- `CHAT_TIME_CAP = 30 min` — крышка времени чат-фазы за ход.

### Правило завершения хода (чистая функция, тестируемая)

Ход профиля завершён, когда:
- `applied_in_turn >= APPLY_TARGET` **или** пул исчерпан (apply-часть закрыта), И
- чат-часть закрыта: все непрочитанные разобраны **или** `replied_in_turn >= CHAT_REPLY_CAP`
  **или** время чат-фазы `>= CHAT_TIME_CAP`.

Сигналы для виджета:
- `applied_in_turn`, `replied_in_turn` — из `summary` (`applied.turn`, `replies.turn`,
  `since` = метка старта хода в RAM).
- «Все непрочитанные разобраны» — переиспользуем существующее определение пустого
  чат-прохода (проход завершился без обработанных чатов / текущая логика завершения
  чат-круга), а не считаем абсолют.
- Время чат-фазы — таймер в RAM виджета, стартует при входе в чат-часть хода.

## Модель данных (SQLite)

### queue — пер-профильный пул

```sql
CREATE TABLE queue(
  profile      TEXT NOT NULL,
  vacancy_id   TEXT NOT NULL,
  group_name   TEXT NOT NULL,
  title        TEXT,
  url          TEXT,
  body         TEXT,
  embedding    BLOB,
  published_at TEXT,
  employer     TEXT,
  enriched     INTEGER NOT NULL DEFAULT 0,
  unavailable  INTEGER NOT NULL DEFAULT 0,
  found_at     TEXT DEFAULT (datetime('now','localtime')),
  PRIMARY KEY (profile, vacancy_id)
);
```

- Все методы пула получают параметр `profile`: `enqueue`, `candidates_for`,
  `candidates_to_enrich`, `queue_counts`, `unenriched_count`, `mark_unavailable`,
  `enrich_row`, и схлопывание клонов (`_collapse_*`, `_clone_*`).
- Схлопывание клонов и дедуп — **в пределах профиля**.
- Миграция: если в `queue` нет колонки `profile` — пересоздать таблицу с составным PK,
  существующие строки перенести как `profile='fullstack'` (исторический аккаунт по
  умолчанию), по образцу уже сделанной миграции `applications`.

### events — лог событий для счётчиков

```sql
CREATE TABLE events(
  profile TEXT NOT NULL,
  kind    TEXT NOT NULL,   -- applied | reply | captcha | form_filled | http_403 | expired
  ts      TEXT DEFAULT (datetime('now','localtime'))
);
CREATE INDEX IF NOT EXISTS events_profile_ts ON events(profile, ts);
```

Точки инкремента (добавляем `store.log_event(profile, kind)` в существующий код, не меняя
его логику):

- `applied` — успешный отклик (listings, рядом с `record_application(status='applied')`).
- `reply` — успешный ответ в чате (chat, при отправке ответа работодателю).
- `captcha` — колбэк `on_captcha` (адаптер/enrich).
- `form_filled` — успешная отправка гугл-формы (`chat_forms`).
- `http_403` — `BrowserFetcher` поймал 403.
- `expired` — вызов `mark_unavailable` (снятая/архивная).

`applied`/`reply` дублируют выводимое из `applications`/`chats`, но единый лог даёт ровный
разрез «за ход (since)» без спец-логики на каждый счётчик.

### profile_meta — снимок топ-N для лёгкого summary

```sql
CREATE TABLE profile_meta(
  profile  TEXT PRIMARY KEY,
  top_json TEXT,          -- JSON-массив топ-N заголовков ранжирования
  updated_at TEXT DEFAULT (datetime('now','localtime'))
);
```

Тяжёлая apply-фаза (`ListingHunter`, где embedder уже загружен) после ранжирования
пишет топ-N заголовков сюда. `summary` только читает `top_json` — без encode/torch.

### Дата последнего скана профиля

Выводим из `SELECT MAX(found_at) FROM queue WHERE profile = ?` (отдельная таблица не
нужна). Виджет по дате решает, нужен ли дневной скан.

## Конфиг

- Поиски переезжают в оверлеи профилей:

```yaml
# config/profiles/back.yaml
site:
  listings_urls:
    - {name: "Backend Go",       url: "https://hh.ru/search/vacancy?text=Backend+Go&..."}
    - {name: "Backend Python",   url: "https://hh.ru/search/vacancy?text=Backend+Python&..."}
    - {name: "Node.js разработчик", url: "https://hh.ru/...text=Node.js..."}
    - {name: "Rust разработчик",  url: "https://hh.ru/...text=Rust..."}
```

- fullstack получает явный оверлей `config/profiles/fullstack.yaml` с его текущими
  fullstack/frontend/LLM-поисками (сейчас они в базовом `config.yaml`).
- Базовый `config.yaml` держит только общее: промпты, `listings_filter`, селекторы,
  дефолтный `listings_url`-fallback. Глобальный `listings_urls`-union удаляется.
- Поле `groups` (allow-list имён) больше не источник для скана — группы профиля = имена
  его `listings_urls`. `SiteConfig.groups()` работает как есть, источник — пер-профильный
  после `_deep_merge` оверлея.

## Скан и обогащение

- `run_scan`/`run_enrich` уже принимают `settings.profile_name` и оверлей `bundle`
  (запускаются с `--profile`). Меняется: методы Store вызываются с `profile`, работают со
  строками пула этого профиля.
- Дневной гейт скана — в виджете: перед первым ходом профиля за день сверяет
  `last_scan_date` (из `summary`) с сегодняшней датой; если старее — запускает скан.
  Ручная кнопка скана — принудительно, как сейчас.

## Мост «виджет ↔ питон»: подкоманда summary

- `autopilot summary --profile <name> --since <iso>` → только SQL к Store, печатает JSON
  в stdout. **Не импортирует** Embedder/torch (отдельный лёгкий путь в `__main__`).
- JSON содержит:
  - `applied`: `{turn, total}` (turn = события `applied` с `ts >= since`);
  - `replies`: `{turn, total}`;
  - `captcha`/`forms`/`http_403`/`expired`: `{turn, total}`;
  - `groups`: `[{name, new}]` — новые (`enriched=0 AND unavailable=0`) по группам профиля;
  - `unenriched`: остаток необогащённых профиля;
  - `last_scan_date`: `MAX(found_at)` профиля;
  - `top`: топ-N (5) заголовков ранжирования профиля (свежесть × релевантность),
    из **персистентного снимка** (см. ниже — ранжирование требует encode/torch, а
    `summary` лёгкий, поэтому снимок пишет тяжёлая apply-фаза, а `summary` его читает);
  - `applied_today`, `daily_limit` — для дневного лимита (как сейчас).
- Виджет захватывает stdout по таймеру (≈ раз в 2–3 с), кэширует, парсит через serde.
- Удаляем запись `scan.json`/`stats-<profile>.json` в питоне и подкоманду `scan-status`;
  чтение этих файлов в виджете (`pilot_scan.rs`, `pilot_stats.rs`) заменяем разбором
  stdout от `summary`.
- Метка старта хода — в RAM виджета (`Instant` + wall clock ISO), передаётся как `--since`.

## Отображение (виджет, блок «Автопилот», всегда развёрнуто)

Для активного профиля:

- Отклики: `12/27` за ход · `340` всего.
- Ответы в чате: `3` за ход · `88` всего (+ таймер чат-фазы, когда идёт чат).
- Капча · Формы · 403 · Протухло — «за ход · всего».
- Новые по группам (пока не обогащено): `Backend Go 8 · Rust 2 · …`.
- Топ профиля: топ-5 заголовков ранжирования (то, по чему пойдут отклики).

## Границы (YAGNI)

- Без графиков/веб-дашборда — только текст в блоке.
- Механику откликов/чата/форм/капчи не трогаем — только добавляем `log_event` в
  существующих точках.
- Ранжирование (`_rank_candidates`, relevance + freshness) не переписываем.
- Legacy `data/browser-profile` fullstack не ломаем; fullstack получает явный оверлей.
- Виджет в БД не пишет (метка хода — в RAM).

## Тестирование

Питон (pytest):

- Миграция `queue` → составной PK `(profile, vacancy_id)`; старые строки → `fullstack`.
- Изоляция профилей: `enqueue`/`candidates_for`/`candidates_to_enrich`/`queue_counts`
  (одна вакансия под двумя профилями — две независимые строки, свой дедуп/схлопывание).
- `events`: `log_event` пишет строку; агрегация `{turn(since), total}` по видам корректна.
- `summary --since`: печатает валидный JSON ожидаемой формы и **не** импортирует torch
  (проверка лёгкости старта / отсутствия импорта).

Rust:

- Новое правило хода — чистая функция решения (apply-target достигнут / пул исчерпан И
  чат-часть закрыта по одному из условий) → `Switch(next)`/`Stop`, как нынешние
  `decide_apply_chain`/`next_eligible_in_order`.
- Разбор JSON `summary` из stdout (serde) с дефолтами для отсутствующих полей.

## Затрагиваемые файлы

work-autopilot (Python):

- `src/autopilot/store.py` — `profile` во всех методах пула, миграция queue, `events` +
  `log_event`, `summary`-агрегации, `MAX(found_at)`.
- `src/autopilot/config.py` — поиски из оверлея (уже через `_deep_merge`; убрать union из
  базового), fullstack-оверлей.
- `src/autopilot/orchestrator.py` — скан/enrich по `profile`; убрать запись файлов
  состояния.
- `src/autopilot/__main__.py` — подкоманда `summary` (лёгкий путь без Embedder); убрать
  `scan-status`.
- `src/autopilot/tasks/listings.py`, `tasks/chat*.py`, `site/hh*.py` — точки `log_event`.
- `config/config.yaml`, `config/profiles/*.yaml` — перенос поисков в профили.

health-widget (Rust):

- `src/main.rs` — константы `APPLY_TARGET/CHAT_REPLY_CAP/CHAT_TIME_CAP`, правило хода в
  `maybe_rotate_profile`, метка старта хода, отрисовка статистики в `draw_autopilot`.
- `src/pilot.rs` / `src/pilot_scan.rs` / `src/pilot_stats.rs` — вызов `summary` с захватом
  stdout, парсинг новой формы JSON вместо чтения файлов.
