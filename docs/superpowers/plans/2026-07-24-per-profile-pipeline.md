# Per-Profile Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the job-application pipeline per-profile — own vacancy pool, own scan under the profile's browser, rich per-profile stats, and a cycle that rotates on "27 applies → clear chat → next profile".

**Architecture:** Two repos. **work-autopilot** (Python): `queue` becomes per-profile (composite PK), a new `events` log feeds counters, a lightweight `summary` CLI prints JSON to stdout (no torch), searches move into profile overlays. **health-widget** (Rust/egui): the widget reads `summary` stdout (no more state files), holds the turn-start marker in RAM, applies the new rotation rule, and renders the stats always-expanded.

**Tech Stack:** Python 3, SQLite (`sqlite3`), pydantic, playwright, pytest. Rust, egui/eframe, serde, serde_json.

## Global Constraints

- **No code comments.** Never add `//`, `///`, `#`, or docstrings to source. Existing comments in edited code must not be reintroduced when rewriting a block. (CLAUDE.md)
- UI strings and any necessary comments are in Russian, matching the project. (CLAUDE.md)
- Single branch `master` in both repos. Do not create git branches. (CLAUDE.md)
- Prefer shelling existing CLIs over new dependencies. Do NOT add new Rust crates (no rusqlite) — the widget talks to Python via the `summary` CLI over stdout. (CLAUDE.md idiom)
- `summary` must never trigger `Embedder.encode` (torch loads lazily on first encode). Keep it SQL-only.
- work-autopilot repo root: `~/projects/work-autopilot`. Python invoked via `.venv/bin/python -m autopilot ...`; tests via `.venv/bin/python -m pytest`.
- health-widget repo root: `~/projects/health-widget`. Build/test via `cargo`.
- Store DB rows use `datetime('now','localtime')` for timestamps (match existing schema).
- Profiles (order): `fullstack`, `back`, `llm`, `analyst` (from `PILOT_PROFILES` in `main.rs`; `fullstack` is the legacy default account).

---

## File Structure

**work-autopilot (Python)**
- `src/autopilot/store.py` — MODIFY. `profile` param across pool methods, `queue` migration to composite PK, `events` table + `log_event` + `count_events`, `profile_meta` top-N snapshot, `summary_payload`, `last_scan_date`.
- `src/autopilot/config.py` — MODIFY. Remove global union expectation; searches come from overlay. No structural change to `SiteConfig.groups()`.
- `src/autopilot/orchestrator.py` — MODIFY. Pass `profile` to Store pool methods; drop `scan.json`/`stats-*.json` writes; captcha callbacks call `log_event`.
- `src/autopilot/__main__.py` — MODIFY. Add `summary` subcommand (SQL-only path); remove `scan-status`.
- `src/autopilot/tasks/listings.py` — MODIFY. `record_application` path logs `applied`; write top-N snapshot after ranking.
- `src/autopilot/tasks/chat_dialog.py` — MODIFY. log `reply` when a reply is sent.
- `src/autopilot/tasks/chat_forms.py` — MODIFY. log `form_filled` on submit.
- `src/autopilot/tasks/enrich.py` — MODIFY. log `http_403` on `VacancyUnavailable`.
- `config/config.yaml`, `config/profiles/*.yaml` — MODIFY. Move searches into overlays; add `fullstack.yaml`.
- `tests/test_queue_per_profile.py`, `tests/test_events.py`, `tests/test_summary_cli.py` — CREATE.

**health-widget (Rust)**
- `src/pilot_summary.rs` — CREATE. `Summary` struct + `fetch(dir, bin, profile, since) -> Option<Summary>` (runs `summary`, parses stdout).
- `src/pilot_scan.rs`, `src/pilot_stats.rs` — DELETE (replaced by `pilot_summary.rs`).
- `src/pilot.rs` — MODIFY. Remove `refresh_scan_status`; add nothing else structural.
- `src/main.rs` — MODIFY. Constants, turn rule (`turn_over` pure fn + wiring in `maybe_rotate_profile`), turn-start marker + chat timer, stats rendering in `draw_autopilot`, use `pilot_summary`.

---

## Phase A — Python data layer (work-autopilot)

### Task A1: `queue` becomes per-profile (composite PK + migration)

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/store.py` (`_SCHEMA` queue block ~81-93, `_migrate` ~124-173)
- Test: `~/projects/work-autopilot/tests/test_queue_per_profile.py`

**Interfaces:**
- Produces: `queue` table with `PRIMARY KEY (profile, vacancy_id)` and a `profile TEXT NOT NULL` column. Legacy rows migrate to `profile='fullstack'`.

- [ ] **Step 1: Write the failing test** — create `tests/test_queue_per_profile.py`:

```python
import sqlite3
from pathlib import Path

from autopilot.store import Store


