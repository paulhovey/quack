"""Orders on disk: one JSON file per order in a folder (data/orders by default)."""

import json
from pathlib import Path

from .serialize import order_from_dict, order_to_dict


class JsonStore:
    def __init__(self, root):
        self.root = Path(root)
        self.root.mkdir(parents=True, exist_ok=True)

    def _path(self, order_id):
        return self.root / f"{order_id}.json"

    def save(self, order):
        text = json.dumps(order_to_dict(order), indent=2, sort_keys=True)
        tmp = self._path(order.id).with_suffix(".tmp")
        tmp.write_text(text + "\n", encoding="utf-8")
        tmp.replace(self._path(order.id))

    def exists(self, order_id):
        return self._path(order_id).is_file()

    def load(self, order_id, shop):
        data = json.loads(self._path(order_id).read_text(encoding="utf-8"))
        return order_from_dict(data, shop)

    def ids(self):
        return sorted((p.stem for p in self.root.glob("*.json")), key=int)

    def load_all(self, shop):
        return [self.load(i, shop) for i in self.ids()]

    def next_id(self):
        ids = self.ids()
        return int(ids[-1]) + 1 if ids else 1001
