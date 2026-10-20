import json

from .net import fetch

BASE = "https://stations.example.org"


def station_list():
    return json.loads(fetch(f"{BASE}/v2/stations", 5))
