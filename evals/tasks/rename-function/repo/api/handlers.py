from users.store import get_usr


def show(user_id):
    return {"name": get_usr(user_id)}
