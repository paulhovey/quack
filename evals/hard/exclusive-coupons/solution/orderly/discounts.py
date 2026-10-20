"""Coupons, from data/coupons.json, and how several of them combine on one order.

Kinds:
- `percent`: `value` percent off the eligible subtotal (one category's lines, or all).
- `fixed`: `value` off the order.
- `bogo`: for every two of the SKU named in `value`, one is free.

Percent coupons each apply to the subtotal before any discount (they don't compound),
then fixed ones, then bogo. A coupon whose `min_subtotal` the order doesn't reach does
nothing. The discount never exceeds the subtotal.

An `exclusive` coupon never combines with another: the order gets the bigger discount of
one exclusive coupon on its own, or all its other coupons together.
"""

import json
from dataclasses import dataclass
from decimal import Decimal
from pathlib import Path
from typing import Optional

from .money import Money

KINDS = ("percent", "fixed", "bogo")


class UnknownCoupon(KeyError):
    pass


@dataclass(frozen=True)
class Coupon:
    code: str
    kind: str
    value: str
    min_subtotal: str = "0"
    category: Optional[str] = None
    exclusive: bool = False


def load_coupons(path):
    data = json.loads(Path(path).read_text(encoding="utf-8"))
    coupons = {}
    for row in data["coupons"]:
        if row["kind"] not in KINDS:
            raise ValueError(f"coupon {row['code']}: unknown kind {row['kind']!r}")
        coupons[row["code"]] = Coupon(
            code=row["code"],
            kind=row["kind"],
            value=str(row["value"]),
            min_subtotal=str(row.get("min_subtotal", "0")),
            category=row.get("category"),
            exclusive=bool(row.get("exclusive", False)),
        )
    return coupons


def _subtotal(lines, currency):
    return sum((line.subtotal for line in lines), Money.zero(currency))


def _one(coupon, lines, currency):
    """What one coupon takes off on its own."""
    if coupon.kind == "percent":
        eligible = [ln for ln in lines if coupon.category in (None, ln.product.category)]
        return _subtotal(eligible, currency) * (Decimal(coupon.value) / 100)
    if coupon.kind == "fixed":
        return Money(coupon.value, currency)
    free = Money.zero(currency)
    for ln in lines:
        if ln.product.sku == coupon.value:
            free += ln.product.price * (ln.qty // 2)
    return free


_ORDER = {"percent": 0, "fixed": 1, "bogo": 2}


def _combined(coupons, lines, subtotal, currency):
    total = Money.zero(currency)
    for coupon in sorted(coupons, key=lambda c: _ORDER[c.kind]):
        if subtotal < Money(coupon.min_subtotal, currency):
            continue
        total += _one(coupon, lines, currency)
    return min(total, subtotal)


def discount_for(lines, coupons, currency="USD"):
    """The order's whole discount from `coupons`."""
    subtotal = _subtotal(lines, currency)
    exclusive = [c for c in coupons if c.exclusive]
    others = [c for c in coupons if not c.exclusive]
    options = [_combined([c], lines, subtotal, currency) for c in exclusive]
    options.append(_combined(others, lines, subtotal, currency))
    return max(options)
