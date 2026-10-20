"""Jobs, from a JSON file: [{"name": "lint", "priority": 2, "after": ["fetch"]}, ...]."""

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Job:
    name: str
    priority: int = 0
    # The jobs that must finish before this one starts.
    after: tuple = ()


def load_jobs(path):
    rows = json.loads(Path(path).read_text(encoding="utf-8"))
    return [
        Job(name=r["name"], priority=int(r.get("priority", 0)), after=tuple(r.get("after", ())))
        for r in rows
    ]
