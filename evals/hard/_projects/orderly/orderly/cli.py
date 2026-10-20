"""`python -m orderly <command>`: reports and exports over the saved orders."""

import argparse
import sys

from . import reports
from .invoices import render_invoice
from .util.dates import parse_date
from .world import load_world


def main(argv=None, out=None):
    out = out or sys.stdout
    parser = argparse.ArgumentParser(prog="orderly")
    parser.add_argument("--orders", help="the folder of saved orders (default: data/orders)")
    sub = parser.add_subparsers(dest="command", required=True)
    rep = sub.add_parser("report", help="a day's sales")
    rep.add_argument("--day", required=True, type=parse_date)
    rep.add_argument("--by-category", action="store_true")
    exp = sub.add_parser("export", help="every order as CSV")
    exp.add_argument("--out", help="write here instead of stdout")
    inv = sub.add_parser("invoice", help="print one order's invoice")
    inv.add_argument("order_id")
    sub.add_parser("stock", help="stock on hand and reserved")
    args = parser.parse_args(argv)

    world = load_world(orders_dir=args.orders)
    shop = world.shop
    orders = shop.store.load_all(shop)

    if args.command == "report":
        if args.by_category:
            for category, net in reports.by_category(orders, args.day).items():
                print(f"{category:<14}{net.amount:>12}", file=out)
        else:
            print(reports.format_summary(reports.daily_summary(orders, args.day)), file=out)
    elif args.command == "export":
        text = reports.to_csv(orders, world.settings.report_columns)
        if args.out:
            with open(args.out, "w", encoding="utf-8", newline="") as f:
                f.write(text)
        else:
            out.write(text)
    elif args.command == "invoice":
        out.write(render_invoice(shop.get(args.order_id)))
    elif args.command == "stock":
        inv = world.inventory
        for sku in sorted(inv.on_hand):
            print(f"{sku:<10} {inv.on_hand[sku]:>4} on hand {inv.reserved[sku]:>4} reserved",
                  file=out)
    return 0
