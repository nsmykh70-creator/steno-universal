# STENO-P7

STENO-P7 is an experimental lossless UTF-8 text preprocessor/archive written in Rust. The recovered prototype has been rebuilt into an SP7 v2 implementation while retaining a reader for the original v1 format.

## Implemented in this reconstruction

- Unicode-aware word/run extraction and subword candidate generation.
- Repeated phrase candidates (2- and 3-word sequences).
- No-whitespace script candidate extraction for CJK/Hangul and Thai/Lao/Khmer/Myanmar text, with UTF-8-safe code-point n-grams.
- Variable-length token IDs with five prefix tiers and canonical-form validation.
- Batched literal runs instead of one escape record per Unicode scalar.
- Optional static byte-frequency rANS (order-0), 21-state two-byte-class context rANS (order-2 class history), and adaptive 21-context arithmetic/range coding. The encoder compares complete serialized sizes and selects only a strictly smaller entropy representation.
- SP7 v2 framing and backward reading of SP7 v1 archives.
- Decoder limits for archive size, dictionary size, entry size, token stream size, and restored output size.
- Checked offsets, integer conversions, UTF-8 validation, duplicate/empty dictionary rejection, and trailing-data rejection.
- CLI commands: `compress`, `decompress`, `roundtrip`, `bench`.
- Benchmark Lab compares RAW, RAW→STENO, RAW→Zstd/LZMA2/GZIP and STENO→Zstd/LZMA2/GZIP. It records output size, ratio and elapsed time; unavailable external tools are marked instead of silently omitted. CSV export is supported.

## Build and test

Requires Rust 1.70+ (edition 2021; no third-party crates). Benchmark Lab optionally uses the external `zstd`, `xz` (raw LZMA2), and `gzip` command-line tools.

```sh
cargo test
cargo build --release
cargo run --release -- compress input.txt output.sp7
cargo run --release -- decompress output.sp7 restored.txt
cargo run --release -- bench input.txt report.csv
```

## Format notes

The byte-level format, token prefixes, entropy modes and safety limits are documented in [`FORMAT.md`](FORMAT.md). SP7 v2 begins with `SP7\0`, version byte `2`, a dictionary, the uncompressed token-stream length, an entropy mode byte, and the payload. Entropy mode `0` stores the token stream directly. Mode `1` stores 256 normalized `u16` byte frequencies (sum 4096) and an rANS payload. Mode `2` stores 21 256-symbol frequency tables (each used context sums to 4096; unused contexts are zero) and a context-conditioned rANS payload. Mode `3` stores an adaptive arithmetic-coded payload with an internal bit length and CRC-32 integrity check; its 21 context models are deterministic and require no serialized frequency tables. The v1 reader supports archives produced by the initial prototype.

## Scope and limitations

This is a lossless UTF-8 text codec, not a general binary compressor. JTCO now evaluates complete serialized size for every accepted dictionary addition, plus bounded pair-look-ahead and replacement moves. Candidate ordering remains heuristic and the bounded search is not a proof of global optimality. Regression coverage includes seeded multilingual corpora and truncated-archive rejection. Entropy contexts use four previous-byte classes (whitespace, ASCII alphanumeric, other ASCII (non-whitespace, non-alphanumeric), non-ASCII/other), with a dedicated start state and ordered two-class history for 21 total contexts. Mode 3 adapts symbol counts independently within those contexts and rescales them deterministically; it is not a full byte-value order-2 model.

## License

MIT

## Stage 5 context-adaptive model

The adaptive range-coder mode now uses a 32-context byte-history model (start,
previous-byte classes, and ordered previous-two-byte class pairs). See
`STAGE5_CONTEXT.md`. The static context-rANS mode keeps its 21-context layout for
SP7 v2 compatibility. Context adaptation is byte-level and is not a semantic or
word-level language model.

## Stage 8 — Predictive Token Cost
The encoder now uses a bounded dynamic-programming parse instead of greedy longest-match tokenization. It compares exact token-ID widths with batched literal-run costs (literal bytes plus one 5-byte escape/length header per run). See `STAGE8_PTC.md` for the objective, complexity, tests, and limitations.

## Stage 9 — Joint Parse & Entropy Optimization

The parser evaluates four bounded token/literal cost profiles and selects the resulting token stream using the actual minimum serialized entropy payload cost, including rANS model-table overhead. See `STAGE9_JPEO.md` for details and limitations.

## Stage 10: adaptive token IDs

