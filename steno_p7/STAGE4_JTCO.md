# STENO-P7 Stage 4 — JTCO neighborhood optimization

This stage adds exact-cost pruning of accepted dictionary entries and a bounded replacement neighborhood after the prior greedy/pair/replacement phases. Every candidate move is evaluated with `complete_archive_cost`, including dictionary records, token stream, and the selected entropy-mode overhead.

The search remains heuristic and bounded. It is not a globally optimal solution to the joint dictionary/tokenization/entropy problem.

Rust compilation and tests have not been executed in this environment (toolchain unavailable); the archive is source-validated and ZIP-integrity checked only.
