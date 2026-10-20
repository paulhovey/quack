"""The shop: places, pays for, ships and cancels orders, and announces each change.

Events (data in brackets):
- `order_placing` (order): before an order exists; a handler may refuse it by raising.
- `order_placed` (order)
- `order_paid` (order)
- `order_shipped` (order, day)
- `order_refunded` (order, amount)
- `order_cancelled` (order, previous): `previous` is the state it was cancelled from.
"""

from datetime import date

from .orders import CANCELLED, PAID, PLACED, SHIPPED, Order, OrderLine


class UnknownOrder(KeyError):
    pass


class Shop:
    def __init__(self, catalog, coupons, settings, bus, store=None):
        self.catalog = catalog
        self.coupons = coupons
        self.settings = settings
        self.bus = bus
        self.store = store
        self.orders = {}
        self._next_id = store.next_id() if store else 1001

    def place_order(self, customer, region, items, coupons=(), day=None):
        """`items`: (sku, qty) pairs. `coupons`: codes."""
        lines = []
        for sku, qty in items:
            if qty <= 0:
                raise ValueError(f"{sku}: quantity must be positive")
            lines.append(OrderLine(self.catalog.get(sku), qty))
        if not lines:
            raise ValueError("an order needs at least one line")
        order = Order(
            id=str(self._next_id),
            customer=customer,
            region=self.settings.region(region),
            lines=lines,
            coupons=[self._coupon(c) for c in coupons],
            placed_on=day or date.today(),
        )
        self.bus.emit("order_placing", order=order)
        self._next_id += 1
        self.orders[order.id] = order
        self._save(order)
        self.bus.emit("order_placed", order=order)
        return order

    def pay(self, order_id):
        order = self.get(order_id)
        order.move_to(PAID)
        self._save(order)
        self.bus.emit("order_paid", order=order)
        return order

    def ship(self, order_id, day=None):
        order = self.get(order_id)
        order.move_to(SHIPPED)
        order.shipped_on = day or date.today()
        self._save(order)
        self.bus.emit("order_shipped", order=order, day=order.shipped_on)
        return order

    def cancel(self, order_id):
        order = self.get(order_id)
        previous = order.state
        order.move_to(CANCELLED)
        self._save(order)
        if previous == PAID:
            return self._refund(order)
        self.bus.emit("order_cancelled", order=order, previous=previous)
        return order

    def _refund(self, order):
        """A paid order's money goes back to the card; accounting books it from the event."""
        self.bus.emit("order_refunded", order=order, amount=order.grand_total)
        return order

    def get(self, order_id):
        order_id = str(order_id)
        if order_id not in self.orders and self.store and self.store.exists(order_id):
            self.orders[order_id] = self.store.load(order_id, self)
        try:
            return self.orders[order_id]
        except KeyError:
            raise UnknownOrder(order_id) from None

    def open_orders(self):
        return [o for o in self.orders.values() if o.state in (PLACED, PAID)]

    def _coupon(self, code):
        from .discounts import UnknownCoupon

        try:
            return self.coupons[code]
        except KeyError:
            raise UnknownCoupon(code) from None

    def _save(self, order):
        if self.store:
            self.store.save(order)