def _legacy_db(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE queue (
            vacancy_id TEXT PRIMARY KEY,
            group_name TEXT NOT NULL,
            title TEXT, url TEXT, body TEXT, embedding BLOB,
            published_at TEXT, employer TEXT,
            enriched INTEGER NOT NULL DEFAULT 0,
            unavailable INTEGER NOT NULL DEFAULT 0,
            found_at TEXT DEFAULT (datetime('now','localtime'))
        );
        INSERT INTO queue(vacancy_id, group_name, title) VALUES ('v1', 'Backend Go', 'Go dev');
        """
    )
    conn.commit()
    conn.close()


def test_migration_adds_profile_and_backfills_fullstack(tmp_path):
    db = tmp_path / "autopilot.db"
    _legacy_db(db)
    store = Store(db)
    cols = {r[1] for r in store._conn.execute("PRAGMA table_info(queue)")}
    assert "profile" in cols
    row = store._conn.execute(
        "SELECT profile FROM queue WHERE vacancy_id = 'v1'"
    ).fetchone()
    assert row[0] == "fullstack"
    store.close()


def test_same_vacancy_two_profiles_two_rows(tmp_path):
    store = Store(tmp_path / "autopilot.db")
    assert store.enqueue("back", "Backend Go", "v9", "t", "u", "body") is True
    assert store.enqueue("llm", "LLM инженер", "v9", "t", "u", "body") is True
    n = store._conn.execute("SELECT COUNT(*) FROM queue WHERE vacancy_id='v9'").fetchone()[0]
    assert n == 2
    store.close()
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd ~/projects/work-autopilot && .venv/bin/python -m pytest tests/test_queue_per_profile.py -v`
Expected: FAIL (enqueue signature has no `profile` yet / migration missing).

- [ ] **Step 3: Update `_SCHEMA` queue block** in `store.py` — replace the `CREATE TABLE ... queue (...)` block with:

```python
CREATE TABLE IF NOT EXISTS queue (
    profile      TEXT NOT NULL DEFAULT 'fullstack',
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

- [ ] **Step 4: Add queue migration** inside `Store._migrate`, after the existing `applications` migration block (before the `q_cols` reads), add:

```python
q_pk = self._conn.execute("PRAGMA table_info(queue)").fetchall()
q_has_profile = any(r[1] == "profile" for r in q_pk)
if q_pk and not q_has_profile:
    self._conn.executescript(
        """
        CREATE TABLE queue_new (
            profile      TEXT NOT NULL DEFAULT 'fullstack',
            vacancy_id   TEXT NOT NULL,
            group_name   TEXT NOT NULL,
            title        TEXT, url TEXT, body TEXT, embedding BLOB,
            published_at TEXT, employer TEXT,
            enriched     INTEGER NOT NULL DEFAULT 0,
            unavailable  INTEGER NOT NULL DEFAULT 0,
            found_at     TEXT DEFAULT (datetime('now','localtime')),
            PRIMARY KEY (profile, vacancy_id)
        );
        INSERT INTO queue_new
            (profile, vacancy_id, group_name, title, url, body, embedding,
             published_at, employer, enriched, unavailable, found_at)
            SELECT 'fullstack', vacancy_id, group_name, title, url, body, embedding,
                   published_at, employer, enriched, unavailable, found_at
            FROM queue;
        DROP TABLE queue;
        ALTER TABLE queue_new RENAME TO queue;
        """
    )
```

Note: the `q_cols` ADD COLUMN blocks below stay; after migration the columns already exist so those `if "x" not in q_cols` guards are computed from a fresh `PRAGMA` read — move the `q_cols = {...}` line to AFTER this migration block so it reflects the new table.

- [ ] **Step 5: Update `enqueue` signature** to take `profile` as first arg (full body change comes in Task A2; for now just make the test's `enqueue("back", ...)` shape valid). Since A2 rewrites all pool methods, implement `enqueue` fully here:

```python
def enqueue(
    self, profile: str, group_name: str, vacancy_id: str, title: str, url: str,
    body: str, embedding: bytes | None = None, employer: str = "",
) -> bool:
    known = self._conn.execute(
        "SELECT 1 FROM queue WHERE profile = ? AND vacancy_id = ?",
        (profile, vacancy_id),
    ).fetchone()
    if known is not None:
        if embedding is not None:
            self._conn.execute(
                "UPDATE queue SET embedding = ? "
                "WHERE profile = ? AND vacancy_id = ? AND embedding IS NULL",
                (embedding, profile, vacancy_id),
            )
        if employer:
            self._conn.execute(
                "UPDATE queue SET employer = ? "
                "WHERE profile = ? AND vacancy_id = ? AND (employer IS NULL OR employer = '')",
                (employer, profile, vacancy_id),
            )
        self._conn.commit()
        return False
    if self._clone_exists(profile, employer, title):
        return False
    self._conn.execute(
        "INSERT INTO queue(profile, vacancy_id, group_name, title, url, body, embedding, employer) "
        "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        (profile, vacancy_id, group_name, title, url, body, embedding, employer or None),
    )
    self._conn.commit()
    return True
```

(`_clone_exists` gains a `profile` param in A2; add the param now with a matching stub if needed, but A2 finalizes clone helpers. To keep this task green, temporarily scope `_clone_exists`/`_collapse_clones` by profile — see A2. If executing strictly task-by-task, fold A2's clone-helper edits into this step so tests pass.)

- [ ] **Step 6: Run tests to verify pass**

Run: `cd ~/projects/work-autopilot && .venv/bin/python -m pytest tests/test_queue_per_profile.py -v`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cd ~/projects/work-autopilot
git add src/autopilot/store.py tests/test_queue_per_profile.py
git commit -m "feat(store): пер-профильный queue (составной PK + миграция)"
```

---

### Task A2: Pool methods take `profile`

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/store.py` (`candidates_for`, `candidates_to_enrich`, `mark_unavailable`, `enrich_row`, `unenriched_count`, `queue_counts`, clone helpers `_collapse_clones`/`_collapse_group`/`_clone_group`/`_clone_exists`/`_applied_ids`)
- Test: `~/projects/work-autopilot/tests/test_queue_per_profile.py` (extend)

**Interfaces:**
- Produces (exact signatures):
  - `candidates_for(profile: str) -> list[tuple]` (groups filter removed — a profile's pool is already only its groups)
  - `candidates_to_enrich(profile: str, limit: int) -> list[tuple]`
  - `mark_unavailable(profile: str, vacancy_id: str) -> None`
  - `enrich_row(profile, vacancy_id, body, published_at, embedding, employer="") -> None`
  - `unenriched_count(profile: str) -> int`
  - `queue_counts(profile: str) -> dict[str, int]`

- [ ] **Step 1: Extend the test** — add to `tests/test_queue_per_profile.py`:

```python
def test_candidates_and_counts_isolated_by_profile(tmp_path):
    store = Store(tmp_path / "autopilot.db")
    store.enqueue("back", "Backend Go", "a", "Go dev", "u", "body")
    store.enqueue("llm", "LLM инженер", "b", "LLM eng", "u", "body")
    store._conn.execute("UPDATE queue SET enriched = 1")
    store._conn.commit()
    back = store.candidates_for("back")
    assert {r[0] for r in back} == {"a"}
    assert store.queue_counts("llm") == {"LLM инженер": 1}
    assert store.unenriched_count("back") == 0
    store.close()


def test_mark_unavailable_scoped(tmp_path):
    store = Store(tmp_path / "autopilot.db")
    store.enqueue("back", "Backend Go", "a", "Go dev", "u", "body")
    store.enqueue("llm", "LLM инженер", "a", "Go dev", "u", "body")
    store.mark_unavailable("back", "a")
    assert store._conn.execute(
        "SELECT unavailable FROM queue WHERE profile='back' AND vacancy_id='a'"
    ).fetchone()[0] == 1
    assert store._conn.execute(
        "SELECT unavailable FROM queue WHERE profile='llm' AND vacancy_id='a'"
    ).fetchone()[0] == 0
    store.close()
```

- [ ] **Step 2: Run to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_queue_per_profile.py -v`
Expected: FAIL (methods don't take `profile` / not scoped).

- [ ] **Step 3: Rewrite the pool + clone methods** in `store.py`. Replace each with the profile-scoped version:

```python
def _applied_ids(self, profile: str) -> set[str]:
    return {
        r[0] for r in self._conn.execute(
            "SELECT DISTINCT vacancy_id FROM applications WHERE profile = ?", (profile,)
        )
    }

def _clone_group(self, profile: str, key: tuple[str, str]) -> list[tuple]:
    return [
        (vid, enriched, found_at)
        for vid, employer, title, enriched, found_at in self._conn.execute(
            "SELECT vacancy_id, employer, title, enriched, found_at FROM queue "
            "WHERE profile = ? AND lower(trim(employer)) = ?",
            (profile, key[0]),
        )
        if _clone_key(employer, title) == key
    ]

def _collapse_clones(self) -> None:
    applied_by_profile: dict[str, set[str]] = {}
    groups: dict[tuple[str, tuple[str, str]], list[tuple]] = {}
    rows = self._conn.execute(
        "SELECT profile, vacancy_id, employer, title, enriched, found_at FROM queue "
        "WHERE employer IS NOT NULL AND employer <> ''"
    )
    for profile, vid, employer, title, enriched, found_at in rows:
        key = _clone_key(employer, title)
        if key is None:
            continue
        groups.setdefault((profile, key), []).append((vid, enriched, found_at))
    drop: list[tuple[str, str]] = []
    for (profile, _key), clones in groups.items():
        if len(clones) < 2:
            continue
        applied = applied_by_profile.setdefault(profile, self._applied_ids(profile))
        drop.extend((profile, c[0]) for c in _rank_clones(clones, applied)[1:])
    if drop:
        self._conn.executemany(
            "DELETE FROM queue WHERE profile = ? AND vacancy_id = ?", drop
        )
        self._conn.commit()

def _collapse_group(self, profile: str, employer: str, title: str) -> None:
    key = _clone_key(employer, title)
    if key is None:
        return
    clones = self._clone_group(profile, key)
    if len(clones) < 2:
        return
    drop = [(profile, c[0]) for c in _rank_clones(clones, self._applied_ids(profile))[1:]]
    self._conn.executemany(
        "DELETE FROM queue WHERE profile = ? AND vacancy_id = ?", drop
    )
    self._conn.commit()

def _clone_exists(self, profile: str, employer: str, title: str) -> bool:
    key = _clone_key(employer, title)
    if key is None:
        return False
    cur = self._conn.execute(
        "SELECT title FROM queue WHERE profile = ? AND lower(trim(employer)) = ?",
        (profile, key[0]),
    )
    return any(_norm(row[0]) == key[1] for row in cur)

def candidates_for(self, profile: str) -> list[tuple]:
    cur = self._conn.execute(
        "SELECT q.vacancy_id, q.group_name, q.title, q.url, q.body, q.embedding, "
        "       q.published_at, q.employer, q.enriched "
        "FROM queue q WHERE q.profile = ? AND q.unavailable = 0 AND NOT EXISTS ("
        "  SELECT 1 FROM applications a "
        "  WHERE a.vacancy_id = q.vacancy_id AND a.profile = ?"
        ")",
        (profile, profile),
    )
    return cur.fetchall()

def candidates_to_enrich(self, profile: str, limit: int) -> list[tuple]:
    cur = self._conn.execute(
        "SELECT vacancy_id, title, url FROM queue "
        "WHERE profile = ? AND enriched = 0 AND unavailable = 0 "
        "ORDER BY found_at DESC LIMIT ?",
        (profile, max(0, limit)),
    )
    return cur.fetchall()

def mark_unavailable(self, profile: str, vacancy_id: str) -> None:
    self._conn.execute(
        "UPDATE queue SET unavailable = 1 WHERE profile = ? AND vacancy_id = ?",
        (profile, vacancy_id),
    )
    self._conn.commit()

def enrich_row(
    self, profile: str, vacancy_id: str, body: str, published_at: str | None,
    embedding: bytes, employer: str = "",
) -> None:
    self._conn.execute(
        "UPDATE queue SET body = ?, published_at = ?, embedding = ?, enriched = 1 "
        "WHERE profile = ? AND vacancy_id = ?",
        (body, published_at, embedding, profile, vacancy_id),
    )
    if not employer:
        self._conn.commit()
        return
    self._conn.execute(
        "UPDATE queue SET employer = ? WHERE profile = ? AND vacancy_id = ?",
        (employer, profile, vacancy_id),
    )
    self._conn.commit()
    row = self._conn.execute(
        "SELECT title FROM queue WHERE profile = ? AND vacancy_id = ?",
        (profile, vacancy_id),
    ).fetchone()
    if row is not None:
        self._collapse_group(profile, employer, row[0])

def unenriched_count(self, profile: str) -> int:
    cur = self._conn.execute(
        "SELECT COUNT(*) FROM queue WHERE profile = ? AND enriched = 0 AND unavailable = 0",
        (profile,),
    )
    return int(cur.fetchone()[0])

def queue_counts(self, profile: str) -> dict[str, int]:
    cur = self._conn.execute(
        "SELECT group_name, COUNT(*) FROM queue WHERE profile = ? GROUP BY group_name",
        (profile,),
    )
    return {row[0]: int(row[1]) for row in cur.fetchall()}
```

Also delete `write_scan_status` and `write_summary` and the old `summary` method (replaced in A5/A6). Remove `candidates_for`'s old `groups` param usages.

- [ ] **Step 4: Run tests to verify pass**

Run: `.venv/bin/python -m pytest tests/test_queue_per_profile.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/autopilot/store.py tests/test_queue_per_profile.py
git commit -m "feat(store): пул-методы принимают profile, дедуп/клоны в пределах профиля"
```

---

### Task A3: `events` table + `log_event` + `count_events`

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/store.py` (`_SCHEMA`, new methods)
- Test: `~/projects/work-autopilot/tests/test_events.py`

**Interfaces:**
- Produces:
  - `log_event(profile: str, kind: str) -> None`
  - `count_events(profile: str, since: str | None) -> dict[str, dict]` returning `{kind: {"turn": int, "total": int}}` for kinds `applied,reply,captcha,form_filled,http_403,expired`. `turn` counts `ts >= since` (all-time if `since` is None).

- [ ] **Step 1: Write the failing test** — `tests/test_events.py`:

```python
from autopilot.store import Store

KINDS = ["applied", "reply", "captcha", "form_filled", "http_403", "expired"]


def test_log_and_count(tmp_path):
    store = Store(tmp_path / "autopilot.db")
    store.log_event("back", "applied")
    store.log_event("back", "applied")
    store.log_event("back", "captcha")
    store.log_event("llm", "applied")
    counts = store.count_events("back", since=None)
    assert counts["applied"]["total"] == 2
    assert counts["captcha"]["total"] == 1
    assert counts["applied"]["turn"] == 2  # since=None -> turn == total
    assert all(k in counts for k in KINDS)
    assert store.count_events("llm", None)["applied"]["total"] == 1
    store.close()


def test_turn_window_by_since(tmp_path):
    store = Store(tmp_path / "autopilot.db")
    store._conn.execute(
        "INSERT INTO events(profile, kind, ts) VALUES ('back','applied','2000-01-01 00:00:00')"
    )
    store._conn.commit()
    store.log_event("back", "applied")
    counts = store.count_events("back", since="2020-01-01 00:00:00")
    assert counts["applied"]["total"] == 2
    assert counts["applied"]["turn"] == 1
    store.close()
```

- [ ] **Step 2: Run to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_events.py -v`
Expected: FAIL (`log_event` missing).

- [ ] **Step 3: Add schema** — append to `_SCHEMA` string in `store.py`:

```python
CREATE TABLE IF NOT EXISTS events (
    profile TEXT NOT NULL,
    kind    TEXT NOT NULL,
    ts      TEXT DEFAULT (datetime('now','localtime'))
);
CREATE INDEX IF NOT EXISTS events_profile_ts ON events(profile, ts);
```

- [ ] **Step 4: Add methods** to `Store`:

```python
_EVENT_KINDS = ("applied", "reply", "captcha", "form_filled", "http_403", "expired")

def log_event(self, profile: str, kind: str) -> None:
    self._conn.execute(
        "INSERT INTO events(profile, kind) VALUES (?, ?)", (profile, kind)
    )
    self._conn.commit()

def count_events(self, profile: str, since: str | None) -> dict:
    out = {k: {"turn": 0, "total": 0} for k in self._EVENT_KINDS}
    for kind, total in self._conn.execute(
        "SELECT kind, COUNT(*) FROM events WHERE profile = ? GROUP BY kind", (profile,)
    ):
        if kind in out:
            out[kind]["total"] = int(total)
    if since is None:
        for k in out:
            out[k]["turn"] = out[k]["total"]
        return out
    for kind, turn in self._conn.execute(
        "SELECT kind, COUNT(*) FROM events WHERE profile = ? AND ts >= ? GROUP BY kind",
        (profile, since),
    ):
        if kind in out:
            out[kind]["turn"] = int(turn)
    return out
```

- [ ] **Step 5: Run tests to verify pass**

Run: `.venv/bin/python -m pytest tests/test_events.py -v`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/autopilot/store.py tests/test_events.py
git commit -m "feat(store): таблица events + log_event/count_events"
```

---

### Task A4: `profile_meta` top-N snapshot + write from ranking

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/store.py` (schema + methods)
- Modify: `~/projects/work-autopilot/src/autopilot/tasks/listings.py` (`apply_from_queue` after ranking ~214)
- Test: `~/projects/work-autopilot/tests/test_events.py` (extend)

**Interfaces:**
- Produces:
  - `set_top(profile: str, titles: list[str]) -> None`
  - `get_top(profile: str) -> list[str]`

- [ ] **Step 1: Extend the test** — add to `tests/test_events.py`:

```python
def test_top_snapshot_roundtrip(tmp_path):
    store = Store(tmp_path / "autopilot.db")
    store.set_top("back", ["Go dev", "Rust dev"])
    assert store.get_top("back") == ["Go dev", "Rust dev"]
    store.set_top("back", ["Only one"])
    assert store.get_top("back") == ["Only one"]
    assert store.get_top("llm") == []
    store.close()
```

- [ ] **Step 2: Run to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_events.py::test_top_snapshot_roundtrip -v`
Expected: FAIL.

- [ ] **Step 3: Add schema + methods.** Append to `_SCHEMA`:

```python
CREATE TABLE IF NOT EXISTS profile_meta (
    profile    TEXT PRIMARY KEY,
    top_json   TEXT,
    updated_at TEXT DEFAULT (datetime('now','localtime'))
);
```

Add methods:

```python
def set_top(self, profile: str, titles: list[str]) -> None:
    self._conn.execute(
        "INSERT INTO profile_meta(profile, top_json, updated_at) "
        "VALUES (?, ?, datetime('now','localtime')) "
        "ON CONFLICT(profile) DO UPDATE SET "
        "top_json = excluded.top_json, updated_at = excluded.updated_at",
        (profile, json.dumps(titles, ensure_ascii=False)),
    )
    self._conn.commit()

def get_top(self, profile: str) -> list[str]:
    row = self._conn.execute(
        "SELECT top_json FROM profile_meta WHERE profile = ?", (profile,)
    ).fetchone()
    if not row or not row[0]:
        return []
    try:
        return list(json.loads(row[0]))
    except (ValueError, TypeError):
        return []
```

- [ ] **Step 4: Write top-N from ranking** — in `listings.py::apply_from_queue`, right after `ranked = self._rank_candidates()` (~line 214), add:

```python
self._store.set_top(self._profile, [row[3] for row in ranked[:5]])
```

(`_rank_candidates` returns tuples `(score, vid, group, title, url, body, employer)` — index 3 is title. `self._profile` is the profile name field on `ListingHunter`.)

- [ ] **Step 5: Run tests to verify pass**

Run: `.venv/bin/python -m pytest tests/test_events.py -v`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/autopilot/store.py src/autopilot/tasks/listings.py tests/test_events.py
git commit -m "feat(store): снимок топ-N профиля (profile_meta), пишется из ранжирования"
```

---

### Task A5: `summary_payload` + `last_scan_date`

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/store.py`
- Test: `~/projects/work-autopilot/tests/test_summary_cli.py`

**Interfaces:**
- Produces:
  - `last_scan_date(profile: str) -> str | None` — `date(MAX(found_at))`
  - `summary_payload(profile: str, since: str | None, group_names: list[str], daily_limit: int) -> dict`

- [ ] **Step 1: Write the failing test** — `tests/test_summary_cli.py`:

```python
from autopilot.store import Store


def test_summary_payload_shape(tmp_path):
    store = Store(tmp_path / "autopilot.db")
    store.enqueue("back", "Backend Go", "a", "Go dev", "u", "body")
    store.enqueue("back", "Rust разработчик", "b", "Rust dev", "u", "body")
    store.log_event("back", "applied")
    store.set_top("back", ["Go dev"])
    p = store.summary_payload(
        "back", since=None, group_names=["Backend Go", "Rust разработчик"], daily_limit=200
    )
    assert p["applied"]["total"] == 1
    assert {g["name"]: g["new"] for g in p["groups"]} == {"Backend Go": 1, "Rust разработчик": 1}
    assert p["unenriched"] == 2
    assert p["top"] == ["Go dev"]
    assert p["daily_limit"] == 200
    assert "captcha" in p and "expired" in p
    store.close()
```

- [ ] **Step 2: Run to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_summary_cli.py -v`
Expected: FAIL.

- [ ] **Step 3: Add methods** to `Store`:

```python
def last_scan_date(self, profile: str) -> str | None:
    row = self._conn.execute(
        "SELECT date(MAX(found_at)) FROM queue WHERE profile = ?", (profile,)
    ).fetchone()
    return row[0] if row else None

def new_by_group(self, profile: str, group_names: list[str]) -> list[dict]:
    counts = {
        row[0]: int(row[1]) for row in self._conn.execute(
            "SELECT group_name, COUNT(*) FROM queue "
            "WHERE profile = ? AND enriched = 0 AND unavailable = 0 GROUP BY group_name",
            (profile,),
        )
    }
    return [{"name": g, "new": counts.get(g, 0)} for g in group_names]

def summary_payload(
    self, profile: str, since: str | None, group_names: list[str], daily_limit: int
) -> dict:
    counts = self.count_events(profile, since)
    now = self._conn.execute("SELECT datetime('now','localtime')").fetchone()[0]
    return {
        "applied": counts["applied"],
        "replies": counts["reply"],
        "captcha": counts["captcha"],
        "forms": counts["form_filled"],
        "http_403": counts["http_403"],
        "expired": counts["expired"],
        "groups": self.new_by_group(profile, group_names),
        "unenriched": self.unenriched_count(profile),
        "last_scan_date": self.last_scan_date(profile),
        "top": self.get_top(profile),
        "applied_today": self.applied_today(profile),
        "daily_limit": daily_limit,
        "updated_at": now,
    }
```

- [ ] **Step 4: Run tests to verify pass**

Run: `.venv/bin/python -m pytest tests/test_summary_cli.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/autopilot/store.py tests/test_summary_cli.py
git commit -m "feat(store): summary_payload + last_scan_date + new_by_group"
```

---

### Task A6: `summary` CLI (SQL-only, no torch) + remove `scan-status`

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/__main__.py`
- Test: `~/projects/work-autopilot/tests/test_summary_cli.py` (extend)

**Interfaces:**
- Consumes: `Store.summary_payload`, `load_site_bundle`, `_apply_profile`.
- Produces: CLI `autopilot summary --profile <name> [--since <iso>]` prints one JSON object to stdout; no `.encode()` call, torch not imported.

- [ ] **Step 1: Extend the test** (subprocess, asserts JSON on stdout and torch absent):

```python
import json
import os
import subprocess
import sys


def test_summary_cli_prints_json_without_torch(tmp_path):
    from autopilot.store import Store
    db = tmp_path / "autopilot.db"
    store = Store(db)
    store.enqueue("fullstack", "Fullstack Python", "a", "Py dev", "u", "body")
    store.log_event("fullstack", "applied")
    store.close()

    env = dict(os.environ, DB_PATH=str(db))
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    code = (
        "import sys, autopilot.__main__ as m; m.main(); "
        "assert 'torch' not in sys.modules, 'torch imported!'"
    )
    proc = subprocess.run(
        [sys.executable, "-c", code, "summary", "--profile", "fullstack"],
        cwd=root, env=env, capture_output=True, text=True,
    )
    assert proc.returncode == 0, proc.stderr
    payload = json.loads(proc.stdout.strip().splitlines()[-1])
    assert payload["applied"]["total"] == 1
    assert payload["unenriched"] == 1
```

(The `-c` wrapper runs `main()` then asserts torch stayed unimported. `sys.argv` for `main()` is `['-c', 'summary', '--profile', 'fullstack']`; argparse reads from `sys.argv[1:]`, so pass command/flags as script args — confirm `main()` uses `parser.parse_args()` with default `sys.argv`.)

- [ ] **Step 2: Run to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_summary_cli.py::test_summary_cli_prints_json_without_torch -v`
Expected: FAIL (`summary` not a choice).

- [ ] **Step 3: Add the subcommand.** In `__main__.py`:
  - Add `"summary"` to the `command` choices; remove `"scan-status"`.
  - Add argument `parser.add_argument("--since", default=None, help="summary: ISO-время старта хода (окно 'за цикл')")`.
  - Replace the `elif args.command == "scan-status":` branch with:

```python
    elif args.command == "summary":
        import json as _json
        store = Store(settings.db_path)
        try:
            payload = store.summary_payload(
                settings.profile_name,
                args.since,
                [g.name for g in bundle.site.groups()],
                bundle.listings_filter.daily_limit,
            )
        finally:
            store.close()
        print(_json.dumps(payload, ensure_ascii=False))
        return
```

  - Update the module docstring command list (keep it comment-free per constraints — this is the module docstring; the project bans docstrings, so instead DELETE the outdated `scan-status` line rather than adding prose). If the docstring already exists, edit the `scan-status` line to describe `summary`; do not add new docstrings.

- [ ] **Step 4: Run tests to verify pass**

Run: `.venv/bin/python -m pytest tests/test_summary_cli.py -v`
Expected: PASS (JSON printed, torch not imported).

- [ ] **Step 5: Manual smoke**

Run: `cd ~/projects/work-autopilot && .venv/bin/python -m autopilot summary --profile fullstack | tail -1`
Expected: a JSON line with `applied`, `groups`, `unenriched`, `top`.

- [ ] **Step 6: Commit**

```bash
git add src/autopilot/__main__.py tests/test_summary_cli.py
git commit -m "feat(cli): лёгкая команда summary (JSON в stdout, без torch); убрал scan-status"
```

---

### Task A7: Wire `log_event` into existing action points

**Files:**
- Modify: `~/projects/work-autopilot/src/autopilot/tasks/listings.py` (record_application applied path)
- Modify: `~/projects/work-autopilot/src/autopilot/tasks/chat_dialog.py` (reply sent)
- Modify: `~/projects/work-autopilot/src/autopilot/tasks/chat_forms.py` (form submitted)
- Modify: `~/projects/work-autopilot/src/autopilot/tasks/enrich.py` (VacancyUnavailable → http_403)
- Modify: `~/projects/work-autopilot/src/autopilot/store.py` (`mark_unavailable` logs `expired`)
- Modify: `~/projects/work-autopilot/src/autopilot/orchestrator.py` (captcha ping logs `captcha`)
- Test: `~/projects/work-autopilot/tests/test_events.py` (extend for mark_unavailable → expired)

**Interfaces:**
- Consumes: `Store.log_event`. All these call sites already hold a `store`/`_store` and a profile.

- [ ] **Step 1: Add the `expired` test** — in `tests/test_events.py`:

```python
def test_mark_unavailable_logs_expired(tmp_path):
    store = Store(tmp_path / "autopilot.db")
    store.enqueue("back", "Backend Go", "a", "Go dev", "u", "body")
    store.mark_unavailable("back", "a")
    assert store.count_events("back", None)["expired"]["total"] == 1
    store.close()
```

- [ ] **Step 2: Run to verify it fails**

Run: `.venv/bin/python -m pytest tests/test_events.py::test_mark_unavailable_logs_expired -v`
Expected: FAIL.

- [ ] **Step 3: `mark_unavailable` logs expired** — in `store.py`, change body to also log:

```python
def mark_unavailable(self, profile: str, vacancy_id: str) -> None:
    self._conn.execute(
        "UPDATE queue SET unavailable = 1 WHERE profile = ? AND vacancy_id = ?",
        (profile, vacancy_id),
    )
    self._conn.commit()
    self.log_event(profile, "expired")
```

- [ ] **Step 4: `applied` event** — in `listings.py`, at the site where a SUCCESS application is recorded (`record_application(..., status="applied", ...)`; find the branch storing the "applied" status after a real submit, ~line 138/255), add right after that `record_application`:

```python
self._store.log_event(self._profile, "applied")
```

Add it ONLY on the genuine "applied" success branches (not `failed`/`already`/`questions`).

- [ ] **Step 5: `reply` event** — in `chat_dialog.py`, at the point a reply is actually sent (where `answered_any = True` is set after a successful send, ~line 58), add:

```python
self._store.log_event(self._profile, "reply")
```

Confirm `chat_dialog` holds `self._store` and a profile attribute; if the profile isn't on the object, thread it in from `ChatResponder` (which is built with `settings.profile_name` in orchestrator `_build`). Add a `profile` ctor arg to the dialog if needed.

- [ ] **Step 6: `form_filled` event** — in `chat_forms.py`, where `status == "submitted"` (~line 56, the "really submitted" branch), add:

```python
self._store.log_event(self._profile, "form_filled")
```

(Thread `profile`/`store` into `chat_forms` if not present, same as Step 5.)

- [ ] **Step 7: `http_403` event** — in `enrich.py`, inside `except VacancyUnavailable:` (~line 71), before/after `self._store.mark_unavailable(...)`, add:

```python
self._store.log_event(self._profile, "http_403")
```

(`PoolEnricher` needs a `profile`; it's built in `run_enrich` with `settings.profile_name` — add the ctor arg. Note `mark_unavailable` now takes `profile`; update the call to `self._store.mark_unavailable(self._profile, vid)`.)

- [ ] **Step 8: `captcha` event** — in `orchestrator.py`, inside `ping_captcha` (~line 41) and `_ping_captcha` (enrich, ~line 208), add before the send:

```python
store.log_event(settings.profile_name, "captcha")
```

(`ping_captcha` has `store` in scope via `_build`; `_ping_captcha` in `run_enrich` has `store` in scope. Use the profile from `settings.profile_name`.)

- [ ] **Step 9: Run the full python suite**

Run: `cd ~/projects/work-autopilot && .venv/bin/python -m pytest -q`
Expected: PASS (all tests; existing suite must stay green — expect signature updates to old tests that call `enqueue`/`candidates_for`/`mark_unavailable`/`enrich_row`/`candidates_to_enrich` without a profile — update those call sites in the existing tests to pass a profile).

- [ ] **Step 10: Commit**

```bash
git add src/autopilot/
git commit -m "feat(events): инкремент applied/reply/form_filled/http_403/expired/captcha в точках действий"
```

---

### Task A8: Move searches into profile overlays; orchestrator scan/enrich per profile; drop state files

**Files:**
- Modify: `~/projects/work-autopilot/config/config.yaml` (remove `listings_urls` union; keep fallback `listings_url`, filter, prompts, selectors)
- Create: `~/projects/work-autopilot/config/profiles/fullstack.yaml` (fullstack searches, resume, contacts — move from base)
- Modify: `~/projects/work-autopilot/config/profiles/back.yaml`, `llm.yaml`, `analyst.yaml` — each gets `site.listings_urls`
- Modify: `~/projects/work-autopilot/src/autopilot/orchestrator.py` — Store pool calls pass `profile`; delete `write_scan_status`/`write_summary` calls and `scan.json`/`stats-*.json` paths
- Modify: `~/projects/work-autopilot/src/autopilot/tasks/listings.py`, `tasks/enrich.py` — Store pool calls pass `profile`

**Interfaces:**
- Consumes: A2 method signatures.
- No new test (config + wiring); verified by A9 smoke + full suite.

- [ ] **Step 1: Create `config/profiles/fullstack.yaml`** with the searches currently in base `config.yaml` for fullstack/frontend/LLM, plus its existing resume/contacts (move `telegram_chat_id`, `resume_url`, `profile`, `contacts`, `form_facts` from base to here). Example head:

```yaml
telegram_chat_id: "505943801"
resume_url: "https://hh.ru/resume/6053493aff10a5225d0039ed1f524633514d75"
site:
  listings_urls:
    - {name: "Fullstack Node.js", url: "https://hh.ru/search/vacancy?text=Fullstack+Node.js&work_format=REMOTE&order_by=publication_time&search_period=30&ored_clusters=true"}
    - {name: "Fullstack Python",  url: "https://hh.ru/search/vacancy?text=Fullstack+Python&work_format=REMOTE&order_by=publication_time&search_period=30&ored_clusters=true"}
    - {name: "Fullstack Golang",  url: "https://hh.ru/search/vacancy?text=Fullstack+Golang&work_format=REMOTE&order_by=publication_time&search_period=30&ored_clusters=true"}
    - {name: "Frontend React",    url: "https://hh.ru/search/vacancy?text=Frontend+React&work_format=REMOTE&order_by=publication_time&search_period=30&ored_clusters=true"}
    - {name: "Node.js разработчик", url: "https://hh.ru/search/vacancy?text=Node.js+разработчик&work_format=REMOTE&order_by=publication_time&search_period=30&ored_clusters=true"}
    - {name: "LLM инженер",       url: "https://hh.ru/search/vacancy?text=LLM+инженер&work_format=REMOTE&order_by=publication_time&search_period=30&ored_clusters=true"}
# profile:, contacts:, form_facts: — перенести из config.yaml сюда
```

- [ ] **Step 2: Add `site.listings_urls` to `back.yaml`** (Backend Go/Python, Node.js, Rust) and `llm.yaml`, `analyst.yaml` with their role URLs. Remove the now-unused `groups:` list from each overlay (groups derive from `listings_urls`). Keep `_apply_profile` mapping fullstack→overlay working: note `_apply_profile` currently sets `overlay=None` for fullstack. Change it so fullstack ALSO loads `config/profiles/fullstack.yaml`:

In `__main__.py::_apply_profile`, replace the `if name != "fullstack":` block so the overlay path is computed for every profile:

```python
settings.profile_name = name
if name != "fullstack":
    settings.browser_profile_dir = (
        settings.db_path.parent / "profiles" / name / "browser-profile"
    )
overlay = settings.config_path.parent / "profiles" / f"{name}.yaml"
```

(fullstack keeps legacy `data/browser-profile`; only its overlay is now explicit.)

- [ ] **Step 3: Strip base `config.yaml`** — delete the `site.listings_urls` union block and the personal `profile:`/`contacts:`/`form_facts:`/`telegram_chat_id`/`resume_url` (moved to fullstack.yaml). Keep `site.name`, poll seconds, `listings_url` fallback, `max_pages`, `listings_filter`, `prompts`, selectors.

- [ ] **Step 4: Thread `profile` into orchestrator/tasks Store calls.** In `orchestrator.py`, remove `scan_path`/`stats_path` and all `store.write_scan_status(...)` / `store.write_summary(...)` calls in `run_scan`, `run_enrich`, `run_forever`. In `listings.py` and `enrich.py`, update every Store pool call to pass `self._profile`: `enqueue`, `candidates_for`, `candidates_to_enrich`, `mark_unavailable`, `enrich_row`, `unenriched_count`, `queue_counts`. (`ListingHunter` has `self._profile`; `PoolEnricher` gets it in A7 Step 7.)

- [ ] **Step 5: Run the full suite + config load smoke**

Run:
```bash
cd ~/projects/work-autopilot
.venv/bin/python -c "from pathlib import Path; from autopilot.config import load_site_bundle; b=load_site_bundle(Path('config/config.yaml'), Path('config/profiles/back.yaml')); print([g.name for g in b.site.groups()])"
.venv/bin/python -m pytest -q
```
Expected: back's group names print (Backend Go, …); suite PASS.

- [ ] **Step 6: Commit**

```bash
git add config/ src/autopilot/
git commit -m "feat: поиски в оверлеях профилей; скан/enrich по profile; убраны файлы состояния"
```

---

## Phase B — Rust widget (health-widget)

### Task B1: `pilot_summary` bridge (read `summary` stdout)

**Files:**
- Create: `~/projects/health-widget/src/pilot_summary.rs`
- Delete: `~/projects/health-widget/src/pilot_scan.rs`, `~/projects/health-widget/src/pilot_stats.rs`
- Modify: `~/projects/health-widget/src/pilot.rs` (remove `refresh_scan_status`)
- Modify: `~/projects/health-widget/src/main.rs` (module decls; call sites in later tasks)

**Interfaces:**
- Produces:
  - `pub struct Counter { pub turn: i64, pub total: i64 }`
  - `pub struct Group { pub name: String, pub new: i64 }`
  - `pub struct Summary { applied, replies, captcha, forms, http_403, expired: Counter; groups: Vec<Group>; unenriched: i64; last_scan_date: Option<String>; top: Vec<String>; applied_today: i64; daily_limit: i64 }`
  - `pub fn fetch(dir: &Path, bin: &Path, profile: &str, since: Option<&str>) -> Option<Summary>`

- [ ] **Step 1: Write the failing test** — put a unit test in `pilot_summary.rs` that parses a JSON sample:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_summary_json() {
        let js = r#"{
          "applied":{"turn":12,"total":340},"replies":{"turn":3,"total":88},
          "captcha":{"turn":1,"total":20},"forms":{"turn":0,"total":7},
          "http_403":{"turn":5,"total":210},"expired":{"turn":4,"total":190},
          "groups":[{"name":"Backend Go","new":8},{"name":"Rust","new":2}],
          "unenriched":10,"last_scan_date":"2026-07-24","top":["Go dev"],
          "applied_today":12,"daily_limit":200,"updated_at":"x"
        }"#;
        let s: Summary = serde_json::from_str(js).unwrap();
        assert_eq!(s.applied.turn, 12);
        assert_eq!(s.groups.len(), 2);
        assert_eq!(s.top, vec!["Go dev".to_string()]);
        assert_eq!(s.last_scan_date.as_deref(), Some("2026-07-24"));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd ~/projects/health-widget && cargo test pilot_summary`
Expected: FAIL (module doesn't exist).

- [ ] **Step 3: Implement `pilot_summary.rs`:**

```rust
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

#[derive(Deserialize, Clone, Default)]
pub struct Counter {
    #[serde(default)]
    pub turn: i64,
    #[serde(default)]
    pub total: i64,
}

#[derive(Deserialize, Clone, Default)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub new: i64,
}

#[derive(Deserialize, Clone, Default)]
pub struct Summary {
    #[serde(default)]
    pub applied: Counter,
    #[serde(default)]
    pub replies: Counter,
    #[serde(default)]
    pub captcha: Counter,
    #[serde(default)]
    pub forms: Counter,
    #[serde(default)]
    pub http_403: Counter,
    #[serde(default)]
    pub expired: Counter,
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub unenriched: i64,
    #[serde(default)]
    pub last_scan_date: Option<String>,
    #[serde(default)]
    pub top: Vec<String>,
    #[serde(default)]
    pub applied_today: i64,
    #[serde(default)]
    pub daily_limit: i64,
}

pub fn fetch(dir: &Path, bin: &Path, profile: &str, since: Option<&str>) -> Option<Summary> {
    let mut cmd = Command::new(bin);
    cmd.arg("summary").args(["--profile", profile]);
    if let Some(s) = since {
        cmd.args(["--since", s]);
    }
    let out = cmd.current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text.lines().rev().find(|l| l.trim_start().starts_with('{'))?;
    serde_json::from_str(last).ok()
}
```

- [ ] **Step 4: Delete old modules + refs.** Remove `src/pilot_scan.rs`, `src/pilot_stats.rs`. In `main.rs` remove `mod pilot_scan;`/`mod pilot_stats;` and add `mod pilot_summary;`. Remove `pub fn refresh_scan_status` from `pilot.rs`. Compilation will break at old call sites — those are fixed in B3/B4; for THIS task, keep it compiling by temporarily leaving the old `autopilot.scan`/`autopilot.stats` field types — NO: to keep the task self-contained, do B1+B3+B4 field/type swaps together if the reviewer needs green compile. If executing strictly, mark B1 as "compiles after B4"; prefer to land B1–B4 as one reviewable unit.

- [ ] **Step 5: Run the module test**

Run: `cargo test pilot_summary`
Expected: PASS (unit test compiles in isolation even if the binary has pending edits — run `cargo test --lib pilot_summary` if needed).

- [ ] **Step 6: Commit** (fold with B4 if compile requires)

```bash
cd ~/projects/health-widget
git add src/pilot_summary.rs src/pilot.rs src/main.rs
git rm src/pilot_scan.rs src/pilot_stats.rs
git commit -m "feat(widget): мост pilot_summary — читает summary из stdout, без файлов"
```

---

### Task B2: Turn rule (pure function + constants) with tests

**Files:**
- Modify: `~/projects/health-widget/src/main.rs` (constants ~60, new `turn_over` fn near `decide_apply_chain` ~85, tests ~3640)

**Interfaces:**
- Produces:
  - `const APPLY_TARGET: i64 = 27;`
  - `const CHAT_REPLY_CAP: i64 = 40;`
  - `const CHAT_TIME_CAP: Duration = Duration::from_secs(30 * 60);`
  - `fn apply_part_done(applied_in_turn: i64, pool_exhausted: bool) -> bool`
  - `fn chat_part_done(replied_in_turn: i64, chats_empty: bool, chat_elapsed: Duration) -> bool`

- [ ] **Step 1: Write failing tests** — in the `#[cfg(test)] mod tests` in `main.rs`:

```rust
#[test]
fn apply_part_done_on_target_or_exhaustion() {
    assert!(apply_part_done(27, false));
    assert!(apply_part_done(5, true));
    assert!(!apply_part_done(26, false));
}

#[test]
fn chat_part_done_on_empty_cap_or_time() {
    use std::time::Duration;
    assert!(chat_part_done(0, true, Duration::from_secs(1)));
    assert!(chat_part_done(40, false, Duration::from_secs(1)));
    assert!(chat_part_done(3, false, Duration::from_secs(31 * 60)));
    assert!(!chat_part_done(3, false, Duration::from_secs(60)));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test apply_part_done chat_part_done`
Expected: FAIL (functions undefined).

- [ ] **Step 3: Add constants + functions.** Replace `const APPLY_BATCH_SIZE: i64 = 27;` with the three constants above, and add near `decide_apply_chain`:

```rust
fn apply_part_done(applied_in_turn: i64, pool_exhausted: bool) -> bool {
    pool_exhausted || applied_in_turn >= APPLY_TARGET
}

fn chat_part_done(replied_in_turn: i64, chats_empty: bool, chat_elapsed: Duration) -> bool {
    chats_empty || replied_in_turn >= CHAT_REPLY_CAP || chat_elapsed >= CHAT_TIME_CAP
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test apply_part_done chat_part_done`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "feat(widget): правило хода — apply_part_done/chat_part_done + константы 27/40/30м"
```

---

### Task B3: Turn state (marker + chat timer) and rotation wiring

**Files:**
- Modify: `~/projects/health-widget/src/main.rs` (`Autopilot` struct fields, `maybe_rotate_profile` ~846, lap helpers, `update` tick)

**Interfaces:**
- Consumes: B1 `pilot_summary::{fetch, Summary}`, B2 `apply_part_done`/`chat_part_done`.
- Produces: `Autopilot` gains `summary: Option<pilot_summary::Summary>`, `turn_started_at: Option<chrono/SystemTime iso>` (store as `String` ISO for `--since` + `Instant` for elapsed), `chat_started_at: Option<Instant>`.

- [ ] **Step 1: Replace summary polling.** Where the widget currently loads `stats-<profile>.json`/`scan.json` (the periodic refresh that set `autopilot.stats`/`autopilot.scan`), replace with a throttled `pilot_summary::fetch`:

```rust
let since = self.autopilot.turn_since.clone();
if let Some(s) = pilot_summary::fetch(
    &self.cfg.autopilot_dir, &self.cfg.autopilot_bin, &self.autopilot.profile, since.as_deref(),
) {
    self.autopilot.summary = Some(s);
}
```

Throttle to ~every 3s using an `Instant` gate field `summary_next: Instant` (fetch only when `Instant::now() >= summary_next`, then set `summary_next = now + Duration::from_secs(3)`). This replaces the mtime-based file reload. Remove `autopilot.stats`, `autopilot.stats_mtime`, `autopilot.scan`, `autopilot.scan_mtime`, `batch_baseline` fields and their uses (batch logic is superseded).

- [ ] **Step 2: Set turn marker on entering a profile's apply lap.** In `enter_apply_lap` and wherever a new profile becomes current (`maybe_rotate_profile` success arm, `advance_chat`), set:

```rust
self.autopilot.turn_since = Some(now_iso());  // "YYYY-MM-DD HH:MM:SS" localtime
self.autopilot.turn_start = Some(Instant::now());
self.autopilot.chat_started_at = None;
```

Add a helper `fn now_iso() -> String` using `chrono::Local::now().format("%Y-%m-%d %H:%M:%S")` IF chrono is already a dep; else format via `time`/existing telemetry timestamp helper. (Check `telemetry.rs` for an existing localtime formatter and reuse it — do not add a crate.)

- [ ] **Step 3: Rewrite `maybe_rotate_profile`** to use the turn rule. Replace its body:

```rust
fn maybe_rotate_profile(&mut self) {
    if self.autopilot.want != Some(pilot::Phase::Apply) {
        return;
    }
    let Some(sum) = &self.autopilot.summary else { return; };
    let over_limit = sum.daily_limit > 0 && sum.applied_today >= sum.daily_limit;
    let applied_turn = sum.applied.turn;
    let pool_exhausted = self.autopilot.applies_exhausted;
    if !over_limit && !apply_part_done(applied_turn, pool_exhausted) {
        return;
    }
    // apply-часть закрыта → перейти в чат-часть хода (см. Step 4), не сразу ротация
    self.enter_chat_part();
}
```

Add `enter_chat_part` (starts chat phase for the SAME profile, sets `chat_started_at = Instant::now()`), and a `maybe_finish_chat_part` called each tick:

```rust
fn maybe_finish_chat_part(&mut self) {
    if self.autopilot.want != Some(pilot::Phase::Chat) || !self.autopilot.cycle {
        return;
    }
    let Some(sum) = &self.autopilot.summary else { return; };
    let elapsed = self.autopilot.chat_started_at
        .map(|t| t.elapsed()).unwrap_or_default();
    let chats_empty = /* reuse existing empty-chat-pass detection (process finished a pass with 0 handled) */;
    if chat_part_done(sum.replies.turn, chats_empty, elapsed) {
        self.rotate_to_next_profile();  // existing next_eligible + switch, or end cycle
    }
}
```

`rotate_to_next_profile` = the profile-switch logic previously in `maybe_rotate_profile`'s `match next` arm (set profile, reset summary, `reconcile_pilot`), or `end_cycle_for_today`/`enter_chat_lap` when no eligible profile. Reuse `next_eligible_profile`.

- [ ] **Step 4: Call `maybe_finish_chat_part` in the tick** — next to `self.maybe_rotate_profile();` (~2873) add `self.maybe_finish_chat_part();`.

- [ ] **Step 5: Compile + run rust tests**

Run: `cargo test`
Expected: PASS (existing `decide_apply_chain`/`next_eligible_in_order` tests still pass; new B2 tests pass). Fix any references to removed fields (`batch_baseline`, `stats`, `scan`).

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat(widget): ход профиля — 27 откликов → чат до крышек → ротация; метка хода в RAM"
```

---

### Task B4: Render stats always-expanded in `draw_autopilot`

**Files:**
- Modify: `~/projects/health-widget/src/main.rs` (`draw_autopilot` ~2313, group/enrich rendering ~2472-2545 now sourced from `summary`)

**Interfaces:**
- Consumes: `self.autopilot.summary: Option<pilot_summary::Summary>`.

- [ ] **Step 1: Replace the scan/stats rendering** inside `draw_autopilot`. Where it used `self.autopilot.scan` (groups/unenriched) and `self.autopilot.stats` (applied/chats), read from `self.autopilot.summary`. Render an always-visible stats block for the active profile:

```rust
if let Some(s) = &self.autopilot.summary {
    ui.add_space(2.0);
    let line = |ui: &mut egui::Ui, label: &str, c: &pilot_summary::Counter| {
        ui.label(egui::RichText::new(format!("{label}: {}/{}", c.turn, c.total)).size(11.0));
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("Отклики {}/{APPLY_TARGET}", s.applied.turn)).size(11.0));
        ui.label(egui::RichText::new(format!("· {} всего", s.applied.total)).size(11.0));
    });
    ui.horizontal(|ui| { line(ui, "Ответы", &s.replies); });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!(
            "Капча {}/{} · Формы {}/{} · 403 {}/{} · Протухло {}/{}",
            s.captcha.turn, s.captcha.total, s.forms.turn, s.forms.total,
            s.http_403.turn, s.http_403.total, s.expired.turn, s.expired.total,
        )).size(11.0));
    });
    if !s.groups.is_empty() {
        let new_line = s.groups.iter().filter(|g| g.new > 0)
            .map(|g| format!("{} {}", g.name, g.new)).collect::<Vec<_>>().join(" · ");
        if !new_line.is_empty() {
            ui.label(egui::RichText::new(format!("Новые: {new_line}")).size(11.0));
        }
    }
    if !s.top.is_empty() {
        ui.label(egui::RichText::new(format!("Топ: {}", s.top.join(" · "))).size(11.0));
    }
}
```

- [ ] **Step 2: Point the enrich button count at `summary.unenriched`.** In the enrich `selectable_label` block (~2524) replace `scan.unenriched` with `self.autopilot.summary.as_ref().map(|s| s.unenriched).unwrap_or(0)`.

- [ ] **Step 3: Build + run the widget briefly** (visual smoke)

Run: `cargo build && echo built`
Expected: builds. (Full run is manual; per project idiom, `setsid` from a shell, not systemd-run — see memory `widget-restart-cgroup-trap`. Do not launch here unless asked.)

- [ ] **Step 4: Run rust tests**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "ui(widget): блок статистики профиля всегда развёрнут (ход/всего, новые, топ)"
```

---

## Self-Review notes (traceability)

- Spec "queue per-profile" → A1, A2. "events counters" → A3, A7. "top snapshot" → A4. "summary payload/CLI no torch" → A5, A6. "searches in overlays / drop state files" → A8. "scan/enrich per profile" → A8. "widget bridge via stdout" → B1. "turn rule 27/40/30" → B2, B3. "turn marker in RAM, --since" → B3. "always-expanded stats UI" → B4.
- Daily scan gate: enforced widget-side using `summary.last_scan_date` vs today — wired in B3/B4 by comparing before entering a profile's first apply lap of the day; if executing, add the check in `enter_apply_lap` (skip scan trigger unless `last_scan_date != today`). Covered by the last_scan_date field (A5) + widget logic (B3).
- Migration for legacy tests: existing python tests that call old signatures must be updated in A7 Step 9 / A8 Step 5 (part of keeping the suite green).

## Risks / watch-items

- **Composite-PK migration** is destructive (drops/recreates `queue`). Back up `data/autopilot.db` before first run on the real DB (memory `enrich-hh-403-block` notes the pool state).
- **`chats` table has no `profile`** — `reply` events are attributed via the profile the chat phase runs under (each phase runs with `--profile`), which is correct for per-profile reply counts. The `chats` fingerprint table stays global; only `events.reply` is per-profile.
- **`summary` spawn cost** (~0.25s import) every 3s — acceptable; do not lower the throttle.
- **fullstack overlay** must preserve legacy `data/browser-profile` (only searches/resume move; browser dir stays legacy) — verified in A8 Step 2.
