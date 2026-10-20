"""An in-memory cache in front of `net.fetch`."""

from .net import FetchOptions, fetch

_cache = {}


def cached_fetch(url, timeout=10, retries=2):
    if url not in _cache:
        _cache[url] = fetch(url, options=FetchOptions(timeout=timeout, retries=retries))
    return _cache[url]


def clear():
    _cache.clear()
