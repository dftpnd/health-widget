# Alice «сколько сегодня откликов» Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Спросить у Яндекс.Станции «Алиса, спроси у автопилота сколько сегодня откликов» и услышать суммарное число откликов автопилота за сегодня.

**Architecture:** Одна публичная Cloud Function в Yandex Cloud с двумя входами: виджет раз в 5 минут пушит цифру `curl`-ом (тело с `token`), Диалоги дёргают ту же функцию (тело с `request`/`session`). Состояние — один объект `applied.json` в приватном бакете Object Storage. Ключи от бакета живут только в переменных окружения функции.

**Tech Stack:** Rust (виджет, без новых зависимостей), Python 3.12 + boto3 (Cloud Function), bash + `yc` CLI (деплой), `curl` и `date` шеллятся из виджета.

## Global Constraints

- **Никаких комментариев в исходниках** — ни в Rust (`//`, `///`, `/* */`), ни в Python (`#`), включая docstring'и модулей и функций. Исключение только для шебанга. Это правило из `CLAUDE.md`, оно жёстче обычных привычек: если тянет пояснить — переименуй или разбей функцию.
- Строки UI и фразы Алисы — на русском.
- Работаем только в ветке `master`, новых веток не создавать.
- Новых зависимостей в `Cargo.toml` не добавлять: дата берётся шеллом `date +%F`, HTTP — шеллом `curl`.
- Фраза активации навыка: «Алиса, спроси у **автопилота** сколько сегодня откликов».
- Порог протухания данных — 30 минут. Период тика виджета — 5 минут. Heartbeat-пуш — 20 минут.
- Часовой пояс всей логики — МСК (UTC+3).
- Спека: `docs/superpowers/specs/2026-08-04-alice-applied-today-design.md`.

---

## File Structure

| Файл | Ответственность |
|---|---|
| `cloud/alice/handler.py` | Точка входа функции: роутинг push/диалог, проверка токена и skill_id, чтение-запись состояния, формирование фразы |
| `cloud/alice/requirements.txt` | `boto3` |
| `cloud/alice/test_handler.py` | Тесты handler'а на фейковом S3-клиенте |
| `src/alice.rs` | Виджет: чтение конфига, сбор сумм по профилям, правило «пушить/не пушить», отправка `curl` |
| `src/main.rs` | `mod alice;` + запуск потока рядом с прочими фоновыми потоками |
| `scripts/alice-deploy.sh` | Идемпотентный деплой: бакет, сервисный аккаунт, ключи, функция, публичный вызов, запись конфига виджета |
| `README.md` | Раздел про навык: ручные шаги в Консоли Диалогов |

Границы: `handler.py` не знает про виджет, `alice.rs` не знает про Object Storage — общий контракт только JSON-тело пуша. Формирование фразы и правило пуша — чистые функции, тестируются без сети и без часов.

**Контракт тела пуша** (единственная точка связи Rust ↔ Python):

```json
{"token": "...", "applied_today": 14, "daily_limit": 200, "date": "2026-08-04"}
```

**Контракт объекта в бакете** (`applied.json`):

```json
{"applied_today": 14, "daily_limit": 200, "date": "2026-08-04", "updated_at": "2026-08-04T12:30:05+03:00"}
```

`updated_at` проставляет функция в момент пуша — так время берётся из одних часов, а не из десктопных.

---

### Task 1: Фраза ответа (чистая функция)

**Files:**
- Create: `cloud/alice/handler.py`
- Create: `cloud/alice/test_handler.py`

**Interfaces:**
- Consumes: ничего
- Produces:
  - `MSK: timezone` — UTC+3
  - `plural(n: int) -> str` — «отклик» / «отклика» / «откликов»
  - `phrase(state: dict | None, now: datetime) -> str` — фраза для Алисы

Спека фиксирует четыре ответа; склонение числительного она не оговаривала, но «Сегодня 1 откликов» для голосового навыка звучит сломанно, поэтому `plural` добавлен.

- [ ] **Step 1: Создать venv для тестов функции**

```bash
python3 -m venv cloud/alice/.venv
cloud/alice/.venv/bin/pip install -q pytest boto3
printf 'boto3\n' > cloud/alice/requirements.txt
printf '.venv/\n__pycache__/\n' > cloud/alice/.gitignore
```

- [ ] **Step 2: Написать падающие тесты**

Создать `cloud/alice/test_handler.py`:

