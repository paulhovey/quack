import json
import unittest

from orderly.orders import InvalidTransition
from orderly.world import load_world

from .support import FRIDAY, fresh_world


class PartialShipmentTest(unittest.TestCase):
    def setUp(self):
        self.world = fresh_world(self)
        self.shop = self.world.shop
        self.stock = self.world.inventory
        self.order = self.shop.place_order(
            "ada@example.com", "UK", [("LAMP-01", 2), ("MUG-01", 3), ("PEN-01", 1)], (), FRIDAY
        )
        self.shop.pay(self.order.id)

    def shipments(self):
        return [e for e in self.world.bus.log if e.name == "order_shipped"]

    def test_shipping_some_lines(self):
        self.shop.ship(self.order.id, FRIDAY, skus=["LAMP-01"])
        self.assertEqual(self.order.state, "partially_shipped")
        self.assertEqual(self.stock.on_hand["LAMP-01"], 4)
        self.assertEqual(self.stock.on_hand["MUG-01"], 40)
        self.assertEqual(self.stock.available("MUG-01"), 37)
        (event,) = self.shipments()
        self.assertEqual([ln.product.sku for ln in event.data["lines"]], ["LAMP-01"])

    def test_then_the_rest(self):
        self.shop.ship(self.order.id, FRIDAY, skus=["LAMP-01", "PEN-01"])
        self.shop.ship(self.order.id, FRIDAY)
        self.assertEqual(self.order.state, "shipped")
        self.assertEqual(
            [self.stock.on_hand[s] for s in ("LAMP-01", "MUG-01", "PEN-01")], [4, 37, 9]
        )
        self.assertEqual([self.stock.available(s) for s in ("LAMP-01", "MUG-01")], [4, 37])
        first, second = self.shipments()
        self.assertEqual([ln.product.sku for ln in second.data["lines"]], ["MUG-01"])
        self.assertEqual(len(self.world.mailer.sent("shipped")), 2)

    def test_everything_at_once_is_still_one_shipment(self):
        self.shop.ship(self.order.id, FRIDAY)
        self.assertEqual(self.order.state, "shipped")
        (event,) = self.shipments()
        self.assertEqual(len(event.data["lines"]), 3)

    def test_bad_skus(self):
        with self.assertRaises(ValueError):
            self.shop.ship(self.order.id, FRIDAY, skus=["RUG-01"])
        self.shop.ship(self.order.id, FRIDAY, skus=["LAMP-01"])
        with self.assertRaises(ValueError):
            self.shop.ship(self.order.id, FRIDAY, skus=["LAMP-01"])
        self.assertEqual(len(self.shipments()), 1)
        self.assertEqual(self.stock.on_hand["LAMP-01"], 4)

    def test_unpaid_orders_still_cannot_ship(self):
        o = self.shop.place_order("bo@example.com", "UK", [("MUG-01", 1)], (), FRIDAY)
        with self.assertRaises(InvalidTransition):
            self.shop.ship(o.id, FRIDAY, skus=["MUG-01"])

    def test_no_cancelling_once_some_has_gone(self):
        self.shop.ship(self.order.id, FRIDAY, skus=["LAMP-01"])
        with self.assertRaises(InvalidTransition):
            self.shop.cancel(self.order.id)

    def test_saved_files(self):
        self.shop.ship(self.order.id, FRIDAY, skus=["MUG-01"])
        saved = json.loads((self.shop.store.root / f"{self.order.id}.json").read_text())
        self.assertEqual(saved["state"], "partially_shipped")
        self.assertEqual([ln.get("shipped") for ln in saved["lines"]], [None, True, None])
        again = self.shop.store.load(self.order.id, self.shop)
        self.assertEqual([ln.shipped for ln in again.lines], [False, True, False])

    def test_old_files_load(self):
        world = load_world()
        orders = world.shop.store.load_all(world.shop)
        self.assertEqual(len(orders), 8)
        paid = {o.id: o for o in orders}["1002"]
        self.assertTrue(all(not ln.shipped for ln in paid.lines))
