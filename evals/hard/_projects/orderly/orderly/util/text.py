"""Text helpers for emails and invoices."""


def columns(left, right, width=40):
    """'Subtotal' and '17.00' as one line, the right part flush right."""
    gap = max(1, width - len(left) - len(right))
    return f"{left}{' ' * gap}{right}"


def shorten(text, width):
    if len(text) <= width:
        return text
    return text[: max(0, width - 1)] + "…"
