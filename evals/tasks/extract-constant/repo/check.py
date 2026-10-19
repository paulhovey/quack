from shop.cart import total
from shop.checkout import receipt

assert total([10]) == 12, total([10])
assert receipt(10).endswith("12.00"), receipt(10)
