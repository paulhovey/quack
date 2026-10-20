"""A small synchronous event bus. Handlers run in the order they subscribed."""

from collections import defaultdict
from dataclasses import dataclass, field


@dataclass(frozen=True)
class Event:
    name: str
    data: dict = field(default_factory=dict)


class EventBus:
    def __init__(self):
        self._handlers = defaultdict(list)
        # Every event emitted, oldest first; handy in tests.
        self.log = []

    def subscribe(self, name, handler):
        self._handlers[name].append(handler)

    def emit(self, name, **data):
        event = Event(name, data)
        self.log.append(event)
        for handler in list(self._handlers[name]):
            handler(event)
        return event

    def names(self):
        return [e.name for e in self.log]
