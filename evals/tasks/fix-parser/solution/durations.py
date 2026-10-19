import re

UNITS = {"s": 1, "m": 60, "h": 3600}


def parse_duration(text):
    """Seconds in a duration like "90s", "5m" or "1h30m"."""
    text = text.strip()
    if not re.fullmatch(r"(\d+[smh])+", text):
        raise ValueError(f"bad duration: {text!r}")
    return sum(int(n) * UNITS[u] for n, u in re.findall(r"(\d+)([smh])", text))
