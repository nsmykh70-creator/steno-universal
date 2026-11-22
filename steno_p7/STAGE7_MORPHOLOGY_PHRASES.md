# STENO-P7 Stage 7 — Morphology & Phrase Candidate Discovery

## Purpose
Expand the candidate vocabulary beyond complete words and character n-grams by proposing repeated Unicode-safe prefixes, suffixes, and exact 2/3-unit phrase strings. Candidates are exact substrings; no normalization, case-folding, or source rewriting is performed.

## Components
- `src/stage7.rs`: separate candidate proposal functions and unit tests.
- `morphology_candidates`: emits Unicode scalar prefixes/suffixes for bounded lexical units.
- `phrase_candidates`: emits exact whitespace-delimited two/three-unit phrases, bounded by bytes.
- `src/lib.rs`: integrates Stage 7 proposals into candidate pool before JTCO scoring.

## Selection policy
These proposals are only a pool. JTCO measures complete archive cost and may reject candidates. This prevents candidate frequency or morphological plausibility from being mistaken for guaranteed compression gain.

## Limits and known limitations
- Whitespace-delimited units are not a true linguistic tokenizer.
- Prefix/suffix candidates are generic, not language-specific morpheme analysis.
- Phrase candidates retain punctuation attached to units, but do not model arbitrary whitespace variants.
- Candidate selection is bounded and heuristic; it does not guarantee a global optimum.
- Rust tests are authored but cannot be executed until `cargo`/`rustc` are available.
