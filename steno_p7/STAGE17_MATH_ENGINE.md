# STENO-P8 Stage 17 — MATH ENGINE

## Delivered

- Added `src/math.rs` and exported it as `steno_p7::math`.
- Added a UTF-8-aware mathematical notation lexer with byte-accurate spans.
- Token classes: numbers, identifiers, Greek letters, operators, delimiters, superscripts, subscripts, whitespace and other symbols.
- Added structural flags for relations, calculus operators and matrix-like delimiters.
- Added bounded detection of repeated exact token sequences (2–8 lexical tokens, at most 250,000 candidate windows, top 256 results).
- Added a lossless math-facing `encode`/`decode` API backed by the shared SP8 economic frame. No canonicalization, whitespace removal, spelling changes or Unicode normalization is performed.
- Invalid UTF-8 is rejected by the math lexer; the raw economic codec still supports arbitrary bytes.

## Important scope boundary

This stage is a notation lexer and repeated-surface-pattern analyzer, not a theorem prover, symbolic algebra system, LaTeX parser, or full AST. The common SP8 encoder remains responsible for dictionary selection and exact complete-frame economics. Math analysis currently does not create a separate math dictionary or guarantee a compression gain. It provides a safe structural layer for later math-aware candidate generation.

## Tests added (not executed in this environment)

- formula token classes and byte-span reconstruction;
- calculus and relation recognition;
- exact Unicode formula roundtrip;
- CJK preservation;
- invalid UTF-8 lexer rejection and raw byte-codec roundtrip.

Rust toolchain availability must be checked before treating these tests as passed.
