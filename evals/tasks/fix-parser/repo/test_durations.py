import unittest

from durations import parse_duration


class ParseDuration(unittest.TestCase):
    def test_single(self):
        self.assertEqual(parse_duration("90s"), 90)
        self.assertEqual(parse_duration("5m"), 300)

    def test_combined(self):
        self.assertEqual(parse_duration("1h30m"), 5400)
        self.assertEqual(parse_duration("2m5s"), 125)

    def test_bad(self):
        with self.assertRaises(ValueError):
            parse_duration("soon")
        with self.assertRaises(ValueError):
            parse_duration("")


if __name__ == "__main__":
    unittest.main()
