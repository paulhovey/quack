"""Compare `quack eval` results: one row per model and profile, summed over the tasks.

Usage: python3 scripts/eval-table.py evals/results/<date> [more dirs or files...]
When a model and profile ran more than once, the newest file (`-2`, `-3`, ...) wins.
"""

import json
import re
import sys
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
        runs[(r["model"], r["profile"])] = r
    print("| model | profile | pass | steps | tool err | parse | edit fail "
          "| prompt k (cached) | out k | cost | time |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    total = 0.0
    for (model, profile), r in sorted(runs.items()):
        rs = [x for x in r["results"] if not x.get("skipped")]

        def s(key):
            return sum(x.get(key) or 0 for x in rs)

        cost = s("cost")
        total += cost
        cached = 100 * s("cached_tokens") / max(1, s("prompt_tokens"))
        print(f"| {model} | {profile} | {sum(x['passed'] for x in rs)}/{len(rs)} "
              f"| {s('steps')} | {s('tool_errors')} | {s('parse_failures')} "
              f"| {s('edit_failures')} | {s('prompt_tokens') / 1000:.0f} ({cached:.0f}%) "
              f"| {s('completion_tokens') / 1000:.1f} | ${cost:.3f} | {s('seconds'):.0f}s |")
    print(f"\ntotal ${total:.2f}")
    for (model, profile), r in sorted(runs.items()):
        for x in r["results"]:
            if not x.get("passed") and not x.get("skipped"):
                why = x.get("error") or "; ".join(x.get("failed_checks", []))
                print(f"FAIL {model} {profile} {x['task']}: {why[:200]}")


if __name__ == "__main__":
    main(sys.argv[1:])
