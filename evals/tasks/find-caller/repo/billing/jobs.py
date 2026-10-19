from billing.invoice import send_invoice


def monthly(customers):
    return [send_invoice(c, 10) for c in customers]
