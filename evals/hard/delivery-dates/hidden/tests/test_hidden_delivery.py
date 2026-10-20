import unittest
from datetime import date

from orderly.util.dates import add_business_days

from .support import fresh_world

THANKSGIVING = date(2026, 11, 26)


class DeliveryTest(unittest.TestCase):
    def test_weekends_are_skipped(self):
        self.assertEqual(add_business_days(date(2026, 10, 16), 1), date(2026, 10, 19))
        self.assertEqual(add_business_days(date(2026, 10, 16), 3), date(2026, 10, 21))
        self.assertEqual(add_business_days(date(2026, 10, 15), 2), date(2026, 10, 19))
        self.assertEqual(add_business_days(date(2026, 10, 17), 1), date(2026, 10, 19))

    def test_holidays_are_skipped(self):
        hols = {THANKSGIVING, date(2026, 12, 25)}
        self.assertEqual(add_business_days(date(2026, 11, 25), 1, hols), date(2026, 11, 27))
        self.assertEqual(add_business_days(date(2026, 11, 25), 2, hols), date(2026, 11, 30))
        self.assertEqual(add_business_days(date(2026, 12, 24), 1, hols), date(2026, 12, 28))

    def test_the_configured_holidays_are_dates(self):
        world = fresh_world(self)
        self.assertIn(THANKSGIVING, world.settings.holidays)

    def shipped_email(self, region, shipped):
        world = fresh_world(self)
        o = world.shop.place_order("ada@example.com", region, [("MUG-01", 1)], (), shipped)
        world.shop.pay(o.id)
        world.shop.ship(o.id, shipped)
        (mail,) = world.mailer.sent("shipped")
        return mail.body

    def test_the_email_from_the_report(self):
        self.assertIn("Monday 19 October", self.shipped_email("US-NY", date(2026, 10, 15)))

    def test_the_email_around_a_holiday(self):
        self.assertIn("Monday 30 November", self.shipped_email("DE", date(2026, 11, 23)))
