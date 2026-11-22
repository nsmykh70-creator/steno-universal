# Stage 6 — Entropy model selection and full-cost comparison


The v2 encoder already evaluates these candidates against the serialized payload cost:

- raw token stream (mode 0)
- static byte rANS (mode 1, including its 256-entry frequency table)
- context byte rANS (mode 2, including its context tables)
- adaptive range coder (mode 3)

Stage 6 adds regression coverage for model-selection invariants: every chosen mode must decode losslessly, and short inputs must not break due to model overhead. Model selection is based on encoded payload plus model-table overhead, not entropy estimate alone. This is local heuristic selection, not a global proof of optimality.

Rust compilation and test execution require `cargo test`; the current workspace environment may not have Rust installed. Do not mark these tests as passed until they are actually run.
