# Stage 14 — Word-Class Prediction & Token Bigram/Trigram Model

Stage 14 adds an encoder-side order-2 Markov model over seven compact Unicode
writing classes: Latin, Cyrillic, CJK/Hangul, Southeast Asian, Indic, RTL, and
other/unknown. The training sequence is derived deterministically from the
current dictionary using longest available dictionary matches; unmatched Unicode
scalars are represented as literal events.

The model stores unigram, bigram, and trigram counts. A smoothed conditional
probability is converted to a small integer surprisal proxy and used as a
transition cost in a dynamic-programming parser. The DP state retains the last
two writing classes and whether a literal run is already open. This lets the
parser compare token and literal transitions under the learned context.

Three penalty strengths are evaluated. Their resulting token streams compete
with the Stage 9 weighted parses and Stage 13 script-transition parses using
the actual minimum entropy payload cost, including rANS model-table overhead.
No Markov table is serialized: it only guides encoder-side parse selection, and
the archive remains independently decodable with the existing SP7 v2 format.

## Tests added

- Ordered trigram transitions are learned and common transitions receive no
  greater penalty than unseen alternatives under the same context.
- Predictive parsing round-trips multilingual Latin, Cyrillic, Japanese, Thai,
  and Arabic text.
- The final parser selection is no larger than each of the tested predictive
  candidates under the implemented entropy-cost metric.

## Limitations

This is a writing-class Markov model, not a semantic word-level language model.
Several different words within one script share a class. Training uses a greedy
longest-match token sequence, while the DP explores alternative segmentations.
The model is deliberately not stored in the archive, so it cannot add model
metadata, but it also cannot adapt to lexical distinctions beyond these seven
classes. The search is bounded and does not guarantee a globally optimal parse.
