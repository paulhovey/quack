"""Fetch the last N days of history: `python -m scripts.backfill 7`."""

import sys

from weather.net import fetch as get


def main(days):
    # The archive is flaky at night; keep trying.
    return [get(f"https://archive.example.org/day/{d}", retries=5) for d in range(days)]


if __name__ == "__main__":
    for body in main(int(sys.argv[1])):
        print(body)
