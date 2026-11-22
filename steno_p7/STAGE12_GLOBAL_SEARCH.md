# Stage 12 — Global Dictionary Search & Token Interaction

## Goal

Stage 11 used exact local additions and replacements. Stage 12 adds a bounded beam search to explore interacting dictionary additions that may be unprofitable in isolation but beneficial as a combination.

## Algorithm

After Stage 11 refinement, the optimizer builds a deterministic pool of up to 16 not-yet-selected candidates. It then runs a beam search with:

- maximum depth: 3 additions;
- beam width: 4 partial dictionaries;
- maximum exact archive-cost evaluations: 384;
- duplicate dictionary states removed before the next beam is ranked.

Each expansion reorders the trial dictionary deterministically and calls the existing complete archive cost function. That function re-tokenizes the complete input, reselects the entropy representation and accounts for dictionary framing and payload metadata. A state is accepted only if its complete archive is strictly smaller than the Stage 11 incumbent.

The final dictionary still passes through adaptive token-ID ordering and a final exact-cost calculation.

## Why beam search

A purely greedy optimizer can reject token A because A alone does not save enough bytes. But A may change the parse so that token B becomes useful, or A+B may jointly replace many literal runs. Beam search preserves several promising partial dictionaries instead of committing to only one local path.

## Complexity and limits

The search is intentionally bounded. With the current constants it performs at most 384 complete-cost evaluations in this phase, with at most four partial states and three expansion layers. The full encoder still performs its earlier JTCO phases, so this is an additional bounded refinement rather than a global-optimality proof.

The candidate pool remains heuristic and input-derived. Beam search cannot discover a token that candidate extraction did not generate. It does not guarantee the mathematically smallest possible dictionary.

## Tests added

- deterministic output across repeated optimizer runs;
- complete-cost equality with the serialized archive size;
- multilingual-independent round-trip coverage on a mixed repeated-phrase sample;
- duplicate-free dictionary invariant.

Rust compilation and `cargo test` have not been run in environments without `rustc`/`cargo`; these tests are authored but not reported as passed.
