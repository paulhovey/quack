"""Orders to and from plain dicts, the format of data/orders/*.json.

The format is shared with the warehouse and accounting tools, which read
`grand_total` straight from the file: don't rename or drop keys.
"""

from .money import Money
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
        "lines": [
            {"sku": ln.product.sku, "qty": ln.qty, "unit_price": str(ln.price.amount)}
            for ln in order.lines
        ],
        "coupons": [c.code for c in order.coupons],
        "grand_total": str(order.grand_total.amount),
    }


def order_from_dict(data, shop):
    """`shop` supplies the catalog, coupons and settings to rebuild the order with."""
    if data.get("format", 1) != FORMAT:
        raise ValueError(f"order {data.get('id')}: unknown format {data.get('format')}")
    return Order(
        id=str(data["id"]),
        customer=data["customer"],
        region=shop.settings.region(data["region"]),
        lines=[_line_from_dict(ln, shop) for ln in data["lines"]],
        coupons=[shop.coupons[c] for c in data.get("coupons", [])],
        state=data["state"],
        placed_on=parse_date(data["placed_on"]) if data.get("placed_on") else None,
        shipped_on=parse_date(data["shipped_on"]) if data.get("shipped_on") else None,
    )


def _line_from_dict(data, shop):
    product = shop.catalog.get(data["sku"])
    price = data.get("unit_price")
    unit_price = Money(price, product.price.currency) if price is not None else None
    return OrderLine(product, int(data["qty"]), unit_price)