```python
from datetime import datetime, timedelta

from handler import MSK, phrase, plural


def at(hh, mm):
    return datetime(2026, 8, 4, hh, mm, tzinfo=MSK)


def state(applied=14, date="2026-08-04", updated="2026-08-04T12:30:05+03:00"):
    return {"applied_today": applied, "daily_limit": 200, "date": date, "updated_at": updated}


def test_plural_forms():
    assert plural(1) == "отклик"
    assert plural(2) == "отклика"
    assert plural(5) == "откликов"
    assert plural(11) == "откликов"
    assert plural(21) == "отклик"
    assert plural(0) == "откликов"


def test_fresh_push_says_number():
    assert phrase(state(), at(12, 45)) == "Сегодня 14 откликов"


def test_yesterday_date_means_no_applies():
    assert phrase(state(date="2026-08-03"), at(12, 45)) == "Сегодня откликов ещё нет"


def test_stale_push_mentions_time():
    got = phrase(state(), at(15, 0))
    assert got == "Сегодня 14 откликов, данные на 12:30"


def test_missing_state_says_no_data():
    assert phrase(None, at(12, 45)) == "Не могу получить данные"


def test_unparsable_timestamp_falls_back_to_plain_phrase():
    assert phrase(state(updated="мусор"), at(15, 0)) == "Сегодня 14 откликов"


def test_single_apply_uses_singular():
    assert phrase(state(applied=1), at(12, 45)) == "Сегодня 1 отклик"


def test_stale_threshold_is_thirty_minutes():
    pushed = datetime(2026, 8, 4, 12, 30, 5, tzinfo=MSK)
    assert phrase(state(), pushed + timedelta(minutes=29)) == "Сегодня 14 откликов"
    assert "данные на" in phrase(state(), pushed + timedelta(minutes=31))
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cd cloud/alice && .venv/bin/pytest -q`
Expected: FAIL — `ModuleNotFoundError: No module named 'handler'`

- [ ] **Step 4: Написать минимальную реализацию**

Создать `cloud/alice/handler.py`:

```python
from datetime import datetime, timedelta, timezone

MSK = timezone(timedelta(hours=3))
STALE_AFTER = timedelta(minutes=30)


def plural(n):
    tail_100 = abs(n) % 100
    tail_10 = abs(n) % 10
    if 11 <= tail_100 <= 14:
        return "откликов"
    if tail_10 == 1:
        return "отклик"
    if 2 <= tail_10 <= 4:
        return "отклика"
    return "откликов"


def parse_ts(raw):
    try:
        return datetime.fromisoformat(raw).astimezone(MSK)
    except (TypeError, ValueError):
        return None


def phrase(state, now):
    if not state:
        return "Не могу получить данные"
    if state.get("date") != now.astimezone(MSK).strftime("%Y-%m-%d"):
        return "Сегодня откликов ещё нет"
    applied = int(state.get("applied_today", 0))
    said = f"Сегодня {applied} {plural(applied)}"
    updated = parse_ts(state.get("updated_at"))
    if updated is not None and now - updated > STALE_AFTER:
        return f"{said}, данные на {updated.strftime('%H:%M')}"
    return said
```

- [ ] **Step 5: Убедиться, что тесты проходят**

Run: `cd cloud/alice && .venv/bin/pytest -q`
Expected: PASS, 8 passed

- [ ] **Step 6: Коммит**

```bash
git add cloud/alice/handler.py cloud/alice/test_handler.py cloud/alice/requirements.txt cloud/alice/.gitignore
git commit -m "feat(alice): фраза ответа про отклики за сегодня 🎙️"
```

---

### Task 2: Роутинг функции — пуш и запрос Диалогов

**Files:**
- Modify: `cloud/alice/handler.py`
- Modify: `cloud/alice/test_handler.py`

**Interfaces:**
- Consumes: `phrase(state, now)`, `MSK` из Task 1
- Produces:
  - `load_state(client, bucket) -> dict | None`
  - `save_state(client, bucket, body: dict) -> dict` — добавляет `updated_at`, кладёт объект
  - `handle(event, context) -> dict` — точка входа Cloud Function

Переменные окружения функции: `ALICE_BUCKET`, `ALICE_PUSH_TOKEN`, `ALICE_SKILL_ID` (может быть пустой, пока навык не создан), `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`.

Тело запроса Диалогов узнаём по наличию ключа `session`; тело пуша — по наличию `token`.

- [ ] **Step 1: Написать падающие тесты**

Дописать в `cloud/alice/test_handler.py`:

