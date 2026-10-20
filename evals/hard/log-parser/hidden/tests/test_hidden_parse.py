import unittest
from datetime import datetime

from logsum.parse import ParseError, parse_log


class MoreParseTest(unittest.TestCase):
    def test_timestamps(self):
        stamps = [e.ts for e in parse_log(
            "2026-01-02T03:04:05,5 INFO a\n2026-01-02 03:04:05.25 INFO b\n2026-01-02T03:04:05 INFO c"
        )]
        self.assertEqual(stamps, [
            datetime(2026, 1, 2, 3, 4, 5, 500000),
            datetime(2026, 1, 2, 3, 4, 5, 250000),
            datetime(2026, 1, 2, 3, 4, 5),
        ])

    def test_json_aliases_and_threads(self):
        (e,) = parse_log('{"ts": "2026-01-02 03:04:05,1", "level": "Warn", "msg": "m", "thread": "t9"}')
        self.assertEqual((e.level, e.thread, e.ts.microsecond), ("WARNING", "t9", 100000))

    def test_continuations(self):
        entries = parse_log(
            "2026-01-02 03:04:05 ERROR boom\n\tdetail one\n    detail two\n\n"
            "Traceback (most recent call last):\n  x\n2026-01-02 03:04:06 INFO next"
        )
        self.assertEqual(len(entries), 2)
        self.assertEqual(entries[0].extra, ["\tdetail one", "    detail two",
                                            "Traceback (most recent call last):", "  x"])
        self.assertIsNone(entries[1].thread)

    def test_a_continuation_with_nothing_before_it(self):
        with self.assertRaises(ParseError) as cm:
            parse_log("\n  orphan")
        self.assertEqual(cm.exception.lineno, 2)

    def test_unknown_levels_are_errors(self):
        with self.assertRaises(ParseError):
            parse_log("2026-01-02 03:04:05 LOUD hello")
