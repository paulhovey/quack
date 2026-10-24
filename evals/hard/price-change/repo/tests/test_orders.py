import unittest

from orderly.money import Money
from orderly.orders import InvalidTransition

from .support import FRIDAY, fresh_world


class OrderTest(unittest.TestCase):
    def setUp(self):
        self.shop = fresh_world(self).shop

    def test_totals(self):
        o = self.shop.place_order("ada@example.com", "UK", [("MUG-01", 2)], ["WELCOME10"], FRIDAY)
        self.assertEqual(o.subtotal, Money("17.00"))
        self.assertEqual(o.discount, Money("1.70"))
        self.assertEqual(o.net, Money("15.30"))
        self.assertEqual(o.tax, Money("3.06"))
        self.assertEqual(o.grand_total, Money("18.36"))

    def test_untaxed_lines(self):
        o = self.shop.place_order("bo@example.com", "UK", [("TEA-01", 2), ("MUG-01", 1)], (), FRIDAY)
        self.assertEqual(o.tax, Money("1.70"))
        self.assertEqual(o.grand_total, Money("23.40"))

    def test_line_nets_share_the_discount(self):
        o = self.shop.place_order(
            "cy@example.com", "US-OR", [("MUG-01", 1), ("BAG-01", 1)], ["WELCOME10"], FRIDAY
        )
        self.assertEqual(o.line_nets(), [Money("7.65"), Money("13.50")])

    def test_states(self):
        o = self.shop.place_order("di@example.com", "DE", [("PEN-01", 1)], (), FRIDAY)
        with self.assertRaises(InvalidTransition):
            self.shop.ship(o.id, FRIDAY)
        self.shop.pay(o.id)
        self.shop.ship(o.id, FRIDAY)
        self.assertEqual(o.state, "shipped")
        with self.assertRaises(InvalidTransition):
            self.shop.cancel(o.id)
