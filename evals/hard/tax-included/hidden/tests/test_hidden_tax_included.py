import unittest
from decimal import Decimal

from orderly.invoices import render_invoice
from orderly.money import Money

from .support import FRIDAY, fresh_world


class TaxIncludedTest(unittest.TestCase):
    def setUp(self):
        self.world = fresh_world(self)
        self.shop = self.world.shop

    def test_the_region(self):
        nz = self.world.settings.region("NZ")
        self.assertEqual((nz.tax_rate, nz.delivery_days, nz.tax_included), (Decimal("0.15"), 6, True))
        self.assertFalse(self.world.settings.region("UK").tax_included)

    def test_the_price_is_what_is_paid(self):
        o = self.shop.place_order("ada@example.com", "NZ", [("RUG-01", 1)], (), FRIDAY)
        self.assertEqual(o.grand_total, Money("129.00"))
        self.assertEqual(o.tax, Money("16.83"))

    def test_discounts_and_untaxed_lines(self):
        o = self.shop.place_order(
            "ada@example.com", "NZ", [("TEA-01", 1), ("MUG-01", 1)], ["WELCOME10"], FRIDAY
        )
        self.assertEqual(o.net, Money("13.27"))
        self.assertEqual(o.grand_total, Money("13.27"))
        self.assertEqual(o.tax, Money("1.00"))

    def test_the_invoice(self):
        o = self.shop.place_order("ada@example.com", "NZ", [("RUG-01", 1)], (), FRIDAY)
        text = render_invoice(o)
        self.assertRegex(text, r"Tax \(included\) +16\.83")
        self.assertRegex(text, r"Total +129\.00 USD")
        uk = self.shop.place_order("bo@example.com", "UK", [("RUG-01", 1)], (), FRIDAY)
        text = render_invoice(uk)
        self.assertNotIn("included", text)
        self.assertRegex(text, r"Total +154\.80 USD")

    def test_other_regions_are_unchanged(self):
        o = self.shop.place_order("ada@example.com", "UK", [("MUG-01", 2)], ["WELCOME10"], FRIDAY)
        self.assertEqual((o.tax, o.grand_total), (Money("3.06"), Money("18.36")))

    def test_saved_and_loaded(self):
        o = self.shop.place_order("ada@example.com", "NZ", [("RUG-01", 1)], (), FRIDAY)
        again = self.shop.store.load(o.id, self.shop)
        self.assertEqual(again.grand_total, Money("129.00"))

    def test_the_daily_report(self):
        from orderly import reports

        nz = self.shop.place_order("ada@example.com", "NZ", [("RUG-01", 1)], (), FRIDAY)
        uk = self.shop.place_order("bo@example.com", "UK", [("MUG-01", 2)], ["WELCOME10"], FRIDAY)
        s = reports.daily_summary([nz, uk], FRIDAY)
        self.assertEqual(s["total"], Money("147.36"))
        self.assertEqual(s["tax"], Money("19.89"))
        self.assertEqual(s["net"], Money("144.30"))
