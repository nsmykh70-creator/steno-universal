//! STENO-P8 Stage 19: joint TEXT/MATH/CODE routing and lossless economic framing.
//!
//! The router is deliberately linear and advisory. It never rewrites source bytes;
//! the shared economic codec is the only transform and its decoder is authoritative.
use crate::economic::{self, Error, Stats};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile { Binary, Text, Math, Code, Mixed }

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    pub bytes: usize,
    pub ascii_letters: usize,
    pub whitespace: usize,
    pub digits: usize,
    pub math_symbols: usize,
    pub code_symbols: usize,
    pub braces: usize,
    pub line_breaks: usize,
    pub valid_utf8: bool,
    pub profile: Option<Profile>,
}

#[derive(Debug, Clone, Default)]
pub struct JointStats {
    pub analysis: Analysis,
    pub codec: Stats,
    pub encoded_bytes: usize,
    pub saved_bytes: isize,
    pub ratio_milli: u32,
}

/// Fast one-pass classifier. This is not a parser and never affects byte content.
pub fn analyze(input: &[u8]) -> Analysis {
    let mut a = Analysis { bytes: input.len(), valid_utf8: std::str::from_utf8(input).is_ok(), ..Analysis::default() };
    if !a.valid_utf8 { a.profile = Some(Profile::Binary); return a; }
    let s = std::str::from_utf8(input).expect("validated UTF-8");
    for c in s.chars() {
        if c.is_ascii_alphabetic() { a.ascii_letters += 1; }
        if c.is_whitespace() { a.whitespace += 1; }
        if c.is_ascii_digit() { a.digits += 1; }
        if c == '\n' { a.line_breaks += 1; }
        if matches!(c, '{'|'}'|'('|')'|'['|']'|';') { a.braces += 1; }
        if matches!(c, '+'|'-'|'*'|'/'|'='|'<'|'>'|'^'|'_'|'%'|'∫'|'∑'|'√'|'∞'|'∂'|'∇'|'≤'|'≥'|'≠'|'≈'|'×'|'÷'|'±'|'∈'|'∉'|'⊂'|'⊆'|'∧'|'∨'|'∥') || c.is_ascii_digit() && false { a.math_symbols += 1; }
        if matches!(c, '{'|'}'|';'|'#'|'@'|'$'|'`') || matches!(c, '+'|'-'|'*'|'/'|'='|'<'|'>'|'&'|'|'|'!'|':'|'.') { a.code_symbols += 1; }
    }
    let math_score = a.math_symbols;
    let code_score = a.code_symbols + a.braces.saturating_mul(2) + a.line_breaks;
    a.profile = Some(if math_score >= 3 && math_score > code_score { Profile::Math }
        else if a.braces >= 2 && code_score >= 4 { Profile::Code }
        else if math_score >= 3 && code_score >= 3 { Profile::Mixed }
        else { Profile::Text });
    a
}

/// Encodes any byte sequence. The returned frame is always decodable to the exact
/// original bytes, including invalid UTF-8 and embedded zero bytes.
pub fn encode(input: &[u8]) -> (Vec<u8>, JointStats) {
    let analysis = analyze(input);
    let (frame, codec) = economic::encode(input);
    let saved = input.len() as i128 - frame.len() as i128;
    let ratio = if input.is_empty() { 0 } else { ((frame.len() as u128 * 1000) / input.len() as u128).min(u32::MAX as u128) as u32 };
    let stats = JointStats { analysis, codec, encoded_bytes: frame.len(), saved_bytes: saved.clamp(isize::MIN as i128, isize::MAX as i128) as isize, ratio_milli: ratio };
    (frame, stats)
}

/// Strictly decodes a joint frame using the shared SP8 parser and output bound.
pub fn decode(frame: &[u8], max_output: usize) -> Result<Vec<u8>, Error> {
    economic::decode(frame, max_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn classifies_source_and_math_without_rewriting() {
        assert_eq!(analyze(b"fn main() { let x = 2; }").profile, Some(Profile::Code));
        assert_eq!(analyze("∫ x² + ∑ y = ∞".as_bytes()).profile, Some(Profile::Math));
        assert_eq!(analyze(b"ordinary repeated words").profile, Some(Profile::Text));
    }
    #[test] fn roundtrip_mixed_multilingual_math_and_code() {
        let s = "Русский текст. the the. ∫_0^∞ e^{-x²} dx = √π/2\nfn main() { let x = β₁ + 42; } 日本語";
        let (f, st) = encode(s.as_bytes());
        assert_eq!(decode(&f, 1_000_000).unwrap(), s.as_bytes());
        assert_eq!(st.encoded_bytes, f.len());
    }
    #[test] fn roundtrip_all_byte_values_and_empty_input() {
        let bytes: Vec<u8> = (0..=255).collect();
        let (f, _) = encode(&bytes); assert_eq!(decode(&f, 1024).unwrap(), bytes);
        let (f, _) = encode(b""); assert_eq!(decode(&f, 0).unwrap(), b"");
    }
    #[test] fn deterministic_output() {
        let s = b"repeatable repeatable repeatable text and source code";
        assert_eq!(encode(s).0, encode(s).0);
    }
    #[test] fn output_limit_is_enforced() {
        let (f, _) = encode(b"some data");
        assert_eq!(decode(&f, 2), Err(Error::OutputLimit));
    }
}
