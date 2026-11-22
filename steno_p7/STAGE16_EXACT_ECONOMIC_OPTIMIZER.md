# Stage 16 — Exact Economic Token Optimizer

Stage 16 improves the SP8 experimental byte-token layer without changing the SP7 archive format.

## Changes

- Token admission now uses the assigned code length (1–5 bytes), not a fixed two-byte assumption.
- Dictionary record cost includes the exact variable-length integer prefixes for code and token lengths.
- A candidate is admitted only when its estimated aggregate byte savings exceed its dictionary record cost and a small framing allowance.
- The final decision remains an exact whole-frame comparison; if the complete SP8 frame is not smaller than the source, the encoder emits a dictionary-free literal frame.
- Replaced per-input-position linear scans across the entire token map with a byte trie and longest-match traversal. This removes the `O(input_bytes × dictionary_size)` matching pattern.
- Added tests for longest-match behavior, serialized-size accounting, and code-length prefix boundaries.

## Scope and limitations

This is a reversible byte-level token optimizer, not yet a full parser for programming-language syntax or mathematical expression trees. It preserves source bytes exactly. Math and source-code sequences can benefit as ordinary repeated byte tokens; syntax-aware transforms remain future work. The current candidate scanner is UTF-8-aware and only extracts candidates from valid UTF-8 input; arbitrary binary input safely falls back to literals.

## Validation

The ZIP is structurally checked. Rust tests are included but were not executed in the current environment because `cargo` and `rustc` are unavailable. Do not interpret archive validation as a successful Rust build.
