import unittest

from orderly.money import Money
from orderly.orders import OrderLine

from .support import fresh_world


class DiscountTest(unittest.TestCase):
    def setUp(self):
        self.world = fresh_world(self)
        self.catalog = self.world.catalog
        self.coupons = self.world.shop.coupons

    def order(self, items, codes):
        from orderly.discounts import discount_for

        lines = [OrderLine(self.catalog.get(s), q) for s, q in items]
        return discount_for(lines, [self.coupons[c] for c in codes])

    def test_percent(self):
        self.assertEqual(self.order([("MUG-01", 2)], ["WELCOME10"]), Money("1.70"))

    def test_percent_on_one_category(self):
        d = self.order([("LAMP-01", 1), ("MUG-01", 1)], ["SUMMER40"])
        self.assertEqual(d, Money("21.96"))

    def test_fixed_needs_the_minimum(self):
        self.assertEqual(self.order([("MUG-01", 2)], ["FLAT5"]), Money("0"))
        self.assertEqual(self.order([("MUG-01", 3)], ["FLAT5"]), Money("5"))

    def test_bogo(self):
        self.assertEqual(self.order([("TEA-01", 5)], ["TEA2"]), Money("13.20"))

    def test_percent_coupons_do_not_compound(self):
        d = self.order([("BOOK-02", 2)], ["WELCOME10", "STATIONERY15"])
        self.assertEqual(d, Money("9.38"))

    def test_never_more_than_the_subtotal(self):
        self.assertEqual(self.order([("BOOK-01", 1)], ["FLAT5", "WELCOME10"]), Money("0.50"))
