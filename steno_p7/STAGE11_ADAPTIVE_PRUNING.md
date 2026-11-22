# STENO-P7 Stage 11 — Adaptive Dictionary Refinement

## Goal

Improve the Stage 10 dictionary search without changing the SP7 v2 archive
format. The optimizer must minimize the complete serialized archive, not a
frequency-only proxy.

## Changes

Stage 10 already introduced greedy dictionary construction, pair look-ahead,
replacement, exact pruning, and adaptive token-ID ordering. Stage 11 adds a final
bounded neighborhood-refinement pass:

1. Build a deterministic pool of up to 48 not-yet-selected candidates.
2. Evaluate pure additions when the dictionary has room.
3. Evaluate one-for-one swaps against every currently selected entry (not only
a prefix of the dictionary).
4. Re-tokenize each trial, optimize token-ID order, and compare the full archive
   cost including dictionary records and the selected entropy representation.
5. Accept only strict byte-size improvements; perform at most two rounds and
   512 trial evaluations per optimization call.

The fixed evaluation budget bounds worst-case work and makes the result
reproducible. This is a local-search heuristic, not a proof of global optimality.

## Correctness

The archive format and decoder are unchanged. Tests cover deterministic search,
exact agreement between estimated and emitted archive size, and lossless
round-trip on mixed English, Russian, and Japanese text.

## Validation status

Source-level changes and test cases are included. `cargo test` has not been run
in this environment because the Rust toolchain (`cargo`/`rustc`) is unavailable.
