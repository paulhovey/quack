from users.store import get_user


def show(user_id):
    return {"name": get_user(user_id)}
