def send_invoice(customer, amount):
    return f"invoice for {customer}: {amount}"


def void_invoice(number):
    return f"voided {number}"
