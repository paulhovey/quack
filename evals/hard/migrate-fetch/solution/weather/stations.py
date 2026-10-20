import json

from .net import FetchOptions, fetch

BASE = "https://stations.example.org"


def station_list():
    return json.loads(fetch(f"{BASE}/v2/stations", options=FetchOptions(timeout=5)))
