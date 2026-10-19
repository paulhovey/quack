import re

UNITS = {"s": 1, "m": 60, "h": 3600}


def parse_duration(text):
    """Seconds in a duration like "90s", "5m" or "1h30m"."""
    match = re.fullmatch(r"(\d+)([smh])", text.strip())
    if not match:
        raise ValueError(f"bad duration: {text!r}")
    return int(match.group(1)) * UNITS[match.group(2)]
