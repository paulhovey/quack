"""Orders to and from plain dicts, the format of data/orders/*.json.

The format is shared with the warehouse and accounting tools, which read
`grand_total` straight from the file: don't rename or drop keys.
"""

from .orders import Order, OrderLine
from .util.dates import parse_date

FORMAT = 1


def order_to_dict(order):
    return {
        "format": FORMAT,
        "id": order.id,
        "customer": order.customer,
        "region": order.region.code,
        "state": order.state,
        "placed_on": order.placed_on.isoformat() if order.placed_on else None,
        "shipped_on": order.shipped_on.isoformat() if order.shipped_on else None,
        "lines": [_line_to_dict(ln) for ln in order.lines],
        "coupons": [c.code for c in order.coupons],
        "grand_total": str(order.grand_total.amount),
    }


def _line_to_dict(line):
    out = {"sku": line.product.sku, "qty": line.qty, "unit_price": str(line.product.price.amount)}
    if line.shipped:
        out["shipped"] = True
    return out


def order_from_dict(data, shop):
    """`shop` supplies the catalog, coupons and settings to rebuild the order with."""
    if data.get("format", 1) != FORMAT:
        raise ValueError(f"order {data.get('id')}: unknown format {data.get('format')}")
    return Order(
        id=str(data["id"]),
        customer=data["customer"],
        region=shop.settings.region(data["region"]),
        lines=[
            OrderLine(shop.catalog.get(ln["sku"]), int(ln["qty"]), bool(ln.get("shipped", False)))
            for ln in data["lines"]
        ],
        coupons=[shop.coupons[c] for c in data.get("coupons", [])],
        state=data["state"],
        placed_on=parse_date(data["placed_on"]) if data.get("placed_on") else None,
        shipped_on=parse_date(data["shipped_on"]) if data.get("shipped_on") else None,
    )
