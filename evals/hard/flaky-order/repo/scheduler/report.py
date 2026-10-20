"""A plan as text, for the CI log."""

from .plan import plan


def describe(jobs):
    by_name = {j.name: j for j in jobs}
    lines = []
    for step, name in enumerate(plan(jobs), 1):
        job = by_name[name]
        after = f" (after {', '.join(job.after)})" if job.after else ""
        lines.append(f"{step:>2}. {name} [p{job.priority}]{after}")
    return "\n".join(lines)
