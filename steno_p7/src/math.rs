//! STENO-P8 Stage 17: lossless mathematical notation analysis.
//!
//! The lexer recognizes mathematical surface syntax without normalizing it.
//! Byte spans always refer to the original UTF-8 input. Encoding delegates to
//! the shared SP8 economic frame, so dictionary and framing overhead are paid
//! once and the exact source bytes remain recoverable.

use crate::economic::{self, Stats};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MathTokenKind {
    Number,
    Identifier,
    Greek,
    Operator,
    Delimiter,
    Superscript,
    Subscript,
    Whitespace,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MathToken {
    pub kind: MathTokenKind,
    pub start: usize,
    pub end: usize,
}

impl MathToken {
    pub fn bytes<'a>(&self, input: &'a [u8]) -> &'a [u8] { &input[self.start..self.end] }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MathAnalysis {
    pub tokens: Vec<MathToken>,
    pub repeated_sequences: Vec<RepeatedSequence>,
    pub has_relation: bool,
    pub has_calculus_operator: bool,
    pub has_matrix_delimiter: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatedSequence {
    pub token_start: usize,
    pub token_len: usize,
    pub occurrences: usize,
}

fn is_greek(c: char) -> bool {
    matches!(c as u32, 0x0370..=0x03FF | 0x1F00..=0x1FFF)
}
fn is_superscript(c: char) -> bool {
    matches!(c as u32, 0x00B2 | 0x00B3 | 0x00B9 | 0x2070..=0x207F)
}
fn is_subscript(c: char) -> bool { matches!(c as u32, 0x2080..=0x208E) }
fn is_operator(c: char) -> bool {
    matches!(c, '+' | '-' | '−' | '*' | '×' | '·' | '/' | '÷' | '=' | '<' | '>' | '≤' | '≥' | '≠' | '≈' | '≡' | '∝' | '±' | '∓' | '^' | '_' | '√' | '∛' | '∜' | '∫' | '∬' | '∭' | '∮' | '∑' | '∏' | '∐' | '∇' | '∂' | '∞' | '∈' | '∉' | '∩' | '∪' | '⊂' | '⊆' | '⊃' | '⊇' | '→' | '←' | '↔' | '⇒' | '⇔' | '∀' | '∃' | '¬' | '∧' | '∨' | '⊕' | '⊗' | '∥' | '∠' | '∴' | '∵')
}
fn is_delimiter(c: char) -> bool {
    matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | '⟨' | '⟩' | '⌈' | '⌉' | '⌊' | '⌋' | '|' | ',' | ';' | ':' | '.' | '、' | '。')
}
fn class(c: char) -> MathTokenKind {
    if c.is_whitespace() { MathTokenKind::Whitespace }
    else if is_superscript(c) { MathTokenKind::Superscript }
    else if is_subscript(c) { MathTokenKind::Subscript }
    else if is_greek(c) { MathTokenKind::Greek }
    else if c.is_ascii_digit() { MathTokenKind::Number }
    else if c.is_alphabetic() || c == '\\' { MathTokenKind::Identifier }
    else if is_operator(c) { MathTokenKind::Operator }
    else if is_delimiter(c) { MathTokenKind::Delimiter }
    else { MathTokenKind::Other }
}

/// Lexes valid UTF-8 mathematical text. Invalid UTF-8 is rejected rather than
/// sliced, while callers can always use the raw economic codec as a fallback.
pub fn tokenize(input: &[u8]) -> Result<Vec<MathToken>, std::str::Utf8Error> {
    let text = std::str::from_utf8(input)?;
    let mut out: Vec<MathToken> = Vec::new();
    let mut current: Option<MathToken> = None;
    for (start, ch) in text.char_indices() {
        let end = start + ch.len_utf8();
        let kind = class(ch);
        match current.as_mut() {
            Some(t) if t.kind == kind && matches!(kind, MathTokenKind::Number | MathTokenKind::Identifier | MathTokenKind::Whitespace) => t.end = end,
            Some(_) => { out.push(current.take().unwrap()); current = Some(MathToken { kind, start, end }); }
            None => current = Some(MathToken { kind, start, end }),
        }
    }
    if let Some(t) = current { out.push(t); }
    Ok(out)
}

