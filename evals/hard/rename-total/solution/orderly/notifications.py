"""Customer emails, sent on shop events. The templates are in config/settings.json and
are `str.format` strings: `{order.id}`, `{order.total_due}`, `{estimate:%A %d %B}`, ...

Nothing is really sent: emails go to `Mailer.outbox`, which a delivery job drains.
"""

from dataclasses import dataclass

from .util.dates import add_business_days


@dataclass(frozen=True)
class Email:
    to: str
    kind: str
    subject: str
    body: str


class Mailer:
    def __init__(self, settings):
        self.settings = settings
        self.outbox = []

    def subscribe(self, bus):
        bus.subscribe("order_placed", lambda e: self._send("confirmation", e))
        bus.subscribe("order_shipped", self._on_shipped)
        bus.subscribe("order_refunded", lambda e: self._send("refund", e))
        bus.subscribe("order_cancelled", lambda e: self._send("cancelled", e))

    def _on_shipped(self, event):
        order = event.data["order"]
        estimate = add_business_days(
            event.data["day"], order.region.delivery_days, self.settings.holidays
        )
        self._send("shipped", event, estimate=estimate)

    def _send(self, kind, event, **extra):
        template = self.settings.templates[kind]
        fields = dict(event.data, **extra)
        order = fields["order"]
        self.outbox.append(
            Email(
                to=order.customer,
                kind=kind,
                subject=template["subject"].format(**fields),
                body=template["body"].format(**fields),
            )
        )

    def sent(self, kind=None):
        return [m for m in self.outbox if kind is None or m.kind == kind]
