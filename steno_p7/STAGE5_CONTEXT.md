# STENO-P7 Stage 5 — Context-adaptive entropy model

Stage 5 refines the adaptive range coder's context selection while preserving the SP7 v2 archive layout.

## Model

The adaptive range coder now uses 32 contexts:
- context 0: stream start;
- contexts 1–6: one preceding-byte class (whitespace, alphabetic, digit, punctuation, non-ASCII, other ASCII);
- contexts 7–31: ordered pairs of previous-byte history classes, each reduced to five categories (whitespace, alphanumeric, punctuation, non-ASCII, other).

Each context maintains its own adaptive symbol-frequency model. The model is updated after every symbol and rescaled when its total reaches the existing threshold. The range coder's checksum remains in place.

The fixed-table rANS context mode remains at 21 contexts. Its layout is not changed, so Stage 5 does not alter the SP7 v2 mode-2 table interpretation.

## Test additions

Added a mixed corpus round-trip integration test containing whitespace, punctuation, ASCII digits/letters, Cyrillic, Japanese, Thai, Arabic and tab characters. Existing adaptive range-coder tests remain.

## Limitations

This is byte-level contextual entropy coding, not word-level semantic context or a language model. Contexts are deterministic classes of the preceding bytes, and no claim of better compression is made until real benchmark runs compare complete archive sizes. Rust tests were not executed in this environment because `cargo` and `rustc` are unavailable.
