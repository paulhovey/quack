import json

from . import net

BASE = "https://forecast.example.org"


def forecast(city):
    # Forecasts are for people waiting on a page: fail fast, don't retry.
    return json.loads(net.fetch(
        f"{BASE}/daily?city={city}", options=net.FetchOptions(timeout=3, retries=0)
    ))
