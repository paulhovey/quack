"""Orders, their lines and totals, and the states an order moves through."""

from dataclasses import dataclass, field
from datetime import date
from typing import Optional

from .catalog import Product
from .config import Region
from .discounts import discount_for
from .money import Money

PLACED = "placed"
PAID = "paid"
SHIPPED = "shipped"
CANCELLED = "cancelled"

TRANSITIONS = {
    PLACED: {PAID, CANCELLED},
    PAID: {SHIPPED, CANCELLED},
    SHIPPED: set(),
    CANCELLED: set(),
}


class InvalidTransition(ValueError):
    pass


@dataclass
class OrderLine:
    product: Product
    qty: int

    @property
    def subtotal(self):
        return self.product.price * self.qty


@dataclass
class Order:
    id: str
    customer: str
    region: Region
    lines: list
    coupons: list = field(default_factory=list)
    state: str = PLACED
    placed_on: Optional[date] = None
    shipped_on: Optional[date] = None

    @property
    def region_code(self):
        return self.region.code

    @property
    def currency(self):
        return self.lines[0].product.price.currency if self.lines else "USD"

    @property
    def subtotal(self):
        return sum((ln.subtotal for ln in self.lines), Money.zero(self.currency))

    @property
    def discount(self):
        return discount_for(self.lines, self.coupons, self.currency)

    def line_nets(self):
        """Each line's subtotal less its share of the discount, shared by subtotal."""
        discount = self.discount
        if not discount:
            return [ln.subtotal for ln in self.lines]
        shares = discount.allocate([ln.subtotal.amount for ln in self.lines])
        return [ln.subtotal - s for ln, s in zip(self.lines, shares)]

    @property
    def net(self):
        return self.subtotal - self.discount

    @property
    def tax(self):
        taxable = sum(
            (n for ln, n in zip(self.lines, self.line_nets()) if ln.product.taxable),
            Money.zero(self.currency),
        )
        return taxable * self.region.tax_rate

    @property
    def total_due(self):
        return self.net + self.tax

    def move_to(self, state):
        if state not in TRANSITIONS[self.state]:
            raise InvalidTransition(f"order {self.id}: cannot go from {self.state} to {state}")
        self.state = state
