import io
import unittest

from orderly.cli import main


class CliTest(unittest.TestCase):
    def run_cli(self, *args):
        out = io.StringIO()
        self.assertEqual(main(list(args), out=out), 0)
        return out.getvalue()

    def test_report(self):
        text = self.run_cli("report", "--day", "2026-10-16")
        self.assertIn("Sales on 2026-10-16", text)

    def test_invoice(self):
        text = self.run_cli("invoice", "1001")
        self.assertIn("Invoice 1001", text)
        self.assertIn("Total", text)
