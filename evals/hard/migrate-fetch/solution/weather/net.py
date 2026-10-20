"""HTTP fetching with retries. `TRANSPORT` does the actual request, so tests can swap it."""

import urllib.request
from dataclasses import dataclass, field


def _urlopen(url, timeout):
    with urllib.request.urlopen(url, timeout=timeout) as r:
        return r.read().decode("utf-8")


TRANSPORT = _urlopen


class FetchFailed(IOError):
    pass


@dataclass(frozen=True)
class FetchOptions:
    timeout: float = 10.0
    retries: int = 2
    headers: dict = field(default_factory=dict)


def fetch(url, *, options=None):
    """The body at `url`, trying up to `options.retries` more times when a request fails."""
    options = options or FetchOptions()
    last = None
    for _ in range(options.retries + 1):
        try:
            return TRANSPORT(url, options.timeout)
        except OSError as e:
            last = e
    raise FetchFailed(f"{url}: {last}")
