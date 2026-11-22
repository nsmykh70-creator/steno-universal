# STENO-P8 Stage 20 — Benchmark & Integrity Lab

## Goals

Stage 20 establishes measurable criteria for compression, throughput, and exact restoration. Compression is not considered a success unless the complete frame size (including the integrity envelope) is measured and decoded bytes exactly equal the source.

## Added components

- `src/integrity.rs`: S20 v1 envelope with original length and CRC-64/ECMA-182 around the SP8 payload. Decoder enforces caller-provided output bounds, verifies decoded length, then verifies checksum.
- `src/stage20.rs`: repeatable in-process benchmark primitives and deterministic synthetic TEXT/MATH/CODE/BINARY corpora.
- `src/bin/stage20_bench.rs`: CSV benchmark runner. Argument 1 is repetition count (default 3).

## Run

```sh
cargo test
cargo run --release --bin stage20_bench -- 5 > stage20-results.csv 2> stage20-skips.log
```

Use release builds for throughput comparisons. Run each competitor (Zstandard, LZMA2, GZIP) on the exact same corpus files and record full container sizes and independent encode/decode wall times. Do not compare different levels or omit wrapper metadata.

## Integrity guarantee and limits

The codec is designed for exact byte restoration. S20 adds CRC-64/ECMA-182 so accidental corruption is detected before returning output. CRC is not cryptographic authentication and does not protect against intentional tampering; authenticated encryption/MAC is a separate future feature. No algorithm can promise successful recovery from arbitrary archive corruption without redundancy; this stage detects corruption rather than repairing it.

## Performance policy

- Benchmark compression and decompression separately.
- Use identical input bytes, repetitions, machine, and build mode.
- Report full archive size and ratio, not just payload size.
- Always verify byte equality after decode.
- Keep synthetic corpora labelled synthetic; replace/add real user datasets before drawing general conclusions.
- A smaller archive that is materially slower must be reported as a trade-off, not an unconditional win.

## Current validation status

Unit tests are included, but must be executed with a Rust toolchain. The benchmark executable emits actual timings only when run; no benchmark results are claimed by this source package. The runner also invokes `zstd`, `xz` (LZMA2), and `gzip` through stdin/stdout when installed, using the same generated corpus and independently verifying each decoded result. Missing/failed tools are reported as `SKIP` on stderr.
