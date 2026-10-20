"""Counts and highlights over parsed entries."""

from collections import Counter

from .parse import parse_log


def summarize(text):
    entries = parse_log(text)
    levels = Counter(e.level for e in entries)
    return {
        "entries": len(entries),
        "levels": dict(sorted(levels.items())),
        "first": entries[0].ts if entries else None,
        "last": entries[-1].ts if entries else None,
        "errors": [e.message for e in entries if e.level in ("ERROR", "CRITICAL")],
        "threads": sorted({e.thread for e in entries if e.thread}),
    }