fn find_repeated(tokens: &[MathToken], input: &[u8]) -> Vec<RepeatedSequence> {
    // Exact byte-identical token sequences, lengths 2..=8. A conservative
    // bounded scan avoids quadratic blowups on very long formula documents.
    let mut counts: HashMap<Vec<u8>, (usize, usize, usize)> = HashMap::new();
    let n = tokens.len();
    let mut examined = 0usize;
    for i in 0..n {
        for len in 2..=8.min(n.saturating_sub(i)) {
            if examined >= 250_000 { break; }
            examined += 1;
            let start = tokens[i].start;
            let end = tokens[i + len - 1].end;
            let key = input[start..end].to_vec();
            let entry = counts.entry(key).or_insert((i, len, 0));
            entry.2 += 1;
        }
        if examined >= 250_000 { break; }
    }
    let mut result: Vec<_> = counts.into_values().filter(|(_, _, count)| *count > 1)
        .map(|(token_start, token_len, occurrences)| RepeatedSequence { token_start: tokens[token_start].start, token_len, occurrences }).collect();
    result.sort_by(|a,b| b.occurrences.cmp(&a.occurrences).then(b.token_len.cmp(&a.token_len)).then(a.token_start.cmp(&b.token_start)));
    result.truncate(256);
    result
}

pub fn analyze(input: &[u8]) -> Result<MathAnalysis, std::str::Utf8Error> {
    let tokens = tokenize(input)?;
    let mut analysis = MathAnalysis::default();
    analysis.has_relation = tokens.iter().any(|t| {
        let b = t.bytes(input);
        t.kind == MathTokenKind::Operator && (b == b"=" || b == b"<" || b == b">" || b == "≤".as_bytes() || b == "≥".as_bytes() || b == "≠".as_bytes() || b == "≈".as_bytes())
    });
    analysis.has_calculus_operator = tokens.iter().any(|t| {
        let b = t.bytes(input);
        t.kind == MathTokenKind::Operator && ["∫", "∬", "∭", "∑", "∏", "∂", "∇"].iter().any(|s| b == s.as_bytes())
    });
    analysis.has_matrix_delimiter = tokens.iter().any(|t| t.kind == MathTokenKind::Delimiter && (t.bytes(input) == b"[" || t.bytes(input) == b"{"));
    analysis.repeated_sequences = find_repeated(&tokens, input);
    analysis.tokens = tokens;
    Ok(analysis)
}

/// Lossless SP8 framing using the shared economic optimizer. Math analysis is
/// intentionally advisory: no semantic rewriting or Unicode normalization.
pub fn encode(input: &[u8]) -> (Vec<u8>, Stats) { economic::encode(input) }
pub fn decode(frame: &[u8], max_output: usize) -> Result<Vec<u8>, economic::Error> { economic::decode(frame, max_output) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_formula_components_and_preserves_spans() {
        let s = "a^2 + β₁ = √π";
        let tokens = tokenize(s.as_bytes()).unwrap();
        assert_eq!(tokens.iter().map(|t| t.bytes(s.as_bytes())).collect::<Vec<_>>().concat(), s.as_bytes());
        assert!(tokens.iter().any(|t| t.kind == MathTokenKind::Superscript));
        assert!(tokens.iter().any(|t| t.kind == MathTokenKind::Subscript));
        assert!(tokens.iter().any(|t| t.kind == MathTokenKind::Greek));
    }

    #[test]
    fn recognizes_calculus_and_relations() {
        let a = analyze("∫_0^∞ e^{-x²} dx = √π/2".as_bytes()).unwrap();
        assert!(a.has_calculus_operator);
        assert!(a.has_relation);
    }

    #[test]
    fn unicode_math_roundtrip_is_byte_exact() {
        let s = "∫_0^∞ e^{-x²} dx = √π/2\nМатрица: [α₁ β₂; γ₃ δ₄]";
        let (frame, _) = encode(s.as_bytes());
        assert_eq!(decode(&frame, 1_000_000).unwrap(), s.as_bytes());
    }

    #[test]
    fn ordinary_cjk_characters_are_not_rewritten_by_lexer() {
        let s = "公式 日本語 中文";
        let tokens = tokenize(s.as_bytes()).unwrap();
        assert_eq!(tokens.iter().map(|t| t.bytes(s.as_bytes())).collect::<Vec<_>>().concat(), s.as_bytes());
    }

    #[test]
    fn invalid_utf8_is_rejected_by_math_lexer_but_raw_codec_remains_lossless() {
        let bytes = [0xff, 0x80, 0x00];
        assert!(tokenize(&bytes).is_err());
        let (frame, _) = encode(&bytes);
        assert_eq!(decode(&frame, 100).unwrap(), bytes);
    }
}
