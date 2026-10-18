# Cross-crate tests and fixtures

- `fixtures/providers/<server>/<model>/*.sse`: recorded provider streams (M0/M1)
- invariant tests (no egress, prefix stability, resume fidelity): docs/plan/13-testing-and-evals.md

Crate-level unit and snapshot tests live next to the code.
