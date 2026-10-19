import sys

from api.handlers import show
from users.store import get_user

if __name__ == "__main__":
    uid = int(sys.argv[1])
    assert show(uid)["name"] == get_user(uid)
    print(show(uid)["name"])
