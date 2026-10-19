from calc import divide

assert divide(6, 3) == 2
try:
    divide(1, 0)
except ValueError as e:
    assert str(e) == "division by zero", e
else:
    raise SystemExit("no ValueError")
