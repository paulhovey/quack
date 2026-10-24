import io
import json
import tempfile
import unittest
from pathlib import Path

from orderly import reports
from orderly.cli import main
from orderly.money import Money
from orderly.world import ROOT, load_world

from .support import FRIDAY, fresh_world


class OldPricesTest(unittest.TestCase):
    def setUp(self):
        self.world = load_world()
        self.orders = self.world.shop.store.load_all(self.world.shop)

    def test_every_saved_order_totals_what_its_file_says(self):
        for order in self.orders:
            saved = json.loads((ROOT / "data" / "orders" / f"{order.id}.json").read_text())
            self.assertEqual(str(order.grand_total.amount), saved["grand_total"], order.id)

    def test_the_day_report_is_what_was_taken(self):
        s = reports.daily_summary(self.orders, FRIDAY)
        self.assertEqual((s["discount"], s["total"]), (Money("81.17"), Money("224.24")))

    def test_the_invoice(self):
        out = io.StringIO()
        self.assertEqual(main(["invoice", "1004"], out=out), 0)
        self.assertRegex(out.getvalue(), r"4 x Breakfast tea, 250g +25\.00")
        self.assertRegex(out.getvalue(), r"Discount \(TEA2\) +-12\.50")
        self.assertRegex(out.getvalue(), r"Total +26\.01 USD")

    def test_saving_an_old_order_keeps_its_prices(self):
        with tempfile.TemporaryDirectory() as tmp:
            world = load_world(orders_dir=tmp)
            old = self.world.shop.get("1004")
            world.shop.store.save(old)
            saved = json.loads((Path(tmp) / "1004.json").read_text())
            self.assertEqual([ln["unit_price"] for ln in saved["lines"]], ["6.25", "12.00"])
            self.assertEqual(saved["grand_total"], "26.01")
            again = world.shop.store.load("1004", world.shop)
            self.assertEqual(again.grand_total, Money("26.01"))


class NewPricesTest(unittest.TestCase):
    def test_new_orders_pay_todays_prices(self):
        shop = fresh_world(self).shop
        o = shop.place_order("ada@example.com", "DE", [("TEA-01", 4), ("MUG-02", 1)], ["TEA2"], FRIDAY)
        self.assertEqual((o.subtotal, o.discount), (Money("39.30"), Money("13.20")))
        again = shop.store.load(o.id, shop)
        self.assertEqual(again.grand_total, o.grand_total)
        saved = json.loads((shop.store.root / f"{o.id}.json").read_text())
        self.assertEqual([ln["unit_price"] for ln in saved["lines"]], ["6.60", "12.90"])
