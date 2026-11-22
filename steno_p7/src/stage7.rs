//! Stage 7: reversible morphology/phrase candidate discovery.
//! This module only proposes exact byte-preserving dictionary strings; it does not
//! rewrite or normalize source text. Acceptance is delegated to full-cost JTCO.
use std::collections::HashMap;

/// Add repeated prefixes/suffixes from whitespace-delimited lexical units.
/// Prefix/suffix lengths are Unicode scalar counts, never byte offsets.
pub fn morphology_candidates(text: &str, out: &mut HashMap<String, usize>, min_chars: usize, max_chars: usize) {
    for unit in text.split_whitespace() {
        let chars: Vec<char> = unit.chars().collect();
        if chars.len() < min_chars * 2 || chars.len() > 256 { continue; }
        let cap = max_chars.min(chars.len() / 2).min(12);
        for n in min_chars..=cap {
            let prefix: String = chars[..n].iter().collect();
            let suffix: String = chars[chars.len()-n..].iter().collect();
            increment(out, prefix);
            increment(out, suffix);
        }
    }
}

/// Add repeated two/three-unit phrases with original spacing and punctuation.
/// Phrase candidates are exact source substrings; the codec remains lossless.
pub fn phrase_candidates(text: &str, out: &mut HashMap<String, usize>, max_bytes: usize) {
    let mut prev: Option<&str> = None;
    let mut prev2: Option<&str> = None;
    for unit in text.split_whitespace() {
        if let Some(p) = prev {
            let phrase = format!("{p} {unit}");
            if phrase.len() <= max_bytes { increment(out, phrase); }
        }
        if let (Some(p2), Some(p1)) = (prev2, prev) {
            let phrase = format!("{p2} {p1} {unit}");
            if phrase.len() <= max_bytes { increment(out, phrase); }
        }
        prev2 = prev;
        prev = Some(unit);
    }
}
fn increment(map: &mut HashMap<String, usize>, candidate: String) {
    if candidate.is_empty() { return; }
    if let Some(v) = map.get_mut(&candidate) { *v = v.saturating_add(1); }
    else if map.len() < 100_000 { map.insert(candidate, 1); }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn morphology_is_unicode_safe() {
        let mut c = HashMap::new();
        morphology_candidates("машинами машинами", &mut c, 2, 8);
        assert!(c.contains_key("маш"));
        assert!(c.contains_key("ми"));
    }
    #[test]
    fn phrase_candidates_preserve_exact_substrings() {
        let mut c = HashMap::new();
        phrase_candidates("Hello, world! Hello, world!", &mut c, 64);
        assert_eq!(c.get("Hello, world!"), Some(&2));
        assert_eq!(c.get("Hello, world! Hello, world!"), Some(&1));
    }
    #[test]
    fn phrase_size_limit_applies() {
        let mut c = HashMap::new();
        phrase_candidates("longword anotherword", &mut c, 8);
        assert!(c.is_empty());
    }
}
