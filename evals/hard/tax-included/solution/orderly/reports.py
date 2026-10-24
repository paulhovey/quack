"""Sales reports over a set of orders: a day's summary, totals by category, CSV export."""

import csv
import io
from collections import defaultdict

from .money import Money
from .orders import CANCELLED


def counted(orders, day=None):
    """The orders a report counts: not cancelled, and placed on `day` if one is given."""
    return [o for o in orders if o.state != CANCELLED and (day is None or o.placed_on == day)]


def daily_summary(orders, day, currency="USD"):
    orders = counted(orders, day)
    zero = Money.zero(currency)
    return {
        "day": day,
        "orders": len(orders),
        "subtotal": sum((o.subtotal for o in orders), zero),
        "discount": sum((o.discount for o in orders), zero),
        "net": sum((o.net for o in orders), zero),
        "tax": sum((o.tax for o in orders), zero),
        "total": sum((o.grand_total for o in orders), zero),
    }


def by_category(orders, day=None, currency="USD"):
    """Net sales (after discounts, before tax) per product category."""
    totals = defaultdict(lambda: Money.zero(currency))
    for order in counted(orders, day):
        for line, net in zip(order.lines, order.line_nets()):
            totals[line.product.category] += net
    return dict(sorted(totals.items()))


def _cell(value):
    if isinstance(value, Money):
        return str(value.amount)
    if value is None:
        return ""
    return str(value)


def rows(orders, columns):
    """One list of values per order; `columns` are Order attribute names."""
    return [[_cell(getattr(o, c)) for c in columns] for o in orders]


def to_csv(orders, columns):
    out = io.StringIO()
    writer = csv.writer(out, lineterminator="\n")
    writer.writerow(columns)
    writer.writerows(rows(orders, columns))
    return out.getvalue()


def format_summary(summary):
    lines = [f"Sales on {summary['day'].isoformat()}: {summary['orders']} orders"]
    for key in ("subtotal", "discount", "net", "tax", "total"):
        lines.append(f"  {key:<9}{summary[key].amount:>12}")
    return "\n".join(lines)
