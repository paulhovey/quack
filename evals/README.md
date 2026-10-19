# evals

The task suite for `quack eval` (docs/plan/13-testing-and-evals.md#quack-eval).

```sh
quack eval -m <provider>/<model>                     # every task, the model's own profile
quack eval -m <provider>/<model> --profile qwen-code # the same tasks with another profile
quack eval --tasks fix-parser,needle --repeat 3      # a few tasks, three runs each
```

Each run gets a fresh copy of the task's `repo/` and its own data directory, so sessions
and memory stay out of yours. Runs are unattended: the permission mode is `full-auto`, and
anything that still asks is denied. Your providers, settings and AGENTS.md apply, since
they are the setup being measured. **Every task calls the model, so a run costs what that
model costs.**

The table goes to stdout and the JSON to `evals/results/<date>/<model>-<profile>.json`
(`--out` to change); a rerun writes `<model>-<profile>-2.json` and so on rather than
overwriting. It records the sampling settings the requests carried (empty: the server's
defaults) and, for each task, pass/fail, model requests, tool errors, argument parse
failures, failed edits, tokens (prompt, cached, completion) and time. `--keep` keeps each
run's workspace and session log (`data/sessions/`) and prints where.

## A task

```
tasks/<id>/
├── task.toml     # the prompt and the checks
├── repo/         # the files the model starts with
└── solution/     # a reference solution: files to put in place, ANSWER, DELETE
```

```toml
category = "fix-test"        # navigate, single-edit, multi-file, fix-test, shell, create, long-context
prompt = "The tests in test_window.py fail. Fix the bug in window.py."
requires = ["python"]        # skipped unless on PATH; `python` is python3 or python
timeout_secs = 600           # the default

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

`cargo test -p quack-exec --test suite` checks every task: its checks must fail on `repo/`
as given and pass once `solution/` is applied.
