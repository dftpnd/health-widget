# Суточный лимит скана (пер-профиль) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Скан вакансий засчитывается по факту запуска (а не находки новых строк), чтобы профиль сканил максимум раз в сутки.

**Architecture:** Источник правды — в work-autopilot. Новая таблица-журнал `scan_runs` фиксирует дату скана профиля; `run_scan` штампует её после каждой пройденной группы; `Store.last_scan_date` читает журнал (фолбэк на старую формулу по `queue`). Виджет health-widget не трогаем — его гейт `begin_turn` начинает работать корректно на честном значении.

**Tech Stack:** Python 3, sqlite3, pytest.

## Global Constraints

- Репозиторий правок: `~/projects/work-autopilot`. Спека: `~/projects/health-widget/docs/superpowers/specs/2026-07-25-scan-daily-limit-design.md`.
- Никаких комментариев в коде (ни `#`, ни docstring'и функций) — правило проекта. Существующие docstring'и модулей/классов не трогаем и не добавляем новые.
- Ключ профиля везде — `settings.profile_name` (тот же, под которым `enqueue` пишет `queue`).
- Даты — локальные, формат `YYYY-MM-DD`, через `date('now','localtime')` в SQL.
- Запуск тестов: из `~/projects/work-autopilot`, `pytest tests/<file>::<name> -v`.

---

### Task 1: Журнал сканов в Store (`scan_runs`, `mark_scan_run`, `last_scan_date`)

**Files:**
- Modify: `src/autopilot/store.py` — добавить таблицу `scan_runs` в `_SCHEMA` (~строка 44–125), метод `mark_scan_run`, переписать `last_scan_date` (строки 621–625).
- Test: `tests/test_scan_runs.py` (create)

**Interfaces:**
- Consumes: `Store(db_path)`, `Store.enqueue(profile, group_name, vacancy_id, title, url, body)`.
- Produces:
  - `Store.mark_scan_run(profile: str) -> None` — фиксирует, что `profile` сканился сегодня (идемпотентно по дате).
  - `Store.last_scan_date(profile: str) -> str | None` — `MAX(ran_on)` из `scan_runs`; если для профиля записей нет — фолбэк на `date(MAX(found_at))` из `queue`.

- [ ] **Step 1: Write the failing test**

Создать `tests/test_scan_runs.py`:

```python
from autopilot.store import Store


def _today(store):
    return store._conn.execute("SELECT date('now','localtime')").fetchone()[0]


def test_mark_scan_run_sets_today_without_queue_rows(tmp_path):
    store = Store(tmp_path / "t.db")
    store.mark_scan_run("back")
    assert store.last_scan_date("back") == _today(store)
    store.close()


def test_last_scan_date_falls_back_to_queue_when_no_runs(tmp_path):
    store = Store(tmp_path / "t.db")
    store.enqueue("back", "Backend Go", "a", "Go dev", "u", "body")
    store._conn.execute(
        "UPDATE queue SET found_at = '2001-02-03 10:00:00' "
        "WHERE profile='back' AND vacancy_id='a'"
    )
    store._conn.commit()
    assert store.last_scan_date("back") == "2001-02-03"
    store.close()


def test_mark_scan_run_idempotent_per_day(tmp_path):
    store = Store(tmp_path / "t.db")
    store.mark_scan_run("back")
    store.mark_scan_run("back")
    rows = store._conn.execute(
        "SELECT COUNT(*) FROM scan_runs WHERE profile='back'"
    ).fetchone()[0]
    assert rows == 1
    assert store.last_scan_date("back") == _today(store)
    store.close()


def test_scan_runs_isolated_per_profile(tmp_path):
    store = Store(tmp_path / "t.db")
    store.mark_scan_run("back")
    assert store.last_scan_date("llm") is None
    store.close()
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_scan_runs.py -v`
Expected: FAIL — `AttributeError: 'Store' object has no attribute 'mark_scan_run'` (и/или `no such table: scan_runs`).

- [ ] **Step 3: Add `scan_runs` table to `_SCHEMA`**

В `src/autopilot/store.py`, внутри строки `_SCHEMA` (рядом с прочими `CREATE TABLE IF NOT EXISTS`, например после блока `events`/`events_profile_ts`), добавить:

```sql
CREATE TABLE IF NOT EXISTS scan_runs (
    profile TEXT NOT NULL,
    ran_on  TEXT NOT NULL,
    PRIMARY KEY (profile, ran_on)
);
```

Составной первичный ключ `(profile, ran_on)` даёт идемпотентность по дате на уровне схемы.

- [ ] **Step 4: Add `mark_scan_run` and rewrite `last_scan_date`**

Заменить текущий `last_scan_date` (строки 621–625):

```python
    def last_scan_date(self, profile: str) -> str | None:
        row = self._conn.execute(
            "SELECT date(MAX(found_at)) FROM queue WHERE profile = ?", (profile,)
        ).fetchone()
        return row[0] if row else None
```

на:

```python
    def mark_scan_run(self, profile: str) -> None:
        self._conn.execute(
            "INSERT OR IGNORE INTO scan_runs(profile, ran_on) "
            "VALUES (?, date('now','localtime'))",
            (profile,),
        )
        self._conn.commit()

    def last_scan_date(self, profile: str) -> str | None:
        row = self._conn.execute(
            "SELECT MAX(ran_on) FROM scan_runs WHERE profile = ?", (profile,)
        ).fetchone()
        if row and row[0] is not None:
            return row[0]
        row = self._conn.execute(
            "SELECT date(MAX(found_at)) FROM queue WHERE profile = ?", (profile,)
        ).fetchone()
        return row[0] if row else None
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest tests/test_scan_runs.py -v`
Expected: PASS (4 passed).

- [ ] **Step 6: Run summary regression to ensure nothing broke**

Run: `pytest tests/test_summary_cli.py tests/test_pool_stats.py -v`
Expected: PASS (существующие тесты `last_scan_date`/summary зелёные — фолбэк сохраняет старое поведение при пустом `scan_runs`).

- [ ] **Step 7: Commit**

```bash
cd ~/projects/work-autopilot
git add src/autopilot/store.py tests/test_scan_runs.py
git commit -m "feat(scan): журнал scan_runs — скан засчитывается по запуску, не по находке"
```

---

### Task 2: Штамп скана в `run_scan`

**Files:**
- Modify: `src/autopilot/orchestrator.py` — `run_scan` (строки ~160–194).
- Test: `tests/test_scan_marks_run.py` (create)

**Interfaces:**
- Consumes: `Store.mark_scan_run(profile)`, `Store.last_scan_date(profile)` (Task 1); `run_scan(settings, bundle, *, group_name=None)`.
- Produces: после `run_scan` `last_scan_date(settings.profile_name)` == сегодня, даже если скан не нашёл ни одной новой вакансии.

- [ ] **Step 1: Write the failing test**

Создать `tests/test_scan_marks_run.py`. Скан гоняет браузер/сеть, поэтому тестируем узкий контракт: `run_scan` вызывает `store.mark_scan_run(settings.profile_name)` после каждой пройденной группы. Мокаем `_build` и обход групп, проверяем эффект на реальном `Store`.

```python
import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from autopilot import orchestrator
from autopilot.store import Store


def _bundle(names):
    groups = [SimpleNamespace(name=n) for n in names]
    site = SimpleNamespace(groups=lambda: groups)
    return SimpleNamespace(site=site)


def test_run_scan_marks_run_per_group(tmp_path):
    store = Store(tmp_path / "t.db")
    settings = SimpleNamespace(profile_name="back")
    bundle = _bundle(["Backend Go", "Rust"])

    listings = SimpleNamespace(scan_group=AsyncMock(return_value=0))
    gate = SimpleNamespace(wait_if_paused=AsyncMock())

    class _Session:
        async def __aenter__(self):
            return self
        async def __aexit__(self, *a):
            return False

    with patch.object(orchestrator, "_install_pause_gate", return_value=gate), \
         patch.object(orchestrator, "BrowserSession", return_value=_Session()), \
         patch.object(orchestrator, "_build",
                      AsyncMock(return_value=(None, listings, store))):
        settings.browser_profile_dir = tmp_path
        settings.headless = True
        settings.browser_slow_mo_ms = 0
        asyncio.run(orchestrator.run_scan(settings, bundle))

    assert store.last_scan_date("back") == \
        store._conn.execute("SELECT date('now','localtime')").fetchone()[0]
    assert listings.scan_group.await_count == 2
    store.close()
```

Примечание для реализатора: сверься с фактической сигнатурой `BrowserSession(...)` и `_build(...)` в `orchestrator.py` (строки ~184–194) — набор патчей должен совпасть с реальными именами. Если `_build` закрывает `store` в `finally` (`store.close()`), проверку `last_scan_date` делай на том же объекте до закрытия — тест использует переданный `store`, а `run_scan` его же и закрывает; читай `last_scan_date` внутри, либо замени `store.close` на no-op через патч. Простейший вариант — `patch.object(store, "close", lambda: None)` в блоке `with`.

- [ ] **Step 2: Run test to verify it fails**

Run: `pytest tests/test_scan_marks_run.py -v`
Expected: FAIL — `last_scan_date("back")` возвращает `None` (штамп ещё не проставляется).

- [ ] **Step 3: Add `mark_scan_run` call in `run_scan`**

В `src/autopilot/orchestrator.py::run_scan`, в цикле по группам (сейчас):

```python
            for group in targets:
                await gate.wait_if_paused()
                logger.info("Скан группы «{}» запущен.", group.name)
                await listings.scan_group(group)
```

добавить штамп после успешного прохода группы:

```python
            for group in targets:
                await gate.wait_if_paused()
                logger.info("Скан группы «{}» запущен.", group.name)
                await listings.scan_group(group)
                store.mark_scan_run(settings.profile_name)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `pytest tests/test_scan_marks_run.py -v`
Expected: PASS.

- [ ] **Step 5: Run the full suite**

Run: `pytest -q`
Expected: PASS (весь набор зелёный).

- [ ] **Step 6: Commit**

```bash
cd ~/projects/work-autopilot
git add src/autopilot/orchestrator.py tests/test_scan_marks_run.py
git commit -m "feat(scan): run_scan штампует дату скана после каждой группы"
```

---

## Self-Review

- **Spec coverage:** §1 таблица `scan_runs` + `mark_scan_run` → Task 1 Step 3–4. §2 штамп в `run_scan` после группы → Task 2 Step 3. §3 `last_scan_date` из журнала с фолбэком → Task 1 Step 4. Ручные кнопки (дизайн: побочный эффект) — покрыто тем же `run_scan`, отдельной задачи не требуют. Тесты §Тесты (a/b/в) → Task 1 Steps 1 (`test_mark_scan_run_sets_today_without_queue_rows`, `test_last_scan_date_falls_back_to_queue_when_no_runs`, `test_mark_scan_run_idempotent_per_day`).
- **Placeholder scan:** нет TBD/TODO; весь код и команды приведены.
- **Type consistency:** `mark_scan_run(profile)` / `last_scan_date(profile)` — одинаковые сигнатуры в Task 1 и Task 2; ключ профиля `settings.profile_name` согласован со спекой.
