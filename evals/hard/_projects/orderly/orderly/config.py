"""Settings from config/settings.json: regions, holidays, email templates, report columns."""

import json
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path

from .util.dates import parse_date

DEFAULT_PATH = Path(__file__).resolve().parent.parent / "config" / "settings.json"


@dataclass(frozen=True)
class Region:
    code: str
    name: str
    tax_rate: Decimal
    # Business days from shipping to the door.
    delivery_days: int


@dataclass(frozen=True)
class Settings:
    currency: str
    regions: dict
    holidays: frozenset
    templates: dict
    report_columns: tuple

    def region(self, code):
        try:
            return self.regions[code]
        except KeyError:
            raise UnknownRegion(code) from None


class UnknownRegion(KeyError):
    pass


def load_settings(path=None):
    raw = json.loads(Path(path or DEFAULT_PATH).read_text(encoding="utf-8"))
    regions = {
        code: Region(
            code=code,
            name=r["name"],
            tax_rate=Decimal(r["tax_rate"]),
            delivery_days=int(r["delivery_days"]),
        )
        for code, r in raw["regions"].items()
    }
    return Settings(
        currency=raw.get("currency", "USD"),
        regions=regions,
        holidays=frozenset(parse_date(d) for d in raw.get("holidays", [])),
        templates=raw["templates"],
        report_columns=tuple(raw["report_columns"]),
    )
