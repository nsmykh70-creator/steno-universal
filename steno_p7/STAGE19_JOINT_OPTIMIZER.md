# STENO-P8 Stage 19 — Joint Optimizer

## Purpose
Unify TEXT, MATH, CODE and arbitrary binary input under one reversible frame.

## Architecture
- `joint::analyze` performs one linear UTF-8 character scan (or marks arbitrary bytes as Binary). It classifies likely Text/Math/Code/Mixed content. Classification is advisory only.
- `joint::encode` invokes the existing SP8 exact-economic encoder once. It does not run both math and code lexers, avoiding repeated tokenization cost.
- `joint::decode` delegates to the strict SP8 decoder with an explicit output-size limit.
- No normalization, whitespace removal, syntax rewriting, or Unicode conversion occurs. The original byte stream is the sole reconstruction source.

## Correctness contract
For every input byte sequence `x`, the required invariant is `decode(encode(x)) == x`. Tests cover UTF-8 mixed content, all 256 byte values, empty input, deterministic output and output-limit rejection. The decoder rejects malformed frames through the shared SP8 parser.

## Speed and compression policy
The router is O(n) and does not invoke the bounded repeated-sequence scans from `math` or `code`. The economic encoder remains the compression stage. Compression ratio is reported as encoded/input in thousandths; negative `saved_bytes` means framing expanded the input. No speed or compression superiority is claimed without benchmark execution. The next optimization must benchmark full encode/decode time and archive size against raw, zstd, LZMA2 and gzip on identical corpora.

## Known limitations
- Profile classification is heuristic, not a full language parser.
- Stage 19 does not yet provide distinct math/code entropy models; it routes all content through the common SP8 byte-token engine.
- The SP8 format has framing overhead and may expand incompressible or tiny inputs. Losslessness takes priority; downstream container selection should compare full archive sizes before choosing SP8.
- Rust tests are included but require a Rust toolchain to execute.
