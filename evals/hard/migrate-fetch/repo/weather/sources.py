"""Raw observation feeds. Each source names the `net` function to call and its arguments."""

from . import net

SOURCES = {
    "noaa": ("https://noaa.example.gov/obs/", "fetch", {"timeout": 8}),
    "met": ("https://met.example.uk/feeds/", "fetch", {"retries": 4}),
}


def from_source(name, path):
    base, func, kwargs = SOURCES[name]
    return getattr(net, func)(base + path, **kwargs)
