"""Stock on hand and stock reserved by open orders, kept in step through events.

Placing an order reserves its lines; shipping takes them off the shelf; cancelling an
order that hasn't shipped puts its reservation back.
"""

import json
from collections import defaultdict
from pathlib import Path


class OutOfStock(Exception):
    def __init__(self, sku, wanted, available):
        super().__init__(f"{sku}: wanted {wanted}, {available} available")
        self.sku = sku


class Inventory:
    def __init__(self, on_hand):
        self.on_hand = dict(on_hand)
        self.reserved = defaultdict(int)

    @classmethod
    def load(cls, path):
        return cls(json.loads(Path(path).read_text(encoding="utf-8")))

    def available(self, sku):
        return self.on_hand.get(sku, 0) - self.reserved[sku]

    def reserve(self, sku, qty):
        if qty > self.available(sku):
            raise OutOfStock(sku, qty, self.available(sku))
        self.reserved[sku] += qty

    def release(self, sku, qty):
        self.reserved[sku] = max(0, self.reserved[sku] - qty)

    def commit(self, sku, qty):
        self.release(sku, qty)
        self.on_hand[sku] = self.on_hand.get(sku, 0) - qty

    def subscribe(self, bus):
        bus.subscribe("order_placing", self._on_placing)
        bus.subscribe("order_cancelled", self._on_cancelled)
        bus.subscribe("order_shipped", self._on_shipped)

    def _on_placing(self, event):
        lines = event.data["order"].lines
        # Check everything first, so a failed order reserves nothing.
        wanted = defaultdict(int)
        for ln in lines:
            wanted[ln.product.sku] += ln.qty
        for sku, qty in wanted.items():
            if qty > self.available(sku):
                raise OutOfStock(sku, qty, self.available(sku))
        for sku, qty in wanted.items():
            self.reserve(sku, qty)

    def _on_cancelled(self, event):
        for ln in event.data["order"].lines:
            self.release(ln.product.sku, ln.qty)

    def _on_shipped(self, event):
        for ln in event.data["order"].lines:
            self.commit(ln.product.sku, ln.qty)