```python
import base64
import json

import pytest

import handler as h

OBJECT_KEY = "applied.json"


class FakeS3:
    def __init__(self, body=None):
        self.body = body
        self.puts = []

    def get_object(self, Bucket, Key):
        if self.body is None:
            raise RuntimeError("NoSuchKey")
        return {"Body": FakeBody(self.body)}

    def put_object(self, Bucket, Key, Body, ContentType):
        self.puts.append((Bucket, Key, json.loads(Body)))


class FakeBody:
    def __init__(self, payload):
        self.payload = payload

    def read(self):
        return json.dumps(self.payload).encode()


@pytest.fixture(autouse=True)
def env(monkeypatch):
    monkeypatch.setenv("ALICE_BUCKET", "bucket")
    monkeypatch.setenv("ALICE_PUSH_TOKEN", "s3cret")
    monkeypatch.setenv("ALICE_SKILL_ID", "skill-1")


def event(body):
    return {"httpMethod": "POST", "body": json.dumps(body)}


def dialog_body(skill_id="skill-1"):
    return {
        "version": "1.0",
        "session": {"skill_id": skill_id, "new": True},
        "request": {"command": "сколько сегодня откликов"},
    }


def push_body(token="s3cret", applied=14):
    return {"token": token, "applied_today": applied, "daily_limit": 200, "date": "2026-08-04"}


def test_load_state_returns_none_on_missing_object():
    assert h.load_state(FakeS3(), "bucket") is None


def test_save_state_stamps_updated_at():
    fake = FakeS3()
    saved = h.save_state(fake, "bucket", push_body())
    assert fake.puts[0][0] == "bucket"
    assert fake.puts[0][1] == OBJECT_KEY
    stored = fake.puts[0][2]
    assert stored["applied_today"] == 14
    assert stored["date"] == "2026-08-04"
    assert "updated_at" in stored
    assert "token" not in stored
    assert saved == stored


def test_push_with_valid_token_writes_object(monkeypatch):
    fake = FakeS3()
    monkeypatch.setattr(h, "client", lambda: fake)
    resp = h.handle(event(push_body()), None)
    assert resp["statusCode"] == 200
    assert fake.puts[0][2]["applied_today"] == 14


def test_push_with_bad_token_is_rejected(monkeypatch):
    fake = FakeS3()
    monkeypatch.setattr(h, "client", lambda: fake)
    resp = h.handle(event(push_body(token="wrong")), None)
    assert resp["statusCode"] == 403
    assert fake.puts == []


def test_dialog_answers_with_phrase(monkeypatch):
    stored = {
        "applied_today": 14,
        "daily_limit": 200,
        "date": h.today(),
        "updated_at": h.now().isoformat(),
    }
    monkeypatch.setattr(h, "client", lambda: FakeS3(stored))
    resp = h.handle(event(dialog_body()), None)
    assert resp["statusCode"] == 200
    payload = json.loads(resp["body"])
    assert payload["version"] == "1.0"
    assert payload["response"]["text"] == "Сегодня 14 откликов"
    assert payload["response"]["tts"] == "Сегодня 14 откликов"
    assert payload["response"]["end_session"] is True


def test_dialog_from_foreign_skill_is_rejected(monkeypatch):
    monkeypatch.setattr(h, "client", lambda: FakeS3(None))
    resp = h.handle(event(dialog_body(skill_id="someone-else")), None)
    assert resp["statusCode"] == 403


def test_dialog_allowed_when_skill_id_not_configured(monkeypatch):
    monkeypatch.setenv("ALICE_SKILL_ID", "")
    monkeypatch.setattr(h, "client", lambda: FakeS3(None))
    resp = h.handle(event(dialog_body(skill_id="anything")), None)
    assert json.loads(resp["body"])["response"]["text"] == "Не могу получить данные"


def test_base64_body_is_decoded(monkeypatch):
    fake = FakeS3()
    monkeypatch.setattr(h, "client", lambda: fake)
    raw = base64.b64encode(json.dumps(push_body()).encode()).decode()
    resp = h.handle({"httpMethod": "POST", "body": raw, "isBase64Encoded": True}, None)
    assert resp["statusCode"] == 200
    assert fake.puts[0][2]["applied_today"] == 14


def test_garbage_body_is_rejected(monkeypatch):
    monkeypatch.setattr(h, "client", lambda: FakeS3())
    resp = h.handle({"httpMethod": "POST", "body": "не json"}, None)
    assert resp["statusCode"] == 400
```

- [ ] **Step 2: Убедиться, что новые тесты падают**

Run: `cd cloud/alice && .venv/bin/pytest -q`
Expected: FAIL — `AttributeError: module 'handler' has no attribute 'handle'`

- [ ] **Step 3: Реализовать роутинг**

