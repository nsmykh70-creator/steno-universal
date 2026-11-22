# Stage 9 — Joint Parse & Entropy Optimization (JPEO)

## Purpose
Stage 8 selected a parse primarily by pre-entropy token/literal byte cost. Stage 9 evaluates a bounded set of alternative dynamic-programming parses and chooses the one with the smallest serialized entropy payload cost.

## Candidate parses
The parser is run with four deterministic relative-cost profiles:

- token:literal = 1:1 (baseline)
- 2:1 (penalize token edges)
- 1:2 (penalize literal edges)
- 3:2 (stronger token-edge penalty)

Each parse is scored by the minimum available payload representation:

- RAW bytes
- static rANS payload + 512-byte frequency table
- context rANS payload + serialized context tables
- adaptive range payload

The winning token stream is used consistently by archive writing and JTCO's complete-archive cost function.

## Scope and limitations
This is a bounded local parse search, not a global optimum. It currently uses a small fixed set of cost profiles and does not jointly optimize dictionary membership and parse in one exhaustive search. Entropy coder implementations and the SP7 v2 wire format are unchanged.

## Validation status
A regression test checks that the selected parse is no larger under the implemented entropy-cost metric than any of the four candidates and that the archive round-trips mixed English, Russian, and Japanese text. ZIP integrity is checked when the checkpoint is produced. Rust compilation/tests require `cargo` and `rustc`; if unavailable, they are explicitly reported as not run.
