from export import to_dict
from models import Contact

c = Contact("ada", "1", email="ada@example.org")
assert to_dict(c) == {"name": "ada", "phone": "1", "email": "ada@example.org"}, to_dict(c)
assert Contact("x").email == ""