Дописать в `cloud/alice/handler.py` (импорты — вверх файла):

```python
import base64
import hmac
import json
import os

import boto3

OBJECT_KEY = "applied.json"
ENDPOINT = "https://storage.yandexcloud.net"


def client():
    return boto3.client("s3", endpoint_url=ENDPOINT, region_name="ru-central1")


def now():
    return datetime.now(MSK)


def today():
    return now().strftime("%Y-%m-%d")


def load_state(s3, bucket):
    try:
        obj = s3.get_object(Bucket=bucket, Key=OBJECT_KEY)
        return json.loads(obj["Body"].read())
    except Exception:
        return None


def save_state(s3, bucket, body):
    stored = {
        "applied_today": int(body.get("applied_today", 0)),
        "daily_limit": int(body.get("daily_limit", 0)),
        "date": body.get("date", today()),
        "updated_at": now().isoformat(timespec="seconds"),
    }
    s3.put_object(
        Bucket=bucket,
        Key=OBJECT_KEY,
        Body=json.dumps(stored).encode(),
        ContentType="application/json",
    )
    return stored


def reply(code, payload):
    return {
        "statusCode": code,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps(payload, ensure_ascii=False),
    }


def voice(text):
    return reply(200, {
        "version": "1.0",
        "response": {"text": text, "tts": text, "end_session": True},
    })


def read_body(event):
    raw = event.get("body") or "{}"
    if event.get("isBase64Encoded"):
        raw = base64.b64decode(raw).decode()
    return json.loads(raw)


def handle(event, context):
    bucket = os.environ["ALICE_BUCKET"]
    try:
        body = read_body(event)
    except (ValueError, UnicodeDecodeError):
        return reply(400, {"error": "bad body"})

    if "token" in body:
        if not hmac.compare_digest(str(body["token"]), os.environ["ALICE_PUSH_TOKEN"]):
            return reply(403, {"error": "bad token"})
        save_state(client(), bucket, body)
        return reply(200, {"ok": True})

    expected = os.environ.get("ALICE_SKILL_ID", "")
    got = (body.get("session") or {}).get("skill_id", "")
    if expected and got != expected:
        return reply(403, {"error": "foreign skill"})

    return voice(phrase(load_state(client(), bucket), now()))
```

- [ ] **Step 4: Убедиться, что все тесты проходят**

Run: `cd cloud/alice && .venv/bin/pytest -q`
Expected: PASS, 18 passed

- [ ] **Step 5: Коммит**

```bash
git add cloud/alice/handler.py cloud/alice/test_handler.py
git commit -m "feat(alice): роутинг функции — пуш виджета и запрос Диалогов 🔀"
```

---

### Task 3: Модуль виджета — конфиг, суммы, правило пуша

**Files:**
- Create: `src/alice.rs`
- Modify: `src/main.rs:36` (добавить `mod alice;` в список модулей)

**Interfaces:**
- Consumes: `crate::pilot_summary::{fetch, Summary}`, `crate::telemetry`
- Produces:
  - `pub struct Cfg { pub url: String, pub token: String }`
  - `pub fn load_cfg() -> Option<Cfg>` — читает `~/.config/health-widget/alice.json`
  - `pub fn totals(sums: &[Summary]) -> Option<(i64, i64)>` — сумма `applied_today` и `daily_limit`
  - `pub fn should_push(prev: Option<i64>, cur: i64, since_last: Duration) -> bool`
  - `pub fn body(cfg: &Cfg, applied: i64, limit: i64, date: &str) -> String`
  - `pub fn today() -> String` — шеллит `date +%F`

`should_push` берёт `Duration` параметром, а не смотрит на часы, — иначе тест пришлось бы усыплять на 20 минут.

- [ ] **Step 1: Написать падающие тесты**

