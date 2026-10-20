import unittest
from decimal import Decimal

from orderly.money import CurrencyMismatch, Money, parse_amount


class MoneyTest(unittest.TestCase):
    def test_rounds_to_the_minor_unit(self):
        self.assertEqual(Money("1.005").amount, Decimal("1.01"))
        self.assertEqual(Money("10", "JPY").amount, Decimal("10"))

    def test_arithmetic(self):
        self.assertEqual(Money("1.10") + Money("2.20"), Money("3.30"))
        self.assertEqual(Money("5") - Money("0.01"), Money("4.99"))
        self.assertEqual(Money("8.50") * 3, Money("25.50"))
        self.assertEqual(sum([Money("1"), Money("2")]), Money("3"))

    def test_no_floats(self):
        with self.assertRaises(TypeError):
            Money(0.1)
        with self.assertRaises(TypeError):
            Money("1") * 1.5

    def test_currencies_do_not_mix(self):
        with self.assertRaises(CurrencyMismatch):
            Money("1", "USD") + Money("1", "EUR")

    def test_parse_amount(self):
        self.assertEqual(parse_amount("$1,200.5"), Money("1200.50"))
        with self.assertRaises(ValueError):
            parse_amount("twelve")

    def test_allocate_by_weight(self):
        self.assertEqual(Money("10").allocate([1, 1]), [Money("5"), Money("5")])
        self.assertEqual(Money("6").allocate([1, 2]), [Money("2"), Money("4")])
