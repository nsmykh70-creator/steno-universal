# STENO-P8 Stage 18 — CODE ENGINE

## Implemented

- Added and exported `src/code.rs` as `steno_p7::code`.
- UTF-8 lexer with exact byte offsets for identifiers, common cross-language
  keywords, numbers, quoted strings, character literals, comments, operators,
  punctuation, whitespace and other characters.
- Supports `//` line comments, nested `/* ... */` block comments, escaped
  characters in quotes, and backtick-delimited template strings.
- Produces source statistics and bounded repeated exact token-window candidates
  (2–12 lexical tokens, at most 200,000 examined windows and 256 results).
- `encode` / `decode` use the common SP8 economic frame. No source rewriting,
  whitespace folding, identifier renaming or semantic normalization occurs.
- Invalid UTF-8 is rejected by the source lexer; the raw codec still handles
  arbitrary bytes losslessly.

## Scope and limitations

This is a conservative lexical analyzer, not a complete parser, compiler front
end, AST builder or language-specific formatter. Keyword classification uses a
shared union of common language keywords; `#` is treated as a line comment, so
preprocessor directives and Python comments are not distinguished yet. Nested
block comments are accepted as a robust superset, even though some languages do
not permit nesting. Template-string interpolation is retained as one literal
span rather than recursively tokenized. These limitations affect analysis only,
not the lossless codec path.

The analyzer does not yet generate a specialized code dictionary. The shared
SP8 encoder is still the only compression engine and its final complete-frame
size check determines whether tokenization is worthwhile.

## Tests added (not executed here)

- exact token-span reconstruction;
- keyword/identifier/literal classification;
- nested block comments and escaped quotes;
- UTF-8 source roundtrip;
- arbitrary binary fallback and invalid UTF-8 rejection.

Rust tests require a Rust toolchain (`cargo test`).
