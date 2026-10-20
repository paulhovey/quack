import unittest
from datetime import date

from orderly.util.dates import add_business_days, is_business_day, parse_date


class DatesTest(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(parse_date(" 2026-10-16 "), date(2026, 10, 16))

    def test_business_days(self):
        self.assertTrue(is_business_day(date(2026, 10, 16)))
        self.assertFalse(is_business_day(date(2026, 10, 17)))

    def test_within_a_week(self):
        monday = date(2026, 10, 12)
        self.assertEqual(add_business_days(monday, 1), date(2026, 10, 13))
        self.assertEqual(add_business_days(monday, 3), date(2026, 10, 15))
