def mean(values):
    tmp = 0
    for v in values:
        tmp += v
    return tmp / len(values)


def spread(values):
    return max(values) - min(values)
