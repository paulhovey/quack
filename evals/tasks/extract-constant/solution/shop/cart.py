from shop.config import TAX_RATE


def total(prices):
    return round(sum(prices) * (1 + TAX_RATE), 2)