Создать `src/alice.rs` с одним только тест-модулем внизу (реализации пока нет):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pilot_summary::Summary;

    fn sum(applied: i64, limit: i64) -> Summary {
        Summary { applied_today: applied, daily_limit: limit, ..Default::default() }
    }

    #[test]
    fn totals_sum_across_profiles() {
        let got = totals(&[sum(9, 100), sum(5, 100)]);
        assert_eq!(got, Some((14, 200)));
    }

    #[test]
    fn totals_of_nothing_is_none() {
        assert_eq!(totals(&[]), None);
    }

    #[test]
    fn first_push_always_happens() {
        assert!(should_push(None, 0, Duration::from_secs(0)));
    }

    #[test]
    fn changed_value_pushes_immediately() {
        assert!(should_push(Some(13), 14, Duration::from_secs(60)));
    }

    #[test]
    fn same_value_stays_quiet_for_a_while() {
        assert!(!should_push(Some(14), 14, Duration::from_secs(5 * 60)));
    }

    #[test]
    fn same_value_pushes_as_heartbeat_after_twenty_minutes() {
        assert!(should_push(Some(14), 14, Duration::from_secs(21 * 60)));
    }

    #[test]
    fn body_carries_token_and_counts() {
        let cfg = Cfg { url: "u".to_string(), token: "s3cret".to_string() };
        let js: serde_json::Value =
            serde_json::from_str(&body(&cfg, 14, 200, "2026-08-04")).unwrap();
        assert_eq!(js["token"], "s3cret");
        assert_eq!(js["applied_today"], 14);
        assert_eq!(js["daily_limit"], 200);
        assert_eq!(js["date"], "2026-08-04");
    }

    #[test]
    fn today_is_iso_date() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(d.matches('-').count(), 2);
    }

    #[test]
    fn cfg_parses_url_and_token() {
        let cfg = parse_cfg(r#"{"url":"https://x/y","token":"t"}"#).unwrap();
        assert_eq!(cfg.url, "https://x/y");
        assert_eq!(cfg.token, "t");
    }

    #[test]
    fn cfg_without_url_is_rejected() {
        assert!(parse_cfg(r#"{"token":"t"}"#).is_none());
    }
}
```

- [ ] **Step 2: Убедиться, что не собирается**

Run: `cargo test alice 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'totals' in this scope` (после добавления `mod alice;` в `src/main.rs`)

- [ ] **Step 3: Реализовать модуль**

Дописать в начало `src/alice.rs`:

```rust
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::pilot_summary::Summary;

const HEARTBEAT: Duration = Duration::from_secs(20 * 60);

#[derive(Clone)]
pub struct Cfg {
    pub url: String,
    pub token: String,
}

pub fn cfg_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("health-widget").join("alice.json"))
}

pub fn parse_cfg(raw: &str) -> Option<Cfg> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let url = v.get("url")?.as_str()?.to_string();
    let token = v.get("token")?.as_str()?.to_string();
    if url.is_empty() || token.is_empty() {
        return None;
    }
    Some(Cfg { url, token })
}

pub fn load_cfg() -> Option<Cfg> {
    let raw = std::fs::read_to_string(cfg_path()?).ok()?;
    parse_cfg(&raw)
}

pub fn totals(sums: &[Summary]) -> Option<(i64, i64)> {
    if sums.is_empty() {
        return None;
    }
    let applied = sums.iter().map(|s| s.applied_today).sum();
    let limit = sums.iter().map(|s| s.daily_limit).sum();
    Some((applied, limit))
}

pub fn should_push(prev: Option<i64>, cur: i64, since_last: Duration) -> bool {
    match prev {
        None => true,
        Some(p) => p != cur || since_last >= HEARTBEAT,
    }
}

pub fn body(cfg: &Cfg, applied: i64, limit: i64, date: &str) -> String {
    serde_json::json!({
        "token": cfg.token,
        "applied_today": applied,
        "daily_limit": limit,
        "date": date,
    })
    .to_string()
}

pub fn today() -> String {
    Command::new("date")
        .arg("+%F")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}
