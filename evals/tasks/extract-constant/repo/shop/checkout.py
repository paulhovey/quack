def receipt(amount):
    tax = amount * 0.2
    return f"amount {amount:.2f} tax {tax:.2f} total {amount + tax:.2f}"
