# Stage 15 — STENO-P8 Economic Token Encoding

## Scope
Adds a standalone, reversible SP8 experimental frame (`src/economic.rs`) to investigate the economic token classes proposed for P8. It does not alter the SP7 v1/v2 wire format.

## Token tiers
- 1-byte: reserved short token IDs `00..7F`.
- 2-byte: `80..BF` prefix range.
- 3-byte: `C0..DF` prefix range.
- 4-byte: `E0..EF` prefix range.
- 5-byte: `F0..F7` prefix range.
- `FF` marks a literal run followed by a varint byte length and raw bytes.

Prefix tiers are disjoint, so token boundaries can be decoded without a token-length table. The 1-byte range can represent punctuation/symbol tokens; this prototype only dictionary-admits repeated text/operator runs and does not yet force all punctuation into one-byte codes.

## Current prototype behavior
- Scans valid UTF-8 into word/identifier and ASCII operator runs.
- Counts repeat candidates, ranks estimated savings, builds token codes.
- Emits literal spans as explicit length + raw bytes; preserves arbitrary input bytes losslessly.
- Compares complete SP8 frame against source size and falls back to a literal-only frame when SP8 is not smaller.
- Decoder validates header, lengths, dictionary duplicate codes, output cap and exact restored size.
- Includes multilingual, formula/code, arbitrary-byte and malformed-header round-trip tests.

## Limitations
- Candidate selection and dictionary admission are heuristic; not yet a globally optimal parser.
- No grammar-specific AST for math, Rust/C/Python or other code yet. Formulas and code are preserved as byte-exact runs; repeat runs may be tokenized.
- The current scanner is intentionally small and dependency-free. Later stages should add lexer profiles without making the decoder dependent on language runtimes.
- Rust tests/compilation were not run in this environment if `cargo`/`rustc` are unavailable; see package status.
