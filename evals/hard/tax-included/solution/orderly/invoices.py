"""Plain-text invoices."""

from .util.text import columns, shorten

WIDTH = 44


def render_invoice(order):
    out = [f"Invoice {order.id} for {order.customer}", ""]
    for ln in order.lines:
        left = f"{ln.qty} x {shorten(ln.product.name, 24)}"
        out.append(columns(left, str(ln.subtotal.amount), WIDTH))
    out.append("")
    out.append(columns("Subtotal", str(order.subtotal.amount), WIDTH))
    if order.discount:
        codes = ", ".join(c.code for c in order.coupons)
        out.append(columns(f"Discount ({codes})", f"-{order.discount.amount}", WIDTH))
    label = "Tax (included)" if order.region.tax_included else "Tax"
    out.append(columns(label, str(order.tax.amount), WIDTH))
    out.append(columns("Total", f"{order.grand_total.amount} {order.currency}", WIDTH))
    return "\n".join(out) + "\n"
