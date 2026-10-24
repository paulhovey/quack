# The hard suite

Tasks meant to take a capable model 10–30 steps and to be failed by some models, so that
profiles, planners and models can be told apart (the `tasks/` suite is passed by all of
them). Run it with `quack eval --suite evals/hard --repeat 3`. **It calls the model for
every task, and these tasks are long: expect a run to cost several times a `tasks/` run.**

Every task has `hidden/` tests that are copied in only after the model is done, so
passing the tests it can see is not enough.

## orderly

`_projects/orderly` is a small shop backend (orders, coupons, tax by region, stock,
events, emails, saved orders, reports, a CLI), about 1,200 lines with its own tests,
README and CONTRIBUTING. Each task below names it as its `base` and puts only the files
it changes in `repo/`, so every task starts from the same codebase with one problem.

| task | kind | what makes it hard |
|---|---|---|
| `category-cents` | symptom far from cause | reported in a report; the cause is rounding in `Money.allocate` |
| `cancel-restock` | symptom far from cause, two causes | stock and emails react to events; the paid path skips one, and the stock handler ignores paid orders |
| `delivery-dates` | two causes | an off-by-one in business days, and holidays loaded as strings |
| `csv-export` | symptom, two causes | hand-joined CSV, and every value is cut to a screen's column width |
| `exclusive-coupons` | feature, 2 files | a combination rule with edge cases |
| `tax-included` | feature, 5 files | config, totals, invoices and the daily report must agree; the invoice and report add up totals of their own |
| `partial-shipment` | feature, 6 files | a new state, a new event field, stock, the file format, open orders, and each email naming its parcel |
| `rename-total` | refactor | the old name lives in templates and config too, but a file key must stay |
| `quote-total` | question | three answers: the tax rides on each line's share of four coupons, and delivery skips a holiday; run the code |
| `price-change` | symptom far from cause, two sites | saved orders are re-priced from today's catalog; the BOGO coupon prices from the catalog too |

## Standalone

| task | needs | what makes it hard |
|---|---|---|
| `flaky-order` | python | two tests fail depending on `PYTHONHASHSEED`, from two sets in two modules; sorting one of them breaks the log |
| `migrate-fetch` | python | callers behind `functools.partial`, a dispatch table and an alias |
| `log-parser` | python | five bugs behind the visible tests, three more docstring rules only the hidden tests check |
| `rust-calc` | cargo | precedence and associativity in a Pratt parser, plus `%` and postfix `!` from the spec |
| `js-lru` | node | an LRU cache with a time to live, built from a spec |

## Changing orderly

Tasks copy files from it into `solution/` (and buggy versions into `repo/`), so a change
to `_projects/orderly` can make a task's solution stale. After any change, run
`cargo test -p quack-exec --test suite`.

## Small-window pass

The same suite with the window held to 16k, so pruning and compaction get exercised (at a
model's full window, almost nothing here fills the context):

```sh
quack eval --suite evals/hard -c context.window=16384 -m <provider/model> --profile generic
EVAL_ARGS="--suite evals/hard -c context.window=16384" scripts/eval-matrix.sh <model>:generic
```

Results carry `"window": 16384` and go to `hard-<model>-<profile>-16k.json`, so they never
overwrite a full-window run; `scripts/eval-table.py` shows the suite as `hard@16k`. Read the
`peak k` and `compact / pruned` columns next to steps: compare a 16k run only with another
16k run. At 32k (2026-10-23) only `partial-shipment` passed the compaction point, which is
why the pass uses 16k (compaction near 11k; the tasks peak at 8–22k at the full window).

## Calibration

A task every model passes every time, or fails every time, says nothing about a setup;
make it harder or replace it.

- **2026-10-21 runs** (`--repeat 3`, `generic`): `deepseek-v4.1-flash` passed 42/42 at 5–16
  steps a task, and Mistral Large 4 passed 14/14; only `glm-5.3-flash` split, on
  `category-cents`, `delivery-dates`, `js-lru` and `migrate-fetch`. A pass cost $0.35–0.55.
- **2026-10-23 rework**: the eight tasks both models passed every time got a second cause,
  extra spec rules checked only by hidden tests, or a likely partial fix that the hidden
  tests reject; `price-change` is new. Each reference solution was checked against that
  partial fix.
- **2026-10-23 calibration** (`--repeat 3`, `generic`, $7.30): `deepseek-v4.1-flash` 43/45
  ($0.60), `kimi-k3` 41/45 ($6.70). The rework raised steps a little (deepseek: `rust-calc`
  6→12, `tax-included` 11→15, `csv-export` 12→14) but split no task: each failure is a
  single run in three (`js-lru` twice; `rust-calc`; `category-cents`, `log-parser`,
  `migrate-fetch` for kimi), and every one is a real miss of the spec, not a check bug. A
  second cause doesn't stop a model that reproduces the symptom and tests its fix, and both
  models do. The suite stays useful for steps and cost; telling models apart on pass rate
  needs a different kind of task (see the roadmap).
