import unittest

from slug import slugify


class Slugify(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(slugify("Hello, World!"), "hello-world")

    def test_runs(self):
        self.assertEqual(slugify("  a -- b__c  "), "a-b-c")

    def test_digits(self):
        self.assertEqual(slugify("Version 2.0"), "version-2-0")


if __name__ == "__main__":
    unittest.main()
