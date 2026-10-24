import json
import unittest

from .support import FRIDAY, fresh_world


class CancelTest(unittest.TestCase):
    def setUp(self):
        self.world = fresh_world(self)
        self.shop = self.world.shop
        self.stock = self.world.inventory
        self.mail = self.world.mailer

    def test_a_paid_order_restocks(self):
        o = self.shop.place_order("ada@example.com", "UK", [("LAMP-01", 2), ("MUG-01", 3)], (), FRIDAY)
        self.shop.pay(o.id)
        self.shop.cancel(o.id)
        self.assertEqual(self.stock.available("LAMP-01"), 6)
        self.assertEqual(self.stock.available("MUG-01"), 40)
        self.assertEqual(self.stock.on_hand["LAMP-01"], 6)

    def test_a_paid_order_gets_one_of_each_email(self):
        o = self.shop.place_order("ada@example.com", "UK", [("LAMP-01", 1)], (), FRIDAY)
        self.shop.pay(o.id)
        self.shop.cancel(o.id)
        self.assertEqual(len(self.mail.sent("cancelled")), 1)
        (refund,) = self.mail.sent("refund")
        self.assertIn("65.88 USD", refund.body)

    def test_an_unpaid_order_gets_no_refund(self):
        o = self.shop.place_order("ada@example.com", "UK", [("LAMP-01", 1)], (), FRIDAY)
        self.shop.cancel(o.id)
        self.assertEqual(len(self.mail.sent("cancelled")), 1)
        self.assertEqual(self.mail.sent("refund"), [])
        self.assertEqual(self.stock.available("LAMP-01"), 6)

    def test_the_saved_order_is_cancelled(self):
        o = self.shop.place_order("ada@example.com", "UK", [("LAMP-01", 1)], (), FRIDAY)
        self.shop.pay(o.id)
        self.shop.cancel(o.id)
        saved = json.loads((self.shop.store.root / f"{o.id}.json").read_text())
        self.assertEqual(saved["state"], "cancelled")

    def test_every_cancel_announces_itself_once(self):
        o = self.shop.place_order("ada@example.com", "UK", [("LAMP-01", 1)], (), FRIDAY)
        self.shop.pay(o.id)
        self.shop.cancel(o.id)
        names = self.world.bus.names()
        self.assertEqual(names.count("order_cancelled"), 1)
        self.assertEqual(names.count("order_refunded"), 1)

    def test_the_stock_report_after_a_paid_cancel(self):
        o = self.shop.place_order("ada@example.com", "UK", [("RUG-01", 2)], (), FRIDAY)
        self.shop.pay(o.id)
        self.shop.cancel(o.id)
        again = self.shop.place_order("bo@example.com", "UK", [("RUG-01", 2)], (), FRIDAY)
        self.assertEqual(again.state, "placed")
        self.assertEqual(self.stock.available("RUG-01"), 0)
