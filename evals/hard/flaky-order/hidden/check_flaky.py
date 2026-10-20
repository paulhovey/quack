"""The plan must not depend on the hash seed, and ties must go in name order."""

import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from scheduler.jobs import Job  # noqa: E402
from scheduler.plan import plan  # noqa: E402

for seed in range(20):
    env = dict(os.environ, PYTHONHASHSEED=str(seed))
    r = subprocess.run(
        [sys.executable, "-m", "unittest", "-q", "tests.test_plan"],
        env=env, capture_output=True, text=True,
    )
    if r.returncode != 0:
        sys.exit(f"tests.test_plan fails with PYTHONHASHSEED={seed}:\n{r.stderr[-800:]}")

jobs = [Job(n, 1) for n in ("zeta", "alpha", "mu", "beta", "omega")] + [Job("first", 2)]
assert plan(jobs) == ["first", "alpha", "beta", "mu", "omega", "zeta"], plan(jobs)
jobs = [Job("root"), Job("y", 0, ("root",)), Job("x", 0, ("root",)), Job("w", 4, ("y",))]
assert plan(jobs) == ["root", "x", "y", "w"], plan(jobs)
print("ok")
