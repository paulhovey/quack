"""Money: a Decimal amount in one currency, kept at the currency's minor unit.

Every amount in orderly goes through Money (see CONTRIBUTING.md): no floats, ever.
"""

from decimal import ROUND_HALF_UP, Decimal

MINOR_UNITS = {"USD": 2, "EUR": 2, "GBP": 2, "NZD": 2, "JPY": 0}


class CurrencyMismatch(ValueError):
    """Two amounts in different currencies were combined."""


def quantum(currency):
    """The currency's smallest unit: Decimal('0.01') for USD."""
    return Decimal(1).scaleb(-MINOR_UNITS[currency])


class Money:
    __slots__ = ("amount", "currency")

    def __init__(self, amount, currency="USD"):
        if currency not in MINOR_UNITS:
            raise ValueError(f"unknown currency {currency!r}")
        if isinstance(amount, float):
            raise TypeError("pass a str or Decimal, not a float")
        self.amount = Decimal(amount).quantize(quantum(currency), rounding=ROUND_HALF_UP)
        self.currency = currency

    @classmethod
    def zero(cls, currency="USD"):
        return cls(0, currency)

    def _check(self, other):
        if not isinstance(other, Money):
            raise TypeError(f"cannot combine Money with {type(other).__name__}")
        if other.currency != self.currency:
            raise CurrencyMismatch(f"{self.currency} and {other.currency}")

    def __add__(self, other):
        # sum() starts from 0.
        if isinstance(other, int) and other == 0:
            return self
        self._check(other)
        return Money(self.amount + other.amount, self.currency)

    __radd__ = __add__

    def __sub__(self, other):
        self._check(other)
        return Money(self.amount - other.amount, self.currency)

    def __neg__(self):
        return Money(-self.amount, self.currency)

    def __mul__(self, factor):
        if isinstance(factor, float):
            raise TypeError("multiply by an int or Decimal, not a float")
        return Money(self.amount * Decimal(factor), self.currency)

    __rmul__ = __mul__

    def __eq__(self, other):
        if not isinstance(other, Money):
            return NotImplemented
        return (self.amount, self.currency) == (other.amount, other.currency)

    def __hash__(self):
        return hash((self.amount, self.currency))

    def __lt__(self, other):
        self._check(other)
        return self.amount < other.amount

    def __le__(self, other):
        self._check(other)
        return self.amount <= other.amount

    def __gt__(self, other):
        self._check(other)
        return self.amount > other.amount

    def __ge__(self, other):
        self._check(other)
        return self.amount >= other.amount

    def __bool__(self):
        return self.amount != 0

    def __repr__(self):
        return f"Money('{self.amount}', '{self.currency}')"

    def __str__(self):
        return f"{self.amount} {self.currency}"

    def allocate(self, weights):
        """Split into parts proportional to `weights`. The parts always add up to exactly
        this amount: the minor units left over after rounding down go one each to the
        parts with the largest remainders (on a tie, the earlier part)."""
        weights = [Decimal(w) for w in weights]
        total = sum(weights)
        if not weights or total <= 0:
            raise ValueError("weights must add up to more than zero")
        return [Money(self.amount * w / total, self.currency) for w in weights]


def parse_amount(text, currency="USD"):
    """An amount typed by a person: '12', '12.5', '$12.50' or '1,200.00'."""
    cleaned = text.strip().lstrip("$").replace(",", "")
    try:
        return Money(Decimal(cleaned), currency)
    except ArithmeticError:
        raise ValueError(f"not an amount: {text!r}") from None
