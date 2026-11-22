# STENO-P7 Stage 8 — Predictive Token Cost (PTC)

## Implemented
- Replaced greedy longest-match tokenization with a dynamic-programming parser over UTF-8 character boundaries.
- The parser evaluates every dictionary match in its bounded look-ahead window.
- Token edges are charged their exact 1–5 byte token-ID width.
- Literal edges are charged by their actual batched representation: UTF-8 bytes plus one 5-byte escape/length header per contiguous literal run.
- The parser reconstructs the minimum-cost path and emits the original byte sequence through dictionary tokens and literal runs.
- Deterministic tie handling preserves reproducible archives.

## Cost model
For a path through the text, the PTC objective is:

`C = sum(token_id_bytes) + sum(literal_run_bytes + 5)`

The 5-byte literal overhead is charged once per contiguous run, not once per character. This is exact for the token stream before the outer entropy layer. Entropy coding is subsequently selected by the existing full-archive-cost logic.

## Complexity and bounds
Let `N` be the number of Unicode scalar boundaries and `W = 3 * max_piece_chars` the bounded look-ahead. The parse is approximately `O(N * W)` dictionary lookups, with `O(N)` dynamic-programming memory. The candidate dictionary is already bounded by codec limits.

## Tests added
- overlapping dictionary matches roundtrip;
- Unicode and combining sequences roundtrip;
- long literal runs remain reversible and batched.

## Limitations
- The PTC objective is exact for the pre-entropy token stream, but it does not jointly optimize the parse against the final rANS/range-coded byte size. A future joint parse/entropy pass could improve this further.
- This is dynamic-programming minimum-cost parsing under the current dictionary and literal format, not a guarantee of globally optimal dictionary construction.
- Rust compilation and `cargo test` must be run in an environment with Rust 1.70 or newer; they were not run in this environment if the toolchain is unavailable.
