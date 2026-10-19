from models import Contact


def to_dict(c: Contact) -> dict:
    return {"name": c.name, "phone": c.phone}
