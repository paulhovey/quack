import csv
import io
import json
import re
import unittest
from pathlib import Path

from orderly import reports
from orderly.cli import main
from orderly.money import Money
from orderly.orders import Order
from orderly.world import load_world

from .support import FRIDAY, fresh_world

ROOT = Path(__file__).resolve().parent.parent


class RenameTest(unittest.TestCase):
    def test_the_property_is_renamed(self):
        self.assertTrue(hasattr(Order, "total_due"))
        self.assertFalse(hasattr(Order, "grand_total"))

    def test_no_code_reads_the_old_name(self):
        for path in (ROOT / "orderly").rglob("*.py"):
            text = path.read_text(encoding="utf-8")
            self.assertNotRegex(text, r"\.grand_total\b", path.name)
            self.assertNotIn("def grand_total", text, path.name)

    def test_templates_and_columns(self):
        world = fresh_world(self)
        settings = json.loads((ROOT / "config" / "settings.json").read_text(encoding="utf-8"))
        self.assertNotIn("grand_total", json.dumps(settings))
        self.assertIn("total_due", settings["report_columns"])
        o = world.shop.place_order("ada@example.com", "UK", [("MUG-01", 2)], (), FRIDAY)
        (mail,) = world.mailer.sent("confirmation")
        self.assertIn("20.40 USD", mail.body)
        world.shop.pay(o.id)
        world.shop.cancel(o.id)
        (refund,) = world.mailer.sent("refund")
        self.assertIn("20.40 USD", refund.body)
        rows = list(csv.reader(io.StringIO(reports.to_csv([o], world.settings.report_columns))))
        self.assertEqual(rows[1][rows[0].index("total_due")], "20.40")

    def test_saved_files_keep_their_keys(self):
        world = fresh_world(self)
        o = world.shop.place_order("ada@example.com", "UK", [("MUG-01", 2)], (), FRIDAY)
        saved = json.loads((world.shop.store.root / f"{o.id}.json").read_text())
        self.assertEqual(
            sorted(saved),
            sorted(["format", "id", "customer", "region", "state", "placed_on", "shipped_on",
                    "lines", "coupons", "grand_total"]),
        )
        self.assertEqual(saved["grand_total"], "20.40")

    def test_old_files_load(self):
        world = load_world()
        orders = world.shop.store.load_all(world.shop)
        self.assertEqual(len(orders), 8)
        by_id = {o.id: o for o in orders}
        self.assertEqual(by_id["1006"].total_due, Money("86.88"))

    def test_the_cli_still_works(self):
        for args in (["export"], ["invoice", "1006"], ["report", "--day", "2026-10-16"]):
            out = io.StringIO()
            self.assertEqual(main(args, out=out), 0, args)
            if args[0] == "invoice":
                self.assertIn("86.88", out.getvalue())
