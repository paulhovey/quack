"""The product catalog, from data/catalog.json."""

import json
from dataclasses import dataclass
from pathlib import Path

from .money import Money


class UnknownSku(KeyError):
    pass


@dataclass(frozen=True)
class Product:
    sku: str
    name: str
    category: str
    price: Money
    # Food and books are untaxed in every region orderly sells to.
    taxable: bool = True


class Catalog:
    def __init__(self, products):
        self._by_sku = {p.sku: p for p in products}

    @classmethod
    def load(cls, path, currency="USD"):
        data = json.loads(Path(path).read_text(encoding="utf-8"))
        return cls(
            Product(
                sku=row["sku"],
                name=row["name"],
                category=row["category"],
                price=Money(row["price"], currency),
                taxable=row.get("taxable", True),
            )
            for row in data["products"]
        )

    def get(self, sku):
        try:
            return self._by_sku[sku]
        except KeyError:
            raise UnknownSku(sku) from None

    def search(self, text):
        text = text.lower()
        return [p for p in self if text in p.name.lower() or text in p.sku.lower()]

    def categories(self):
        return sorted({p.category for p in self})

    def __iter__(self):
        return iter(sorted(self._by_sku.values(), key=lambda p: p.sku))

    def __len__(self):
        return len(self._by_sku)
