import csv
import io
import unittest

from orderly import reports
from orderly.money import Money

from .support import FRIDAY, fresh_world


class ReportTest(unittest.TestCase):
    def setUp(self):
        self.world = fresh_world(self)
        shop = self.world.shop
        shop.place_order("ada@example.com", "UK", [("MUG-01", 2)], ["WELCOME10"], FRIDAY)
        shop.place_order("bo@example.com", "US-CA", [("LAMP-01", 1), ("CANDLE-01", 2)], (), FRIDAY)
        gone = shop.place_order("cy@example.com", "DE", [("PEN-01", 1)], (), FRIDAY)
        shop.cancel(gone.id)
        self.orders = list(shop.orders.values())

    def test_daily_summary(self):
        s = reports.daily_summary(self.orders, FRIDAY)
        self.assertEqual(s["orders"], 2)
        self.assertEqual(s["net"], Money("92.80"))
        self.assertEqual(s["total"], Money("101.48"))

    def test_categories_add_up_to_the_day(self):
        shop = self.world.shop
        shop.place_order(
            "di@example.com", "US-NY", [("BOOK-02", 1), ("TEA-01", 1)], ["WELCOME10"], FRIDAY
        )
        shop.place_order(
            "ed@example.com", "US-NY", [("BAG-01", 1), ("BOOK-02", 1), ("TEA-01", 1)],
            ["WELCOME10"], FRIDAY,
        )
        orders = list(shop.orders.values())
        cats = reports.by_category(orders, FRIDAY)
        self.assertEqual(sum(cats.values()), reports.daily_summary(orders, FRIDAY)["net"])

    def test_csv(self):
        text = reports.to_csv(self.orders, self.world.settings.report_columns)
        rows = list(csv.reader(io.StringIO(text)))
        self.assertEqual(rows[0], list(self.world.settings.report_columns))
        self.assertEqual(len(rows), 4)
        self.assertEqual(rows[1][-1], "18.36")
