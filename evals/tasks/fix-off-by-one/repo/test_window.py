import unittest

from window import last_n


class LastN(unittest.TestCase):
    def test_two(self):
        self.assertEqual(last_n([1, 2, 3, 4], 2), [3, 4])

    def test_zero(self):
        self.assertEqual(last_n([1, 2], 0), [])

    def test_all(self):
        self.assertEqual(last_n([1, 2], 5), [1, 2])


if __name__ == "__main__":
    unittest.main()
