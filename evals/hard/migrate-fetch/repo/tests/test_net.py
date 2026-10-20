import unittest

from weather import net


class FetchTest(unittest.TestCase):
    def setUp(self):
        self.calls = []
        self.addCleanup(setattr, net, "TRANSPORT", net.TRANSPORT)

    def transport(self, fail_times):
        def t(url, timeout):
            self.calls.append((url, timeout))
            if len(self.calls) <= fail_times:
                raise OSError("down")
            return "body"

        net.TRANSPORT = t

    def test_retries_then_succeeds(self):
        self.transport(fail_times=2)
        self.assertEqual(net.fetch("u", timeout=4, retries=2), "body")
        self.assertEqual(self.calls, [("u", 4)] * 3)

    def test_gives_up(self):
        self.transport(fail_times=99)
        with self.assertRaises(net.FetchFailed):
            net.fetch("u", retries=1)
        self.assertEqual(self.calls, [("u", 10)] * 2)
