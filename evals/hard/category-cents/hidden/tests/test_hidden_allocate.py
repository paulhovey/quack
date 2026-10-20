import random
import unittest
from datetime import date

from orderly import reports
from orderly.money import Money
from orderly.world import load_world


class AllocateTest(unittest.TestCase):
    def test_leftover_cents_go_to_the_largest_remainders(self):
        self.assertEqual(Money("0.05").allocate([1, 1]), [Money("0.03"), Money("0.02")])
        self.assertEqual(
            Money("1.00").allocate([1, 1, 1]), [Money("0.34"), Money("0.33"), Money("0.33")]
        )
        self.assertEqual(Money("1.00").allocate([1, 2]), [Money("0.33"), Money("0.67")])

    def test_parts_always_add_up(self):
        rng = random.Random(7)
        for _ in range(500):
            amount = Money(f"{rng.randint(-5000, 5000) / 100:.2f}")
            weights = [rng.randint(1, 9999) for _ in range(rng.randint(1, 6))]
            parts = amount.allocate(weights)
            self.assertEqual(sum(parts, Money.zero()), amount, (amount, weights))

    def test_the_saved_day_adds_up(self):
        world = load_world()
        orders = world.shop.store.load_all(world.shop)
        day = date(2026, 10, 16)
        cats = reports.by_category(orders, day)
        self.assertEqual(sum(cats.values(), Money.zero()), reports.daily_summary(orders, day)["net"])
