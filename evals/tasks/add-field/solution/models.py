from dataclasses import dataclass


@dataclass
class Contact:
    name: str
    phone: str = ""
    email: str = ""
