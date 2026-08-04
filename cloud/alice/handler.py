import base64
import hmac
import json
import os
from datetime import datetime, timedelta, timezone

import boto3

MSK = timezone(timedelta(hours=3))
STALE_AFTER = timedelta(minutes=30)
OBJECT_KEY = "applied.json"
ENDPOINT = "https://storage.yandexcloud.net"


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
