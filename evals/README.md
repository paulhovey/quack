# evals

The task suite for `quack eval` (docs/plan/13-testing-and-evals.md#quack-eval).

```sh
quack eval -m <provider>/<model>                     # every task, the model's own profile
quack eval -m <provider>/<model> --profile qwen-code # the same tasks with another profile
quack eval --tasks fix-parser,needle --repeat 3      # a few tasks, three runs each
quack eval --suite evals/hard --repeat 3             # the hard suite (hard/README.md)
```

There are two suites. `tasks/` is quick (2–6 steps a task) and every model tested so far
passes it, so it catches regressions and compares cost. `hard/` is meant to take 10–30
steps a task and to be failed by some models, so it can tell setups apart.

Each run gets a fresh copy of the task's `repo/` and its own data directory, so sessions
and memory stay out of yours. Runs are unattended: the permission mode is `full-auto`, and
anything that still asks is denied. Your providers, settings and AGENTS.md apply, since
they are the setup being measured. **Every task calls the model, so a run costs what that
model costs.**

The table goes to stdout and the JSON to `evals/results/<date>/<model>-<profile>.json`
(`--out` to change); a rerun writes `<model>-<profile>-2.json` and so on rather than
overwriting, and another suite's results start with its name
(`hard-<model>-<profile>.json`). It records the sampling settings the requests carried
(empty: the server's defaults) and, for each task, pass/fail, model requests, tool errors, argument parse
failures, failed edits, tokens (prompt, cached, completion), time, the files the model
changed, and for a failed run a `failure` label that says why:

| label | meaning |
|---|---|
| `timeout`, `error` | the time ran out; the provider or engine failed |
| `bad-calls` | half or more of its tool calls didn't parse |
| `edit-failed` | edits failed, and the solution's files never all changed |
| `no-change` | the task needed changes and it made none |
| `wrong-files` / `missed-files` | none / only some of the files the solution changes |
| `wrong-answer` | only the checks on its reply failed |
| `untested` | the right files changed, it never ran a command, and the result is wrong |
| `wrong-fix` | the right files changed and the result is still wrong |

`--keep` keeps each run's workspace and session log (`data/sessions/`) and prints where.

## Comparing two setups

```sh
python3 scripts/eval-table.py evals/results/<date>     # one row per setup, with failure labels
python3 scripts/eval-compare.py A.json,A-2.json B.json # two setups, task by task
```

`eval-compare.py` counts each task once however many runs it had, and gives each
difference (pass rate, steps, cost, prompt tokens) a 95% interval from resampling the
tasks. Only a difference whose interval leaves out zero is "clear"; one run of 14 tasks
rarely gets there, which is why `--repeat 3` matters.

## A task

```
tasks/<id>/
├── task.toml     # the prompt and the checks
├── repo/         # the files the model starts with
├── hidden/       # optional: files copied in after the model is done, for the checks
└── solution/     # a reference solution: files to put in place, ANSWER, DELETE
```

```toml
category = "fix-test"        # navigate, single-edit, multi-file, fix-test, shell, create, long-context
prompt = "The tests in test_window.py fail. Fix the bug in window.py."
requires = ["python"]        # skipped unless on PATH; `python` is python3 or python
timeout_secs = 600           # the default
base = "../_projects/orderly" # optional: a project copied in first; repo/ then holds
                              # only the files this task changes (it may be left out)

# Every check must hold. Each sets one condition.
[[check]]
command = "{python} -m unittest -q test_window"   # exits 0; sh on Unix, cmd on Windows
[[check]]
file = "window.py"           # with contains / not_contains / equals
contains = "def last_n"
[[check]]
answer_contains = "42"       # the final reply, case-insensitive
[[check]]
tool_called = "shell"        # an internal tool name
[[check]]
exists = "new.py"            # or missing = "old.py"
```

`cargo test -p quack-exec --test suite` checks every task in both suites: its checks must
fail on the files as given and pass once `solution/` is applied (with `hidden/` in place
both times).

The solution also tells failures apart: a run that changed none of the files
`solution/` changes is `wrong-files`, so keep a solution to the files a fix needs.
