"""Dates: parsing, and counting business days for delivery estimates."""

from datetime import date, timedelta


def parse_date(text):
    """'2026-10-16' -> date(2026, 10, 16)."""
    if isinstance(text, date):
        return text
    return date.fromisoformat(text.strip())


def is_business_day(day, holidays=()):
    return day.weekday() < 5 and day not in holidays


def add_business_days(start, n, holidays=()):
    """The date `n` business days after `start`; `start` itself never counts."""
    day = start
    while n > 0:
        if is_business_day(day, holidays):
            n -= 1
        day += timedelta(days=1)
    return day
