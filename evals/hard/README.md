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
| `cancel-restock` | symptom far from cause | stock and emails react to events; the paid path skips one |
| `delivery-dates` | two causes | an off-by-one in business days, and holidays loaded as strings |
| `csv-export` | symptom | hand-joined CSV; the easy end of the suite |
| `exclusive-coupons` | feature, 2 files | a combination rule with edge cases |
| `tax-included` | feature, 4 files | config, totals and invoices must agree |
| `partial-shipment` | feature, 4 files | a new state, a new event field, stock and the file format |
| `rename-total` | refactor | the old name lives in templates and config too, but a file key must stay |
| `quote-total` | question | the tax depends on each line's share of the discount: run the code |

## Standalone

| task | needs | what makes it hard |
|---|---|---|
| `flaky-order` | python | the test fails depending on `PYTHONHASHSEED` |
| `migrate-fetch` | python | callers behind `functools.partial`, a dispatch table and an alias |
| `log-parser` | python | five separate bugs behind one test file |
| `rust-calc` | cargo | precedence and associativity in a Pratt parser |
| `js-lru` | node | an LRU cache with a time to live, built from a spec |

## Changing orderly

Tasks copy files from it into `solution/` (and buggy versions into `repo/`), so a change
to `_projects/orderly` can make a task's solution stale. After any change, run
`cargo test -p quack-exec --test suite`.

## Calibration

The difficulty above is a guess. The first live run should be one cheap and one strong
model at `--repeat 3`. A task every model passes every time, or fails every time, says
nothing about a setup; replace it.
