import inspect
import unittest

from scripts import backfill
from weather import alerts, cache, forecast, net, sources, stations


class MigrationTest(unittest.TestCase):
    def setUp(self):
        self.calls = []
        self.addCleanup(setattr, net, "TRANSPORT", net.TRANSPORT)

        def down(url, timeout):
            self.calls.append(timeout)
            raise OSError("down")

        net.TRANSPORT = down
        cache.clear()

    def attempts(self, call):
        """(timeout, attempts) a caller makes against a service that is down."""
        self.calls.clear()
        with self.assertRaises(net.FetchFailed):
            call()
        self.assertEqual(len(set(self.calls)), 1, self.calls)
        return self.calls[0], len(self.calls)

    def test_the_new_signature(self):
        params = inspect.signature(net.fetch).parameters
        self.assertEqual(list(params), ["url", "options"])
        self.assertEqual(params["options"].kind, inspect.Parameter.KEYWORD_ONLY)
        o = net.FetchOptions()
        self.assertEqual((o.timeout, o.retries, o.headers), (10.0, 2, {}))
        with self.assertRaises(TypeError):
            net.fetch("u", 5)

    def test_every_caller_keeps_its_settings(self):
        self.assertEqual(self.attempts(stations.station_list), (5, 3))
        self.assertEqual(self.attempts(lambda: forecast.forecast("Oslo")), (3, 1))
        self.assertEqual(self.attempts(lambda: alerts.alerts("NW")), (1, 3))
        self.assertEqual(self.attempts(lambda: sources.from_source("noaa", "x")), (8, 3))
        self.assertEqual(self.attempts(lambda: sources.from_source("met", "x")), (10, 5))
        self.assertEqual(self.attempts(lambda: cache.cached_fetch("u", 4, 1)), (4, 2))
        self.assertEqual(self.attempts(lambda: cache.cached_fetch("v")), (10, 3))
        self.assertEqual(self.attempts(lambda: backfill.main(1)), (10, 6))

    def test_callers_still_get_the_body(self):
        net.TRANSPORT = lambda url, timeout: "[1, 2]"
        self.assertEqual(stations.station_list(), [1, 2])
        self.assertEqual(cache.cached_fetch("w"), "[1, 2]")
