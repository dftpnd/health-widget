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
