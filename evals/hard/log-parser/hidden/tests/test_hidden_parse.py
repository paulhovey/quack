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

    def test_epoch_timestamps_are_utc(self):
        (a, b) = parse_log(
            '{"ts": 1792108800, "level": "info", "msg": "a"}\n'
            '{"ts": 1792108801.25, "level": "info", "msg": "b"}'
        )
        self.assertEqual(a.ts, datetime(2026, 10, 16, 0, 0, 0))
        self.assertEqual(b.ts, datetime(2026, 10, 16, 0, 0, 1, 250000))
        self.assertIsNone(a.ts.tzinfo)

    def test_json_lines_without_a_level(self):
        (e,) = parse_log('{"ts": "2026-01-02T03:04:05", "msg": "quiet"}')
        self.assertEqual(e.level, "INFO")

    def test_other_json_keys_are_fields(self):
        e, t = parse_log(
            '{"ts": "2026-01-02T03:04:05", "level": "error", "msg": "m", "thread": "w",'
            ' "code": 503, "retry": true, "tags": ["db"]}\n'
            "2026-01-02 03:04:06 INFO plain"
        )
        self.assertEqual(e.fields, {"code": 503, "retry": True, "tags": ["db"]})
        self.assertEqual(e.thread, "w")
        self.assertEqual(t.fields, {})
