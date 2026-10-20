"""Everything wired together, from a project folder (config/ and data/)."""

from dataclasses import dataclass
from pathlib import Path

from .catalog import Catalog
from .config import load_settings
from .discounts import load_coupons
from .events import EventBus
from .inventory import Inventory
from .notifications import Mailer
from .shop import Shop
from .storage import JsonStore

ROOT = Path(__file__).resolve().parent.parent


@dataclass
class World:
    settings: object
    catalog: Catalog
    inventory: Inventory
    bus: EventBus
    mailer: Mailer
    shop: Shop


def load_world(root=ROOT, orders_dir=None):
    """`orders_dir`: where orders are saved; data/orders by default. None of the shop's
    state is shared between worlds, so tests can each have their own."""
    root = Path(root)
    settings = load_settings(root / "config" / "settings.json")
    catalog = Catalog.load(root / "data" / "catalog.json", settings.currency)
    coupons = load_coupons(root / "data" / "coupons.json")
    inventory = Inventory.load(root / "data" / "stock.json")
    bus = EventBus()
    inventory.subscribe(bus)
    mailer = Mailer(settings)
    mailer.subscribe(bus)
    store = JsonStore(orders_dir or root / "data" / "orders")
    shop = Shop(catalog, coupons, settings, bus, store)
    return World(settings, catalog, inventory, bus, mailer, shop)
