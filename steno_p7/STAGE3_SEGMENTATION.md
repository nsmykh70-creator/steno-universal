# STENO-P7 Stage 3 — Multilingual segmentation

## Changes
- Expanded script-aware candidate generation for Indic alphabets and Arabic/Hebrew RTL ranges.
- Kept sliding windows code-point based, so candidates remain UTF-8 safe.
- Candidate windows reset when the script group changes; punctuation and spaces reset active windows.
- RTL candidate windows capped at six code points to avoid oversized candidate pools.
- Added regression tests for Indic/RTL round trips and script-boundary behavior.

## Limitations
This is a script-range heuristic, not linguistic tokenization. It does not yet perform Unicode grapheme-cluster normalization, morphological analysis, dictionary-based word segmentation, or language identification. It preserves input bytes/text exactly and does not normalize the user's text.
