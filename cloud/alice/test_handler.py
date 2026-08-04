import base64
import json
from datetime import datetime, timedelta

import pytest

import handler as h
from handler import MSK, phrase, plural

OBJECT_KEY = "applied.json"


class FakeBody:
    def __init__(self, payload):
        self.payload = payload

    def read(self):
        return json.dumps(self.payload).encode()


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
