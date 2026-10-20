import unittest

from orderly.inventory import OutOfStock

from .support import FRIDAY, fresh_world


class ShopTest(unittest.TestCase):
    def setUp(self):
        self.world = fresh_world(self)
        self.shop = self.world.shop
        self.stock = self.world.inventory

    def test_placing_reserves_stock(self):
        self.shop.place_order("ada@example.com", "UK", [("LAMP-01", 2)], (), FRIDAY)
        self.assertEqual(self.stock.available("LAMP-01"), 4)
        self.assertEqual(self.stock.on_hand["LAMP-01"], 6)

    def test_an_order_that_cannot_be_filled_reserves_nothing(self):
        with self.assertRaises(OutOfStock):
            self.shop.place_order(
                "ada@example.com", "UK", [("MUG-01", 1), ("RUG-01", 3)], (), FRIDAY
            )
        self.assertEqual(self.stock.available("MUG-01"), 40)
        self.assertEqual(self.shop.orders, {})

    def test_shipping_takes_stock_off_the_shelf(self):
        o = self.shop.place_order("ada@example.com", "UK", [("LAMP-01", 2)], (), FRIDAY)
        self.shop.pay(o.id)
        self.shop.ship(o.id, FRIDAY)
        self.assertEqual(self.stock.on_hand["LAMP-01"], 4)
        self.assertEqual(self.stock.available("LAMP-01"), 4)

    def test_cancelling_an_unpaid_order_restocks(self):
        o = self.shop.place_order("ada@example.com", "UK", [("LAMP-01", 2)], (), FRIDAY)
        self.shop.cancel(o.id)
        self.assertEqual(self.stock.available("LAMP-01"), 6)
        self.assertEqual(len(self.world.mailer.sent("cancelled")), 1)
        self.assertEqual(self.world.mailer.sent("refund"), [])

    def test_emails(self):
        o = self.shop.place_order("ada@example.com", "UK", [("MUG-01", 2)], (), FRIDAY)
        (mail,) = self.world.mailer.sent("confirmation")
        self.assertEqual(mail.subject, f"Order {o.id} confirmed")
        self.assertIn("20.40 USD", mail.body)

    def test_orders_are_saved(self):
        o = self.shop.place_order("ada@example.com", "UK", [("MUG-01", 2)], (), FRIDAY)
        self.assertTrue(self.shop.store.exists(o.id))
        self.assertEqual(self.shop.store.next_id(), int(o.id) + 1)
