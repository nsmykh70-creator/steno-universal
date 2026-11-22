# STENO-P7 stage checkpoint

Current checkpoint: Stage 14 — Word-Class Prediction & Token Bigram/Trigram Model.

Implemented through Stage 14:
- Unicode-aware candidate generation and multilingual segmentation.
- SP7 v2 lossless framing with v1 reader compatibility.
- Variable-width token IDs and adaptive token ordering.
- Joint dictionary/tokenization/entropy cost optimization.
- Bounded dictionary refinement and multi-entry beam search.
- Context-conditioned parsing and order-2 Markov writing-class prediction.
- Static rANS, context rANS, and adaptive range coding selection.
- Decoder limits, checked offsets, UTF-8 validation, and corruption checks.

Stage 14 adds an encoder-side seven-class Markov model trained on a greedy
longest-match dictionary token sequence. Its DP parser tracks two prior classes
and literal-run state. Three strengths compete using actual entropy payload
cost; no model tables are added to SP7 v2 archives.

Validation note: archive integrity and source-level checks can be performed in
environments without Rust, but `cargo test` and compilation must be run in a
Rust 1.70+ environment before treating the implementation as build-verified.
