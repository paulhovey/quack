# orderly

A small shop backend: a catalog, coupons, sales tax by region, stock, customer emails,
saved orders and sales reports.

```sh
python -m orderly report --day 2026-10-16
python -m orderly report --day 2026-10-16 --by-category
python -m orderly export --out orders.csv
python -m orderly invoice 1003
python -m unittest discover -s tests -t .
```

## Layout

- `orderly/money.py`: `Money`, the only way amounts are handled.
- `orderly/catalog.py`, `discounts.py`, `orders.py`: products, coupons, orders and totals.
- `orderly/shop.py`: placing, paying, shipping and cancelling; each emits an event
  (`orderly/events.py`) that `inventory.py` and `notifications.py` act on.
- `orderly/serialize.py`, `storage.py`: the order files in `data/orders/`.
- `orderly/reports.py`, `invoices.py`, `cli.py`: what people read.
- `config/settings.json`: regions (tax, delivery times), holidays, email templates and the
  export's columns.
