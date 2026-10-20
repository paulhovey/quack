import unittest

from .support import FRIDAY, fresh_world


class StorageTest(unittest.TestCase):
    def test_round_trip(self):
        world = fresh_world(self)
        shop = world.shop
        o = shop.place_order("ada@example.com", "UK", [("MUG-01", 2)], ["WELCOME10"], FRIDAY)
        shop.pay(o.id)
        again = shop.store.load(o.id, shop)
        self.assertEqual(again.state, "paid")
        self.assertEqual(again.grand_total, o.grand_total)
        self.assertEqual([c.code for c in again.coupons], ["WELCOME10"])
