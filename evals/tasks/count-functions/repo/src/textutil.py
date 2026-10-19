"""Small text helpers."""


def words(s):
    return s.split()


def lines(s):
    return s.splitlines()


class Counter:
    def count(self, s):
        return len(words(s))

    def reset(self):
        pass


def shout(s):
    return s.upper()


def whisper(s):
    return s.lower()


def title(s):
    return s.title()
