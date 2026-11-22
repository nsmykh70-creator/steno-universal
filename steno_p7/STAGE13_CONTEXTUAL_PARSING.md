# Stage 13 — Contextual Token Interaction & Predictive Parsing

## Goal

Add deterministic context-conditioned parse candidates to the existing bounded
parse/entropy joint selector, without changing SP7 v2 framing or decoder rules.

## Context signal

The new parser uses the Unicode script class immediately preceding a candidate
match as a compact context state. Script classes are supplied by the existing
`script_group` classifier (CJK/Hangul, Southeast Asian scripts, Indic scripts,
RTL scripts, and the default/other class). A token whose first character changes
the active script class pays a small transition penalty. Three penalty strengths
(1, 2, and 3 parser-cost units) are evaluated.

This is a deliberately compact local context model. It is not a trained language
model, does not predict semantic words, and does not maintain a full previous
Token-ID bigram table. It provides a low-cost context-sensitive alternative to
the four Stage 9 token/literal DP profiles.

## Selection objective

The contextual parser's transition penalty is only a search heuristic. Each
resulting token stream is measured with the existing exact entropy payload
selector (RAW, static rANS, context rANS, adaptive range), including model-table
overhead. `best_parse_stream` selects the smallest measured payload across all
four Stage 9 profiles and three contextual candidates. The archive format and
round-trip decoder remain unchanged.

## Tests added

- contextual parsing produces streams for mixed English, Russian, Japanese and
  Thai text and the archive round-trips exactly;
- the selected parse is no larger under the implemented entropy metric than any
  of the three contextual candidates.

## Limitations

The script-class context is a heuristic, not a claim of global parse optimality.
Its local penalty units are not bits and do not replace the entropy coder's
actual cost measurement. A future semantic context model should compare compact
word-class or token-pair contexts and serialize model data only when the complete
archive becomes smaller.

Rust compilation and `cargo test` must be run with Rust 1.70+; they were not run
in this environment because `rustc` and `cargo` are unavailable.
