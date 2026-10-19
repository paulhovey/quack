def last_n(items, n):
    """The last `n` items, oldest first."""
    if n <= 0:
        return []
    return items[max(len(items) - n, 0) :]
