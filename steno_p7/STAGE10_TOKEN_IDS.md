# Stage 10 — Adaptive Dictionary & Token ID Optimization

STENO-P7 now performs a bounded deterministic token-ID reassignment pass.

1. Parse the current dictionary/token stream with the Stage 9 cost-aware parser.
2. Count emitted dictionary IDs (literal runs are skipped by their explicit length).
3. Rank dictionary entries by observed use count, then by prior ID tier, length, and lexical order.
4. Reparse and repeat up to three passes because ID widths can alter the best parse.
5. Use the same canonical ordering during complete-archive cost evaluation and final serialization.

The format remains SP7 v2: the dictionary order itself defines token IDs, so no additional metadata is required. The algorithm is a bounded local optimization, not a proof of globally optimal ID assignment.

Two regression tests cover short-ID assignment for a highly repeated token and deterministic dictionary ordering. Run `cargo test` with Rust 1.70 or newer to validate compilation and execution.
