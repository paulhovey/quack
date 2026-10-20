"""HTTP fetching with retries. `TRANSPORT` does the actual request, so tests can swap it."""

import urllib.request


def _urlopen(url, timeout):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return r.read().decode("utf-8")


TRANSPORT = _urlopen


class FetchFailed(IOError):
    pass


def fetch(url, timeout=10, retries=2):
    """The body at `url`, trying up to `retries` more times when a request fails."""
    last = None
    for _ in range(retries + 1):
        try:
            return TRANSPORT(url, timeout)
        except OSError as e:
            last = e
    raise FetchFailed(f"{url}: {last}")
