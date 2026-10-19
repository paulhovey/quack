import handlers

for n in range(1, 1201):
    want = "archived" if n == 742 else "active"
    got = getattr(handlers, f"handle_event_{n}")(None)
    assert got == want, f"handle_event_{n} returns {got!r}, not {want!r}"
