"""A fresh world per test: the project's config and data, orders saved to a temp folder."""

import tempfile
from datetime import date

from orderly.world import load_world

FRIDAY = date(2026, 10, 16)


def fresh_world(test):
    tmp = tempfile.TemporaryDirectory()
    test.addCleanup(tmp.cleanup)
    return load_world(orders_dir=tmp.name)
