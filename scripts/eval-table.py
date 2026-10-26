"""Compare `quack eval` results: one row per suite, model, profile and planner, summed over
the tasks, with each failure's label (see `Failure` in crates/quack-exec/src/eval.rs).
`clean` counts the passes with no process label (`Process`: unverified, blind-retry,
regression), `?` for results written before those labels existed.

Usage: python3 scripts/eval-table.py evals/results/<date> [more dirs or files...]
When one ran more than once, the newest file (`-2`, `-3`, ...) wins.
"""

import json
import re
import sys
from collections import Counter
from pathlib import Path


def newest_first(path):
    m = re.search(r"-(\d+)\.json$", path.name)
    return int(m.group(1)) if m else 1


def main(args):
    files = []
    for a in args or ["evals/results"]:
        p = Path(a)
        files += sorted(p.rglob("*.json")) if p.is_dir() else [p]
    runs = {}
    for f in sorted(files, key=lambda f: (str(f.parent), newest_first(f))):
        r = json.loads(f.read_text())
        suite = r.get("suite", "tasks")
        if r.get("window"):
            suite += f"@{r['window'] // 1024}k"
        runs[(suite, r["model"], r["profile"], r.get("planner") or "-")] = r
    print("| suite | model | profile | planner | pass | clean | steps | tool err | parse | edit fail "
          "| prompt k (cached) | out k | peak k | compact / pruned | cost | time | failures "
          "| process |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    total = 0.0
    for (suite, model, profile, planner), r in sorted(runs.items()):
        rs = [x for x in r["results"] if not x.get("skipped")]

        def s(key):
            return sum(x.get(key) or 0 for x in rs)

        cost = s("cost")
        total += cost
        cached = 100 * s("cached_tokens") / max(1, s("prompt_tokens"))
        failures = Counter(x.get("failure") or "?" for x in rs if not x["passed"])
        labels = ", ".join(f"{k} {v}" for k, v in failures.most_common()) or "-"
        graded = all("process" in x for x in rs)
        clean = sum(x["passed"] and not x["process"] for x in rs) if graded else "?"
        habits = Counter(p for x in rs for p in x.get("process", []))
        process = ", ".join(f"{k} {v}" for k, v in habits.most_common()) or ("-" if graded else "?")
        print(f"| {suite} | {model} | {profile} | {planner} | {sum(x['passed'] for x in rs)}/{len(rs)} "
              f"| {clean} "
              f"| {s('steps')} | {s('tool_errors')} | {s('parse_failures')} "
              f"| {s('edit_failures')} | {s('prompt_tokens') / 1000:.0f} ({cached:.0f}%) "
              f"| {s('completion_tokens') / 1000:.1f} "
              f"| {max((x.get('peak_prompt') or 0 for x in rs), default=0) / 1000:.0f} "
              f"| {s('compactions')} / {s('pruned')} | ${cost:.3f} | {s('seconds'):.0f}s "
              f"| {labels} | {process} |")
    print(f"\ntotal ${total:.2f}")
    for (suite, model, profile, planner), r in sorted(runs.items()):
        for x in r["results"]:
            if not x.get("passed") and not x.get("skipped"):
                why = x.get("error") or "; ".join(x.get("failed_checks", []))
                label = x.get("failure") or "?"
                print(f"FAIL {suite} {model} {profile} {planner} {x['task']} ({label}): {why[:200]}")


if __name__ == "__main__":
    main(sys.argv[1:])
