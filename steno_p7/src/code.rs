//! STENO-P8 Stage 18: lossless source-code lexical analysis.
//!
//! This is a conservative, language-agnostic lexer for common C/Rust/JS/Python-like
//! source syntax. It records byte spans and never rewrites source text. Compression
//! remains delegated to the shared SP8 economic frame and its whole-frame gate.

use crate::economic::{self, Stats};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeTokenKind {
    Identifier, Keyword, Number, StringLiteral, CharacterLiteral, Comment,
    Operator, Punctuation, Whitespace, Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeToken { pub kind: CodeTokenKind, pub start: usize, pub end: usize }
impl CodeToken {
    pub fn bytes<'a>(&self, input: &'a [u8]) -> &'a [u8] { &input[self.start..self.end] }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatedCodeSequence { pub byte_start: usize, pub token_len: usize, pub occurrences: usize }

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeAnalysis {
    pub tokens: Vec<CodeToken>,
    pub repeated_sequences: Vec<RepeatedCodeSequence>,
    pub identifier_count: usize,
    pub keyword_count: usize,
    pub comment_count: usize,
    pub string_count: usize,
    pub operator_count: usize,
    pub has_brace_blocks: bool,
    pub has_function_like_keyword: bool,
}

fn is_ident_start(c: char) -> bool { c == '_' || c == '$' || c.is_alphabetic() }
fn is_ident_continue(c: char) -> bool { is_ident_start(c) || c.is_numeric() }
fn is_op(c: char) -> bool { matches!(c, '+'|'-'|'*'|'/'|'%'|'='|'!'|'<'|'>'|'&'|'|'|'^'|'~'|'?'|':'|'.'|'@'|'#'|'\\') }
fn is_punct(c: char) -> bool { matches!(c, '('|')'|'['|']'|'{'|'}'|','|';') }

fn is_keyword(s: &str) -> bool {
    // Union of common language keywords. Classification is advisory, not a parser.
    matches!(s,
        "if"|"else"|"elif"|"for"|"while"|"loop"|"do"|"break"|"continue"|"return"|
        "fn"|"func"|"function"|"def"|"lambda"|"class"|"struct"|"enum"|"trait"|"impl"|
        "interface"|"extends"|"implements"|"pub"|"public"|"private"|"protected"|"static"|
        "const"|"let"|"var"|"mut"|"final"|"readonly"|"async"|"await"|"yield"|"match"|
        "switch"|"case"|"default"|"try"|"catch"|"finally"|"throw"|"raise"|"import"|
        "from"|"use"|"mod"|"package"|"namespace"|"export"|"as"|"in"|"of"|"where"|
        "type"|"typeof"|"instanceof"|"new"|"delete"|"this"|"self"|"super"|"true"|"false"|
        "null"|"nil"|"None"|"undefined"|"and"|"or"|"not"|"is"|"with"|"pass"|"assert"|
        "global"|"nonlocal"|"unsafe"|"extern"|"move"|"select"|"go"|"defer"|"chan"|"range")
}

fn push_token(out: &mut Vec<CodeToken>, kind: CodeTokenKind, start: usize, end: usize) {
    if start < end { out.push(CodeToken { kind, start, end }); }
}

/// Tokenize source code while preserving exact byte offsets. Invalid UTF-8 is rejected.
pub fn tokenize(input: &[u8]) -> Result<Vec<CodeToken>, std::str::Utf8Error> {
    let s = std::str::from_utf8(input)?;
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (start, c) = chars[i];
        let end_at = |idx: usize| -> usize { if idx < chars.len() { chars[idx].0 } else { input.len() } };
        if c.is_whitespace() {
            i += 1; while i < chars.len() && chars[i].1.is_whitespace() { i += 1; }
            push_token(&mut out, CodeTokenKind::Whitespace, start, end_at(i)); continue;
        }
        // C-style line and block comments; block comments support nesting.
        if c == '/' && i + 1 < chars.len() && chars[i+1].1 == '/' {
            i += 2; while i < chars.len() && chars[i].1 != '\n' { i += 1; }
            push_token(&mut out, CodeTokenKind::Comment, start, end_at(i)); continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i+1].1 == '*' {
            i += 2; let mut depth = 1usize;
            while i < chars.len() && depth > 0 {
                if chars[i].1 == '/' && i+1 < chars.len() && chars[i+1].1 == '*' { depth += 1; i += 2; }
                else if chars[i].1 == '*' && i+1 < chars.len() && chars[i+1].1 == '/' { depth -= 1; i += 2; }
                else { i += 1; }
            }
            push_token(&mut out, CodeTokenKind::Comment, start, end_at(i)); continue;
        }
        // Quoted strings/chars and backtick templates. Escapes are consumed verbatim.
        if c == '"' || c == '\'' || c == '`' {
            let quote = c; i += 1;
            while i < chars.len() {
                if chars[i].1 == '\\' { i = (i + 2).min(chars.len()); continue; }
                let ch = chars[i].1; i += 1; if ch == quote { break; }
            }
            let kind = if quote == '\'' { CodeTokenKind::CharacterLiteral } else { CodeTokenKind::StringLiteral };
            push_token(&mut out, kind, start, end_at(i)); continue;
        }
        // Python/shell-style # line comments. In other languages this remains a conservative heuristic.
        if c == '#' {
            i += 1; while i < chars.len() && chars[i].1 != '\n' { i += 1; }
            push_token(&mut out, CodeTokenKind::Comment, start, end_at(i)); continue;
        }
        if is_ident_start(c) {
            i += 1; while i < chars.len() && is_ident_continue(chars[i].1) { i += 1; }
            let end = end_at(i); let word = &s[start..end];
            push_token(&mut out, if is_keyword(word) { CodeTokenKind::Keyword } else { CodeTokenKind::Identifier }, start, end); continue;
        }
        if c.is_numeric() {
            i += 1;
            while i < chars.len() && (chars[i].1.is_alphanumeric() || matches!(chars[i].1, '_'|'.')) { i += 1; }
            push_token(&mut out, CodeTokenKind::Number, start, end_at(i)); continue;
        }
        if is_op(c) {
            i += 1; while i < chars.len() && is_op(chars[i].1) { i += 1; }
            push_token(&mut out, CodeTokenKind::Operator, start, end_at(i)); continue;
        }
        if is_punct(c) { i += 1; push_token(&mut out, CodeTokenKind::Punctuation, start, end_at(i)); continue; }
        i += 1; push_token(&mut out, CodeTokenKind::Other, start, end_at(i));
    }
    Ok(out)
}

