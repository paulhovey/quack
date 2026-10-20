import unittest

from orderly.discounts import Coupon, discount_for
from orderly.money import Money
from orderly.orders import OrderLine

from .support import FRIDAY, fresh_world


class ExclusiveTest(unittest.TestCase):
    def setUp(self):
        self.world = fresh_world(self)
        self.coupons = self.world.shop.coupons

    def discount(self, items, coupons):
        lines = [OrderLine(self.world.catalog.get(s), q) for s, q in items]
        return discount_for(lines, coupons)

    def codes(self, *codes):
        return [self.coupons[c] for c in codes]

    def test_the_flag_loads(self):
        self.assertTrue(self.coupons["SUMMER40"].exclusive)
        self.assertFalse(self.coupons["WELCOME10"].exclusive)
        self.assertFalse(Coupon("X", "fixed", "1").exclusive)

    def test_the_exclusive_coupon_wins_when_it_is_bigger(self):
        d = self.discount([("RUG-01", 1)], self.codes("WELCOME10", "SUMMER40"))
        self.assertEqual(d, Money("51.60"))
        d = self.discount([("MUG-01", 2), ("CANDLE-01", 1)], self.codes("SUMMER40", "WELCOME10"))
        self.assertEqual(d, Money("4.52"))

    def test_the_others_together_win_when_they_are_bigger(self):
        d = self.discount(
            [("BOOK-02", 2), ("CANDLE-01", 1)],
            self.codes("SUMMER40", "WELCOME10", "STATIONERY15"),
        )
        self.assertEqual(d, Money("10.51"))

    def test_two_exclusive_coupons_never_combine(self):
        a = Coupon("A", "percent", "30", exclusive=True)
        b = Coupon("B", "fixed", "20.00", exclusive=True)
        self.assertEqual(self.discount([("BAG-02", 1)], [a, b]), Money("26.70"))
        self.assertEqual(self.discount([("BAG-01", 1)], [a, b]), Money("15.00"))

    def test_an_exclusive_coupon_below_its_minimum_does_nothing(self):
        a = Coupon("A", "fixed", "50.00", min_subtotal="500", exclusive=True)
        d = self.discount([("MUG-01", 2)], [a] + self.codes("WELCOME10"))
        self.assertEqual(d, Money("1.70"))

    def test_without_exclusive_coupons_nothing_changes(self):
        d = self.discount([("BOOK-02", 2)], self.codes("WELCOME10", "STATIONERY15"))
        self.assertEqual(d, Money("9.38"))

    def test_orders_use_it(self):
        o = self.world.shop.place_order(
            "ada@example.com", "UK", [("RUG-01", 1)], ["SUMMER40", "FLAT5"], FRIDAY
        )
        self.assertEqual(o.discount, Money("51.60"))
        self.assertEqual(o.grand_total, Money("92.88"))