Dictionary entries are deterministically reordered using observed token-use counts from the selected Stage 9 parse. Frequent entries are assigned the shortest available IDs first. The optimizer reparses for up to three passes to account for ID-width changes. See `STAGE10_TOKEN_IDS.md`.


## Stage 11: Adaptive Dictionary Refinement

Stage 11 adds a bounded final neighborhood search over dictionary additions and one-for-one swaps. Each candidate move is judged by the complete serialized archive size after re-tokenization, adaptive token-ID ordering, and entropy-mode selection. See `STAGE11_ADAPTIVE_PRUNING.md`.

## Stage 12: bounded global dictionary search

A deterministic beam search explores up to three interacting dictionary additions (beam width 4, candidate pool up to 16, at most 384 exact cost evaluations). Every state is measured using complete archive size after reparsing and entropy selection. This improves coverage beyond one-step greedy moves but is still bounded heuristic search, not a proof of global optimality. See `STAGE12_GLOBAL_SEARCH.md`.

## Stage 13: contextual token interaction

The parse selector now also evaluates three context-conditioned DP parses. The
context is the Unicode script class immediately before a candidate token; a
script-class switch incurs a small parser transition penalty. These candidates
are compared with the four Stage 9 profiles using the actual entropy payload
cost, including model overhead. This is a bounded local heuristic, not a trained
semantic language model or a global-optimality guarantee. See
`STAGE13_CONTEXTUAL_PARSING.md`.


## Stage 14: writing-class Markov prediction

Stage 14 trains a compact order-2 Markov model over seven Unicode writing
classes using a deterministic dictionary-tokenized training sequence. A
context-aware DP parser tracks the previous two classes and literal-run state;
three strengths are compared with prior parse candidates using final entropy
payload cost. The model is encoder-side guidance only and is not serialized, so
SP7 v2 compatibility is unchanged. This is not a semantic word-level model.
See `STAGE14_WORD_CLASS_PREDICTION.md`.

## Stage 15 — P8 economic token encoding (experimental)

A separate SP8 prototype is available in `src/economic.rs`. It uses prefix-free 1–5-byte token IDs, explicit literal runs, and a fail-open full-frame size gate. This stage is independent from the SP7 v2 format. See `STAGE15_ECONOMIC_TOKEN_ENCODING.md`.

## STENO-P8 experimental path

The experimental SP8 economic byte-token codec is in `src/economic.rs` and is intentionally separate from the stable SP7 path. Stage 15 introduced the reversible 1–5 byte token framing. Stage 16 adds exact dictionary-record accounting, code-length-aware candidate admission, and trie-based longest-match scanning. See `STAGE15_ECONOMIC_TOKEN_ENCODING.md` and `STAGE16_EXACT_ECONOMIC_OPTIMIZER.md`.

## STENO-P8 Stage 17 — Math Engine

The experimental `math` module provides a UTF-8-aware mathematical notation lexer, byte-accurate source spans, relation/calculus/matrix markers, and bounded repeated-token-sequence analysis. Its `encode`/`decode` API uses the shared SP8 economic frame and preserves source bytes exactly. See [STAGE17_MATH_ENGINE.md](STAGE17_MATH_ENGINE.md) for scope and limitations.

## STENO-P8 Stage 18 — CODE ENGINE

`src/code.rs` adds a lossless, language-agnostic source lexer and analyzer. It
classifies common keywords, identifiers, numeric/string/character literals,
comments, operators, punctuation and whitespace; tracks exact UTF-8 byte spans;
reports repeated exact token windows; and exposes `code::encode` / `code::decode`
through the shared SP8 economic frame. It does not parse a complete language AST
or normalize source. See `STAGE18_CODE_ENGINE.md` for scope and limitations.

## STENO-P8 Stage 19 — Joint Optimizer

`src/joint.rs` adds a linear TEXT/MATH/CODE/BINARY profile router and a single shared reversible SP8 encode/decode path. The router does not normalize or rewrite input. See `STAGE19_JOINT_OPTIMIZER.md` for the correctness contract and performance limitations.

## Stage 20: benchmark and integrity lab

See `STAGE20_BENCHMARK_INTEGRITY.md`. The new `integrity` module wraps SP8 frames in an S20 envelope with CRC-64/ECMA-182, original-length validation, and bounded decoding. Run `cargo test` and `cargo run --release --bin stage20_bench -- 5` to produce CSV throughput/ratio results.

The Stage 20 CSV runner optionally benchmarks installed `zstd`, `xz` (LZMA2), and `gzip` binaries against the same deterministic test corpora. Missing tools are reported as skipped on stderr.
