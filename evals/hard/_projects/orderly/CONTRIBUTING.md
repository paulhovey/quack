# Contributing

- Amounts are always `Money` (`orderly/money.py`). Never use floats for money; parse what
  people type with `parse_amount`.
- State changes go through `Shop` so that the events go out. Code that reacts to an order
  changing subscribes to an event; it doesn't get called from `Shop` directly.
- The files in `data/orders/` are read by other tools: their keys don't change.
- Run the tests with `python -m unittest discover -s tests -t .` before you're done.
