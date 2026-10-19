from shop.config import TAX_RATE


def receipt(amount):
    tax = amount * TAX_RATE
    return f"amount {amount:.2f} tax {tax:.2f} total {amount + tax:.2f}"
