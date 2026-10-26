"""Compare two `quack eval` setups task by task, with confidence intervals.

Usage: python3 scripts/eval-compare.py A.json[,A-2.json,...] B.json[,B-2.json,...]

Each side is one setup: one or more result files (a `--repeat` run, or reruns). Only
tasks both sides ran are compared, and each task counts once however many runs it had,
so a difference is never just one side having more tasks. The intervals come from
resampling tasks (a paired bootstrap): when one includes zero, this many tasks can't tell
the setups apart.
"""

import json
import random
import sys
from collections import Counter, defaultdict

ROUNDS = 10_000


def load(spec):
    files = spec.split(",")
    reports = [json.loads(open(f, encoding="utf-8").read()) for f in files]
    first = reports[0]
    name = f"{first['model']} · {first['profile']}"
    if first.get("planner"):
        name += f" · planned by {first['planner']}"
    name += f" · {first.get('suite', 'tasks')}"
    for r in reports[1:]:
        if (r["model"], r["profile"], r.get("planner"), r.get("suite", "tasks")) != (
            first["model"], first["profile"], first.get("planner"), first.get("suite", "tasks")
        ):
            sys.exit(f"{spec}: these files are different setups")
    runs = defaultdict(list)
    for r in reports:
        for x in r["results"]:
            if not x.get("skipped"):
                runs[x["task"]].append(x)
    return name, runs


def mean(xs):
    return sum(xs) / len(xs) if xs else 0.0


def per_task(runs, key):
    return {t: mean([float(key(x)) for x in rs]) for t, rs in runs.items()}


def interval(diffs, rng):
    """The mean of `diffs` and its 95% bootstrap interval."""
    n = len(diffs)
    means = sorted(mean([diffs[rng.randrange(n)] for _ in range(n)]) for _ in range(ROUNDS))
    return mean(diffs), means[int(0.025 * ROUNDS)], means[int(0.975 * ROUNDS) - 1]


def main(args):
    if len(args) != 2:
        sys.exit(__doc__)
    (name_a, a), (name_b, b) = load(args[0]), load(args[1])
    tasks = sorted(set(a) & set(b))
    if not tasks:
        sys.exit("the two sides have no task in common")
    only = sorted(set(a) ^ set(b))
    rng = random.Random(0)

    metrics = {
        "pass rate": lambda x: x["passed"],
        "steps": lambda x: x["steps"],
        "cost $": lambda x: x.get("cost") or 0.0,
        "prompt k": lambda x: x["prompt_tokens"] / 1000,
    }
    # Passes with no process label (see `Process` in crates/quack-exec/src/eval.rs), when
    # every run on both sides was graded.
    graded = all("process" in x for side in (a, b) for t in tasks for x in side[t])
    if graded:
        metrics["clean pass"] = lambda x: x["passed"] and not x["process"]
    va = {m: per_task(a, f) for m, f in metrics.items()}
    vb = {m: per_task(b, f) for m, f in metrics.items()}

    print(f"A: {name_a} ({sum(len(a[t]) for t in tasks)} runs)")
    print(f"B: {name_b} ({sum(len(b[t]) for t in tasks)} runs)")
    print(f"{len(tasks)} tasks in common" + (f"; left out: {', '.join(only)}" if only else ""))
    print()
    print("| task | A pass | B pass | A steps | B steps | A cost | B cost | A failures | B failures |")
    print("|---|---|---|---|---|---|---|---|---|")

    def labels(rs):
        c = Counter(x.get("failure") or "fail" for x in rs if not x["passed"])
        return ", ".join(f"{k} {v}" if v > 1 else k for k, v in sorted(c.items())) or "-"

    for t in tasks:
        pa = sum(x["passed"] for x in a[t])
        pb = sum(x["passed"] for x in b[t])
        print(
            f"| {t} | {pa}/{len(a[t])} | {pb}/{len(b[t])} "
            f"| {va['steps'][t]:.1f} | {vb['steps'][t]:.1f} "
            f"| {va['cost $'][t]:.4f} | {vb['cost $'][t]:.4f} | {labels(a[t])} | {labels(b[t])} |"
        )
    print()
    print("B - A, per task (95% interval):")
    for m in metrics:
        diffs = [vb[m][t] - va[m][t] for t in tasks]
        d, lo, hi = interval(diffs, rng)
        rate = m in ("pass rate", "clean pass")
        scale, unit = (100, " pts") if rate else (1, "")
        p = 1 if rate else {"cost $": 4}.get(m, 2)
        clear = "clear" if lo > 0 or hi < 0 else "not clear"
        print(
            f"  {m:<10} A {mean(list(va[m].values())) * scale:9.{p}f}  "
            f"B {mean(list(vb[m].values())) * scale:9.{p}f}  "
            f"diff {d * scale:+9.{p}f}{unit} ({lo * scale:+.{p}f} .. {hi * scale:+.{p}f}): {clear}"
        )
    print()
    for label, side in (("A", a), ("B", b)):
        c = Counter(x.get("failure") or "unlabelled" for t in tasks for x in side[t]
                    if not x["passed"])
        print(f"{label} failures: " + (", ".join(f"{k} {v}" for k, v in c.most_common()) or "none"))
    if graded:
        for label, side in (("A", a), ("B", b)):
            c = Counter(p for t in tasks for x in side[t] for p in x["process"])
            print(f"{label} process: " + (", ".join(f"{k} {v}" for k, v in c.most_common()) or "none"))


if __name__ == "__main__":
    main(sys.argv[1:])
