import unittest
from datetime import datetime
from pathlib import Path

from logsum.parse import ParseError, parse_log
from logsum.summary import summarize

SAMPLE = (Path(__file__).resolve().parent.parent / "sample.log").read_text()


class ParseTest(unittest.TestCase):
    def test_a_text_line(self):
        (e,) = parse_log("2026-10-16 12:00:01,123 INFO [main] starting up")
        self.assertEqual(e.ts, datetime(2026, 10, 16, 12, 0, 1, 123000))
        self.assertEqual((e.level, e.thread, e.message), ("INFO", "main", "starting up"))

    def test_a_json_line(self):
        (e,) = parse_log('{"ts": "2026-10-16T12:00:03", "level": "info", "msg": "hi", "thread": "w"}')
        self.assertEqual((e.level, e.thread, e.message), ("INFO", "w", "hi"))

    def test_level_aliases(self):
        levels = [e.level for e in parse_log(
            "2026-10-16 12:00:00 WARN x\n2026-10-16 12:00:00 err y\n2026-10-16 12:00:00 Fatal z"
        )]
        self.assertEqual(levels, ["WARNING", "ERROR", "CRITICAL"])

    def test_tracebacks_belong_to_their_entry(self):
        entries = parse_log(SAMPLE)
        self.assertEqual(len(entries), 5)
        self.assertEqual(len(entries[2].extra), 4)
        self.assertTrue(entries[2].extra[0].startswith("Traceback"))

    def test_bad_lines(self):
        with self.assertRaises(ParseError) as cm:
            parse_log("2026-10-16 12:00:00 INFO ok\nnonsense here")
        self.assertEqual(cm.exception.lineno, 2)

    def test_summary(self):
        s = summarize(SAMPLE)
        self.assertEqual(s["levels"], {"ERROR": 2, "INFO": 2, "WARNING": 1})
        self.assertEqual(s["threads"], ["main", "worker-1", "worker-3"])
        self.assertEqual(s["errors"], ["could not reach the database", "gave up after 3 tries"])