```

Добавить `mod alice;` в список модулей `src/main.rs` (рядом со строкой `mod audio;`, порядок алфавитный — перед ней).

- [ ] **Step 4: Убедиться, что тесты проходят**

Run: `cargo test alice 2>&1 | tail -20`
Expected: PASS, 10 passed

- [ ] **Step 5: Коммит**

```bash
git add src/alice.rs src/main.rs
git commit -m "feat(alice): конфиг, суммы по профилям и правило пуша 📊"
```

---

### Task 4: Поток пуша и подключение к виджету

**Files:**
- Modify: `src/alice.rs`
- Modify: `src/main.rs` (запуск потока рядом с прочими `std::thread::spawn` в конструкторе приложения, около `src/main.rs:602`)

**Interfaces:**
- Consumes: `Cfg`, `load_cfg`, `totals`, `should_push`, `body`, `today` из Task 3; `pilot_summary::fetch(dir, bin, profile, since)`
- Produces: `pub fn spawn(dir: PathBuf, bin: PathBuf, profiles: Vec<String>)` — стартует поток; если конфига нет, возвращается молча, ничего не логируя

- [ ] **Step 1: Написать падающий тест на отправку**

Дописать в тест-модуль `src/alice.rs`:

```rust
    #[test]
    fn push_reports_failure_for_unreachable_url() {
        let cfg = Cfg { url: "http://127.0.0.1:1/none".to_string(), token: "t".to_string() };
        let err = push(&cfg, &body(&cfg, 1, 2, "2026-08-04")).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn spawn_without_config_is_a_noop() {
        let handle = spawn_with(None, PathBuf::from("/nonexistent"), PathBuf::from("/nonexistent"), vec![]);
        assert!(handle.is_none());
    }
```

- [ ] **Step 2: Убедиться, что не собирается**

Run: `cargo test alice 2>&1 | tail -20`
Expected: FAIL — `cannot find function 'push' in this scope`

- [ ] **Step 3: Реализовать отправку и поток**

Дописать в `src/alice.rs`:

```rust
use std::io::Write;
use std::process::Stdio;
use std::thread::JoinHandle;
use std::time::Instant;

const TICK: Duration = Duration::from_secs(5 * 60);

pub fn push(cfg: &Cfg, payload: &str) -> Result<(), String> {
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "-m",
            "5",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
            &cfg.url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or("нет stdin")?
        .write_all(payload.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() && out.stderr.is_empty() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub fn spawn(dir: PathBuf, bin: PathBuf, profiles: Vec<String>) -> Option<JoinHandle<()>> {
    spawn_with(load_cfg(), dir, bin, profiles)
}

pub fn spawn_with(
    cfg: Option<Cfg>,
    dir: PathBuf,
    bin: PathBuf,
    profiles: Vec<String>,
) -> Option<JoinHandle<()>> {
    let cfg = cfg?;
    Some(std::thread::spawn(move || {
        let mut prev: Option<i64> = None;
        let mut last = Instant::now();
        loop {
            let sums: Vec<Summary> = profiles
                .iter()
                .filter_map(|p| crate::pilot_summary::fetch(&dir, &bin, p, None))
                .collect();
            if let Some((applied, limit)) = totals(&sums) {
                if should_push(prev, applied, last.elapsed()) {
                    match push(&cfg, &body(&cfg, applied, limit, &today())) {
                        Ok(()) => {
                            crate::telemetry::event(
                                "alice.push",
                                serde_json::json!({ "applied": applied, "limit": limit }),
                            );
                            prev = Some(applied);
                            last = Instant::now();
                        }
                        Err(e) => crate::telemetry::error("alice.push", &e),
                    }
                }
            }
            std::thread::sleep(TICK);
        }
    }))
}
```

- [ ] **Step 4: Подключить к виджету**

В `src/main.rs`, рядом с блоком запуска фоновых потоков (около строки 602, где стартует поток `detect::screencast_active`), добавить:

```rust
        alice::spawn(
            cfg.autopilot_dir.clone(),
            cfg.autopilot_bin.clone(),
            PILOT_PROFILES.iter().map(|(k, _)| k.to_string()).collect(),
        );
```

- [ ] **Step 5: Убедиться, что тесты и сборка проходят**

Run: `cargo test alice 2>&1 | tail -20 && cargo build 2>&1 | tail -5`
Expected: PASS, 12 passed; сборка без ошибок и без warning'ов про неиспользуемые функции

- [ ] **Step 6: Коммит**

```bash
git add src/alice.rs src/main.rs
git commit -m "feat(alice): фоновый пуш откликов в облако 🚀"
```

---

### Task 5: Деплой-скрипт и README

**Files:**
- Create: `scripts/alice-deploy.sh`
- Modify: `README.md`

**Interfaces:**
- Consumes: `cloud/alice/` как `--source-path` функции
- Produces: `~/.config/health-widget/alice.json` (`url`, `token`) — то, что читает `alice::load_cfg`; `~/.config/health-widget/alice-cloud.json` (bucket, ключи S3, skill_id) — служебное состояние скрипта

Скрипт идемпотентен: повторный прогон создаёт новую версию функции, но переиспользует бакет, сервисный аккаунт, статический ключ и push-токен. `--skill-id <id>` дописывает skill_id в окружение функции после того, как навык создан в Консоли Диалогов.

- [ ] **Step 1: Написать скрипт**

Создать `scripts/alice-deploy.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

FN=alice-applied
SA=health-widget-alice
CFG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/health-widget"
CFG="$CFG_DIR/alice.json"
CLOUD="$CFG_DIR/alice-cloud.json"
SRC="$(cd "$(dirname "$0")/.." && pwd)/cloud/alice"

SKILL_ID=""
if [ "${1:-}" = "--skill-id" ]; then
  SKILL_ID="${2:?нужен id навыка}"
fi

if ! command -v yc >/dev/null; then
  curl -sSL https://storage.yandexcloud.net/yandexcloud-yc/install.sh | bash -s -- -a
  export PATH="$HOME/yandex-cloud/bin:$PATH"
fi

mkdir -p "$CFG_DIR"
[ -f "$CLOUD" ] || echo '{}' > "$CLOUD"

read_cloud() { jq -r --arg k "$1" '.[$k] // ""' "$CLOUD"; }
write_cloud() {
  tmp=$(mktemp)
  jq --arg k "$1" --arg v "$2" '.[$k] = $v' "$CLOUD" > "$tmp"
  mv "$tmp" "$CLOUD"
  chmod 600 "$CLOUD"
}

FOLDER=$(yc config get folder-id)

BUCKET=$(read_cloud bucket)
if [ -z "$BUCKET" ]; then
  BUCKET="health-widget-alice-$(head -c6 /dev/urandom | od -An -tx1 | tr -d ' \n')"
  write_cloud bucket "$BUCKET"
fi

TOKEN=$(read_cloud token)
if [ -z "$TOKEN" ]; then
  TOKEN=$(head -c24 /dev/urandom | od -An -tx1 | tr -d ' \n')
  write_cloud token "$TOKEN"
fi

[ -n "$SKILL_ID" ] && write_cloud skill_id "$SKILL_ID"
SKILL_ID=$(read_cloud skill_id)

if ! yc iam service-account get --name "$SA" >/dev/null 2>&1; then
  yc iam service-account create --name "$SA"
fi
SA_ID=$(yc iam service-account get --name "$SA" --format json | jq -r .id)

yc resource-manager folder add-access-binding "$FOLDER" \
  --role storage.editor --subject "serviceAccount:$SA_ID" >/dev/null 2>&1 || true

if ! yc storage bucket get --name "$BUCKET" >/dev/null 2>&1; then
  yc storage bucket create --name "$BUCKET"
fi

KEY_ID=$(read_cloud key_id)
SECRET=$(read_cloud key_secret)
if [ -z "$KEY_ID" ] || [ -z "$SECRET" ]; then
  KEY_JSON=$(yc iam access-key create --service-account-name "$SA" --format json)
  KEY_ID=$(echo "$KEY_JSON" | jq -r .access_key.key_id)
  SECRET=$(echo "$KEY_JSON" | jq -r .secret)
  write_cloud key_id "$KEY_ID"
  write_cloud key_secret "$SECRET"
fi

if ! yc serverless function get --name "$FN" >/dev/null 2>&1; then
  yc serverless function create --name "$FN"
fi

yc serverless function version create \
  --function-name "$FN" \
  --runtime python312 \
  --entrypoint handler.handle \
  --memory 128m \
  --execution-timeout 5s \
  --source-path "$SRC" \
  --service-account-id "$SA_ID" \
  --environment "ALICE_BUCKET=$BUCKET,ALICE_PUSH_TOKEN=$TOKEN,ALICE_SKILL_ID=$SKILL_ID,AWS_ACCESS_KEY_ID=$KEY_ID,AWS_SECRET_ACCESS_KEY=$SECRET" >/dev/null

yc serverless function allow-unauthenticated-invoke "$FN" >/dev/null 2>&1 || true

FN_ID=$(yc serverless function get --name "$FN" --format json | jq -r .id)
URL="https://functions.yandexcloud.net/$FN_ID"

jq -n --arg url "$URL" --arg token "$TOKEN" '{url: $url, token: $token}' > "$CFG"
chmod 600 "$CFG"

echo "функция: $FN_ID"
echo "url:     $URL"
echo "конфиг:  $CFG"
if [ -z "$SKILL_ID" ]; then
  echo
  echo "дальше: создай навык «автопилот» в Консоли Диалогов (backend = Cloud Function, id выше),"
  echo "затем прогони: scripts/alice-deploy.sh --skill-id <skill_id из Консоли>"
fi
```

- [ ] **Step 2: Проверить синтаксис скрипта**

Run: `chmod +x scripts/alice-deploy.sh && bash -n scripts/alice-deploy.sh && shellcheck scripts/alice-deploy.sh || true`
Expected: `bash -n` молчит (shellcheck может отсутствовать — не блокер)

- [ ] **Step 3: Дописать раздел в README**

Добавить в `README.md` раздел:

```markdown
## Алиса: «сколько сегодня откликов»

Навык Яндекс.Диалогов, который называет число откликов автопилота за сегодня
(суммарно по всем профилям). Бэкенд — Cloud Function в Yandex Cloud, виджет
раз в 5 минут пушит туда цифру.

Развернуть:

    yc init                        # если ещё не настроен
    scripts/alice-deploy.sh

Скрипт создаст бакет, сервисный аккаунт, функцию и запишет
`~/.config/health-widget/alice.json` — виджет подхватит его при следующем старте.

Дальше руками, в [Консоли Диалогов](https://dialogs.yandex.ru/developer):

1. Создать навык, имя активации — **автопилот**.
2. Backend → Cloud Function, id из вывода скрипта.
3. Скопировать `skill_id` и прогнать `scripts/alice-deploy.sh --skill-id <id>` —
   после этого функция отвечает только этому навыку.
4. Опубликовать навык приватно (доступен только своему аккаунту).

Спросить: «Алиса, спроси у автопилота сколько сегодня откликов».

Если ПК был выключен, Алиса добавит «данные на HH:MM» — значит цифра старше
получаса. Пока конфига `alice.json` нет, виджет ничего никуда не шлёт.
```

- [ ] **Step 4: Коммит**

```bash
git add scripts/alice-deploy.sh README.md
git commit -m "feat(alice): деплой навыка одной командой 📦"
```

---

### Task 6: Живая проверка

**Files:** ничего не меняется, если проверка прошла

- [ ] **Step 1: Развернуть**

Run: `scripts/alice-deploy.sh`
Expected: печатает id функции и url, создаёт `~/.config/health-widget/alice.json`

- [ ] **Step 2: Проверить пуш вручную**

```bash
URL=$(jq -r .url ~/.config/health-widget/alice.json)
TOKEN=$(jq -r .token ~/.config/health-widget/alice.json)
curl -sS -X POST -H 'Content-Type: application/json' \
  -d "{\"token\":\"$TOKEN\",\"applied_today\":14,\"daily_limit\":200,\"date\":\"$(date +%F)\"}" "$URL"
```

Expected: `{"ok": true}`

- [ ] **Step 3: Проверить ответ Диалогам и уложиться в 3 секунды**

```bash
curl -sS -o /dev/stdout -w '\nвремя: %{time_total}\n' -X POST -H 'Content-Type: application/json' \
  -d '{"version":"1.0","session":{"skill_id":"","new":true},"request":{"command":"сколько сегодня откликов"}}' "$URL"
```

Expected: в теле `"text": "Сегодня 14 откликов"`, `время` меньше 3 секунд. Повторить через несколько минут простоя — холодный старт тоже должен уложиться. Если не укладывается, поднять память функции до 256m в `scripts/alice-deploy.sh` и передеплоить.

- [ ] **Step 4: Проверить пуш из виджета**

Перезапустить виджет (`setsid` из шелла, не через `systemd-run --collect`), подождать тик и посмотреть телеметрию:

Run: `target/debug/health-widget --telemetry-today 50 | grep alice`
Expected: запись `alice.push` с реальным `applied`

- [ ] **Step 5: Спросить у Станции**

Сказать: «Алиса, спроси у автопилота сколько сегодня откликов».
Expected: называет ту же цифру, что показывает виджет.

- [ ] **Step 6: Коммит, если что-то правилось**

```bash
git add -A && git commit -m "fix(alice): правки по итогам живой проверки 🔧"
```

---

## Self-Review

**Покрытие спеки:**

| Требование спеки | Задача |
|---|---|
| Правила ответа (сегодня / вчера / протухло / нет объекта) | Task 1 |
| Формат `applied.json` | Task 2 |
| Пуш через функцию, проверка токена | Task 2 |
| Проверка `session.skill_id` | Task 2 |
| `end_session: true`, пустой запуск сразу называет цифру | Task 2 (ответ один на любой запрос Диалогов) |
| Конфиг `~/.config/health-widget/alice.json`, нет файла → тишина | Task 3, Task 4 |
| Сумма по всем `PILOT_PROFILES` | Task 3, Task 4 |
| Тик 5 минут, heartbeat 20 минут | Task 3 (правило), Task 4 (тик) |
| Провалы `curl` в telemetry, без ретраев | Task 4 |
| Идемпотентный деплой на `yc`, установка `yc` при отсутствии | Task 5 |
| `--skill-id` вторым проходом | Task 5 |
| Ручные шаги в README | Task 5 |
| Риск холодного старта против 3 секунд | Task 6, шаг 3 |

**Отклонения от спеки, внесённые сознательно:**
- `updated_at` проставляет функция, а не виджет — часы одни, десктопный сдвиг времени не искажает «данные на HH:MM».
- Добавлено склонение числительного (`plural`) — спека фиксировала только «Сегодня 14 откликов».
