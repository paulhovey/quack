def mean(values):
    total = 0
    for v in values:
        total += v
    return total / len(values)


def spread(values):
    return max(values) - min(values)
