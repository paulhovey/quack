# defaults/hooks.d

Example hooks, one per file, all `enabled = false`. To use one, copy it (and the folder
its `command` names with `{dir}`, if any) to `hooks.d` in your config folder
(`quack config paths` shows where), and delete the `enabled = false` line. `/doctor`
checks a hook without running it.

A hook needs an `event` and a `command`. The command gets the event as JSON on stdin and
may answer on stdout; see docs/plan/11-security.md#hooks, or `../schemas/hook.json` for
every option.

| File | Event | What it does |
|---|---|---|
| `jira.toml` + `jira/jira.py` | `user_prompt` | Adds the Jira issues a prompt links to, fetched over the REST API (Python 3, standard library only) |
