import unittest

from stats.median import median


class Median(unittest.TestCase):
    def test_odd(self):
        self.assertEqual(median([3, 1, 2]), 2)

    def test_even(self):
        self.assertEqual(median([4, 1, 3, 2]), 2.5)

    def test_empty(self):
        self.assertIsNone(median([]))


if __name__ == "__main__":
    unittest.main()
