from net.settings import MAX_RETRIES, TIMEOUT


def fetch(url):
    for attempt in range(MAX_RETRIES):
        print("try", attempt, url, TIMEOUT)
