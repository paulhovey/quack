"""The run order of a set of jobs."""


class CycleError(ValueError):
    pass


class UnknownJob(KeyError):
    pass


def plan(jobs):
    """The order to run `jobs` in. A job runs after every job in its `after`. Of the jobs
    that are ready to run, the one with the highest priority goes first; jobs with equal
    priorities go in name order."""
    by_name = {j.name: j for j in jobs}
    waiting = {}
    for j in jobs:
        for dep in j.after:
            if dep not in by_name:
                raise UnknownJob(f"{j.name} runs after {dep}, which is not a job")
        waiting[j.name] = set(j.after)
    ready = {name for name, deps in waiting.items() if not deps}
    order = []
    while ready:
        best = min(ready, key=lambda name: (-by_name[name].priority, name))
        ready.remove(best)
        del waiting[best]
        order.append(best)
        for name, deps in waiting.items():
            if best in deps:
                deps.discard(best)
                if not deps:
                    ready.add(name)
    if waiting:
        raise CycleError(f"these jobs wait on each other: {', '.join(sorted(waiting))}")
    return order
