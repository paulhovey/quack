import functools
import json

from .net import fetch

# Alerts poll every few seconds, so a slow answer is better dropped.
quick_fetch = functools.partial(fetch, timeout=1)


def alerts(region):
    return json.loads(quick_fetch(f"https://alerts.example.org/active?region={region}"))
