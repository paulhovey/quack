"""Parses application logs into entries.

Two line formats are accepted, and may be mixed in one file:

    2026-10-16 12:00:01,123 ERROR [worker-3] could not reach the database
    {"ts": "2026-10-16T12:00:02.5", "level": "info", "msg": "retrying", "thread": "worker-3"}

The timestamp may use a space or a `T` between date and time, with `,` or `.` before the
fraction, or no fraction. Levels are upper-cased, and `WARN` means `WARNING`, `ERR` means
`ERROR`, `FATAL` means `CRITICAL`. A line that starts with whitespace, or a `Traceback`
line, continues the entry before it: it goes into that entry's `extra`. Blank lines are
skipped. Anything else that doesn't parse raises ParseError with its line number.
"""

import json
import re
from dataclasses import dataclass, field
from datetime import datetime
from typing import Optional

ALIASES = {"WARN": "WARNING", "ERR": "ERROR", "FATAL": "CRITICAL"}
LEVELS = {"DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL"}

TEXT_LINE = re.compile(
    r"^(?P<ts>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:[.,]\d+)?)\s+"
    r"(?P<level>[A-Za-z]+)\s+"
    r"(?:\[(?P<thread>[^\]]+)\]\s+)?"
    r"(?P<msg>.*)$"
)


class ParseError(ValueError):
    def __init__(self, lineno, line):
        super().__init__(f"line {lineno}: cannot parse {line!r}")
        self.lineno = lineno


@dataclass
class Entry:
    ts: datetime
    level: str
    message: str
    thread: Optional[str] = None
    extra: list = field(default_factory=list)


def parse_ts(text):
    text = text.replace("T", " ", 1)
    fmt = "%Y-%m-%d %H:%M:%S.%f" if "." in text else "%Y-%m-%d %H:%M:%S"
    return datetime.strptime(text, fmt)


def level_of(text):
    level = ALIASES.get(text, text).upper()
    if level not in LEVELS:
        raise ValueError(level)
    return level


def _parse_line(line):
    if line.startswith("{"):
        data = json.loads(line)
        return Entry(
            ts=parse_ts(data["ts"]),
            level=level_of(data["level"]),
            message=data["msg"],
        )
    m = TEXT_LINE.match(line)
    if not m:
        raise ValueError(line)
    return Entry(
        ts=parse_ts(m["ts"]),
        level=level_of(m["level"]),
        message=m["msg"].rstrip(),
        thread=m["thread"],
    )


def parse_log(text):
    entries = []
    for lineno, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        if line.startswith("Traceback"):
            if not entries:
                raise ParseError(lineno, line)
            entries[-1].extra.append(line)
            continue
        try:
            entries.append(_parse_line(line))
        except (ValueError, KeyError) as e:
            raise ParseError(lineno, line) from e
    return entries
