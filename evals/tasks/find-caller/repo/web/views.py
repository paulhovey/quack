from billing.invoice import void_invoice


def cancel(request):
    return void_invoice(request["number"])
