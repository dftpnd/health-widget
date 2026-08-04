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
