import csv
import io
import tempfile
import unittest
from pathlib import Path

from orderly import reports
from orderly.cli import main

from .support import FRIDAY, fresh_world

NAMES = ["Lovelace, Ada", 'Bo "the builder" Baggins', "Cy\nOn two lines", "plain@example.com"]


class CsvTest(unittest.TestCase):
    def test_awkward_values_read_back(self):
        world = fresh_world(self)
        orders = [
            world.shop.place_order(n, "UK", [("MUG-01", 1)], (), FRIDAY) for n in NAMES
        ]
        columns = world.settings.report_columns
        rows = list(csv.reader(io.StringIO(reports.to_csv(orders, columns))))
        self.assertEqual(rows[0], list(columns))
        self.assertEqual([r[columns.index("customer")] for r in rows[1:]], NAMES)
        self.assertTrue(all(len(r) == len(columns) for r in rows))
        self.assertEqual(rows[1][columns.index("grand_total")], "10.20")

    def test_the_command_writes_a_readable_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "orders.csv"
            self.assertEqual(main(["export", "--out", str(out)], out=io.StringIO()), 0)
            with open(out, newline="", encoding="utf-8") as f:
                rows = list(csv.reader(f))
        self.assertEqual(len(rows), 9)
        (ada,) = [r for r in rows if r[0] == "1006"]
        self.assertEqual(ada[2], "Lovelace, Ada")
        self.assertEqual(ada[-1], "86.88")