fn repeated_sequences(tokens: &[CodeToken], input: &[u8]) -> Vec<RepeatedCodeSequence> {
    let mut counts: HashMap<Vec<u8>, (usize, usize, usize)> = HashMap::new();
    let mut examined = 0usize;
    for i in 0..tokens.len() {
        for len in 2..=12.min(tokens.len() - i) {
            if examined >= 200_000 { break; }
            examined += 1;
            let a = tokens[i].start; let b = tokens[i + len - 1].end;
            let e = counts.entry(input[a..b].to_vec()).or_insert((i, len, 0)); e.2 += 1;
        }
        if examined >= 200_000 { break; }
    }
    let mut found: Vec<_> = counts.into_values().filter(|(_, _, n)| *n > 1)
        .map(|(idx, len, n)| RepeatedCodeSequence { byte_start: tokens[idx].start, token_len: len, occurrences: n }).collect();
    found.sort_by(|a,b| b.occurrences.cmp(&a.occurrences).then(b.token_len.cmp(&a.token_len)).then(a.byte_start.cmp(&b.byte_start)));
    found.truncate(256); found
}

pub fn analyze(input: &[u8]) -> Result<CodeAnalysis, std::str::Utf8Error> {
    let tokens = tokenize(input)?;
    let mut a = CodeAnalysis::default();
    for t in &tokens {
        match t.kind {
            CodeTokenKind::Identifier => a.identifier_count += 1,
            CodeTokenKind::Keyword => a.keyword_count += 1,
            CodeTokenKind::Comment => a.comment_count += 1,
            CodeTokenKind::StringLiteral | CodeTokenKind::CharacterLiteral => a.string_count += 1,
            CodeTokenKind::Operator => a.operator_count += 1,
            _ => {}
        }
        if t.kind == CodeTokenKind::Punctuation && t.bytes(input) == b"{" { a.has_brace_blocks = true; }
        if t.kind == CodeTokenKind::Keyword && ["fn", "func", "function", "def"].iter().any(|k| t.bytes(input) == k.as_bytes()) { a.has_function_like_keyword = true; }
    }
    a.repeated_sequences = repeated_sequences(&tokens, input);
    a.tokens = tokens;
    Ok(a)
}

/// Exact source-byte codec API. The common SP8 frame performs final size admission.
pub fn encode(input: &[u8]) -> (Vec<u8>, Stats) { economic::encode(input) }
pub fn decode(frame: &[u8], max_output: usize) -> Result<Vec<u8>, economic::Error> { economic::decode(frame, max_output) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn token_spans_reconstruct_source_exactly() {
        let s = "pub fn main() { let x = 42; // note\n println!(\"hi\\n\"); }";
        let t = tokenize(s.as_bytes()).unwrap();
        assert_eq!(t.iter().map(|x| x.bytes(s.as_bytes())).collect::<Vec<_>>().concat(), s.as_bytes());
    }
    #[test] fn classifies_keywords_identifiers_and_literals() {
        let a = analyze("fn calc(value: i32) { let answer = value + 42; }".as_bytes()).unwrap();
        assert!(a.keyword_count >= 2); assert!(a.identifier_count >= 3); assert!(a.has_function_like_keyword); assert!(a.has_brace_blocks);
    }
    #[test] fn handles_nested_block_comments_and_escaped_quotes() {
        let s = br#"/* outer /* inner */ end */ let s = "a\"b";"#;
        let t = tokenize(s).unwrap();
        assert!(t.iter().any(|x| x.kind == CodeTokenKind::Comment));
        assert!(t.iter().any(|x| x.kind == CodeTokenKind::StringLiteral));
    }
    #[test] fn source_roundtrip_is_byte_exact() {
        let s = "// комментарий\nfn main() { let x = \"λ\"; x + x }\n";
        let (frame, _) = encode(s.as_bytes()); assert_eq!(decode(&frame, 1_000_000).unwrap(), s.as_bytes());
    }
    #[test] fn arbitrary_binary_fallback_remains_lossless() {
        let b = [0xff, 0, 0x80, 1]; let (f, _) = encode(&b); assert_eq!(decode(&f, 100).unwrap(), b);
        assert!(tokenize(&b).is_err());
    }
}
