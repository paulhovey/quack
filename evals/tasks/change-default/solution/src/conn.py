import socket


def connect(host, port, timeout=60):
    return socket.create_connection((host, port), timeout=timeout)


def reconnect(host, port, timeout=30):
    return connect(host, port, timeout)
