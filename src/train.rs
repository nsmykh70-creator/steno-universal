//! Обучение словаря на входном файле: атомизация, н-граммы фраз 2–10 атомов,
//! MDL-отбор с учётом стоимости записи в кадре и упрощённая PMI-поддержка
//! предшествующего контекста.
//!
//! Портирование идей STENO-P2 (MDL-гейн по слоям), P3 (CJK/тайская атомизация
//! по кодовым точкам) и P5 (детерминированные окна обучения без сшивки фраз
//! на искусственных стыках; контекстная оценка только ранжирует кандидатов).
//! Отличие от P5: окончательный гейт «повторное кодирование полного файла»
//! вынесен в пайплайн пользователя; здесь принимается оценка экономии.

use crate::dict::{
    varint_len, Dictionary, LANE_EXTENDED, LANE_REGULAR, LANE_SHORT, LANE_STRUCT, LANE_SYMBOL,
};
use crate::vocab::LANE_WIDTHS;

#[derive(Debug, Clone)]
pub struct TrainConfig {
    /// Символов обучения на файл (суммарно по окнам).
    pub max_train_chars: usize,
    /// Число детерминированных окон для больших файлов.
    pub windows: usize,
    /// Максимальная длина фразы в атомах (P5: 2..=15; здесь 10 для скорости).
    pub max_atoms: usize,
    /// Минимальная длина фразы-кандидата в байтах.
    pub min_phrase_bytes: usize,
    /// Минимальное число вхождений.
    pub min_count: usize,
    /// Пределы обучаемых записей по слоям [symbol, short, regular, struct, extended].
    pub caps: [usize; 5],
    /// Максимальное число кандидатов в пуле после ранжирования.
    pub candidate_cap: usize,
    /// Число слоговых хорд для short-слоя (PS14; 0 — выключить слой).
    pub syllables: usize,
}

impl Default for TrainConfig {
    fn default() -> Self {
        TrainConfig {
            max_train_chars: 1_000_000,
            windows: 16,
            max_atoms: 10,
            min_phrase_bytes: 7,
            min_count: 3,
            caps: [32, 2048, 20_000, 20_000, 10_000],
            candidate_cap: 512,
            syllables: 700,
        }
    }
}

/// Классы атомов: 0–3 — скрипты без пробелов (Han, Kana, Thai, Hangul),
/// 4 — слово/числовой прогон, 5 — одиночный знак.
const CLS_HAN: u8 = 0;
const CLS_KANA: u8 = 1;
const CLS_THAI: u8 = 2;
const CLS_HANGUL: u8 = 3;
const CLS_WORD: u8 = 4;
const CLS_PUNCT: u8 = 5;

fn script_class(c: char) -> Option<u8> {
    let u = c as u32;
    if (0x3400..=0x9FFF).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0x20000..=0x3134F).contains(&u)
    {
        Some(CLS_HAN)
    } else if (0x3040..=0x30FF).contains(&u) || (0x31F0..=0x31FF).contains(&u) {
        Some(CLS_KANA)
    } else if (0x0E00..=0x0E7F).contains(&u) {
        Some(CLS_THAI)
    } else if (0xAC00..=0xD7AF).contains(&u) {
        Some(CLS_HANGUL)
    } else {
        None
    }
}

/// Атомизация окна: байтовые отрезки с классами. Слова — прогоны букв/цифр
/// (с внутренними апострофами/дефисами); знаки — одиночные символы; скрипты
/// без пробелов — по одной кодовой точке (кандидатная сетка, не сегментация).
pub fn atomize(text: &str) -> Vec<(usize, usize, u8)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < text.len() {
        let c = match text[i..].chars().next() {
            Some(c) => c,
            None => break,
        };
        let start = i;
        i += c.len_utf8();
        if let Some(cls) = script_class(c) {
            out.push((start, i, cls));
        } else if c.is_alphanumeric() || c == '_' {
            while i < text.len() {
                let nc = match text[i..].chars().next() {
                    Some(nc) => nc,
                    None => break,
                };
                if nc.is_alphanumeric() || nc == '_' || nc == '-' || nc == '\'' || nc == '’' {
                    i += nc.len_utf8();
                } else {
                    break;
                }
            }
            out.push((start, i, CLS_WORD));
        } else if !c.is_whitespace() {
            out.push((start, i, CLS_PUNCT));
        }
        // пробелы не образуют атомов, но сохраняются внутри фразовых отрезков
    }
    out
}

struct Counts {
    phrases: std::collections::HashMap<String, u64>,
    atoms: std::collections::HashMap<String, u64>,
    bigrams: std::collections::HashMap<(String, String), u64>,
    total_bigrams: u64,
}

/// Слово целиком состоит из кириллических букв?
fn is_cyrillic_word(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| ('\u{0400}'..='\u{04FF}').contains(&c) && c.is_alphabetic())
}

/// Слогование (эвристика «максимального начала»): гласные — ядра слогов,
/// согласные между гласными уходят к следующему слогу, хвост согласных
/// присоединяется к последнему слогу. «компьютер» → «ко», «мпью», «тер».
pub fn syllabify(word: &str) -> Vec<String> {
    const VOWELS: [char; 10] = ['а', 'е', 'ё', 'и', 'о', 'у', 'ы', 'э', 'ю', 'я'];
    let chars: Vec<char> = word.chars().collect();
    let nuclei: Vec<usize> = (0..chars.len())
        .filter(|&i| VOWELS.contains(&chars[i]))
        .collect();
    if nuclei.is_empty() {
        return vec![word.to_string()];
    }
    let mut out: Vec<String> = Vec::with_capacity(nuclei.len());
    for (k, &ni) in nuclei.iter().enumerate() {
        let start = if k == 0 { 0 } else { nuclei[k - 1] + 1 };
        out.push(chars[start..=ni].iter().collect());
    }
    let tail_start = nuclei[nuclei.len() - 1] + 1;
    if tail_start < chars.len() {
        let last = out.len() - 1;
        out[last].extend(chars[tail_start..].iter());
    }
    out
}

fn count_window(text: &str, cfg: &TrainConfig, counts: &mut Counts) {
    let atoms = atomize(text);
    let n = atoms.len();
    for w in atoms.windows(2) {
        let (a, b) = (w[0], w[1]);
        let first = &text[a.0..a.1];
        let second = &text[b.0..b.1];
        *counts.bigrams.entry((first.to_string(), second.to_string())).or_insert(0) += 1;
        *counts.atoms.entry(first.to_string()).or_insert(0) += 1;
        counts.total_bigrams += 1;
    }
    if n > 0 {
        let last = atoms[n - 1];
        *counts.atoms.entry(text[last.0..last.1].to_string()).or_insert(0) += 1;
    }
    for width in 2..=cfg.max_atoms.min(n) {
        for i in 0..=(n - width) {
            let start = atoms[i].0;
            let end = atoms[i + width - 1].1;
            let phrase = &text[start..end];
            if phrase.len() < cfg.min_phrase_bytes {
                continue;
            }
            *counts.phrases.entry(phrase.to_string()).or_insert(0) += 1;
        }
    }
}

/// Суммарная PMI-поддержка предшественников первого атома кандидата
/// (упрощение P5: только ранжирование пула, никогда не гейт приёма).
fn pmi_profile(first: &str, counts: &Counts) -> f64 {
    let mut sum = 0.0f64;
    for (pair, joint) in &counts.bigrams {
        if pair.1 != first {
            continue;
        }
        let a = counts.atoms.get(&pair.0).copied().unwrap_or(1) as f64;
        let b = counts.atoms.get(&pair.1).copied().unwrap_or(1) as f64;
        let j = *joint as f64;
        let total = counts.total_bigrams.max(1) as f64;
        let ratio = j * total / (a * b);
        if ratio > 1.0 {
            sum += j * ratio.log2();
        }
    }
    sum
}

struct Candidate {
    s: String,
    lane: usize,
    score: f64,
}

/// MDL-оценка: суммарная экономия минус стоимость записи в кадре.
fn mdl_gross(count: u64, bytes: usize, lane: usize) -> f64 {
    let width = LANE_WIDTHS[lane] as f64;
    count as f64 * (bytes as f64 - width) - (varint_len(bytes as u64) + bytes) as f64
}

/// Обучение: словарь = статический базовый + обучаемые записи.
/// Невалидный UTF-8 или пустой вход возвращают базовый словарь без изменений.
pub fn train(input: &[u8], base: &Dictionary, cfg: &TrainConfig) -> Result<Dictionary, &'static str> {
    let text = match std::str::from_utf8(input) {
        Ok(t) if !t.is_empty() => t,
        _ => return Ok(clone_dictionary(base)),
    };
    // Детерминированные окна: границы не сшиваются, фразы через стык не рождаются
    let mut samples: Vec<&str> = Vec::new();
    if text.len() <= cfg.max_train_chars {
        samples.push(text);
    } else {
        let width = cfg.max_train_chars / cfg.windows.max(1);
        for k in 0..cfg.windows {
            let base_start = ((text.len() - width) * k / (cfg.windows - 1).max(1))
                .min(text.len() - width);
            let mut s = base_start;
            while !text.is_char_boundary(s) {
                s += 1;
            }
            let mut e = base_start + width;
            while !text.is_char_boundary(e) {
                e += 1;
            }
            samples.push(&text[s..e]);
        }
    }
    let mut counts = Counts {
        phrases: std::collections::HashMap::new(),
        atoms: std::collections::HashMap::new(),
        bigrams: std::collections::HashMap::new(),
        total_bigrams: 0,
    };
    for sample in &samples {
        count_window(sample, cfg, &mut counts);
    }
    // Кандидаты: одиночные атомы по слоям 1–3, фразы по слоям 4–5.
    // covered — атомы, обеспеченные словарём (база или кандидат):
    // их слоги в слоговый слой не попадают.
    let mut covered: std::collections::HashSet<String> = base
        .lanes_snapshot()
        .iter()
        .flat_map(|l| l.iter().map(|s| s.to_string()))
        .collect();
    let mut candidates: Vec<Candidate> = Vec::new();
    for (s, c) in &counts.atoms {
        if *c < cfg.min_count as u64 {
            continue;
        }
        let b = s.len();
        let chars: Vec<char> = s.chars().collect();
        let is_symbol = chars.len() == 1
            && !chars[0].is_alphanumeric()
            && script_class(chars[0]).is_none();
        let lane = if is_symbol {
            LANE_SYMBOL
        } else if b <= 6 {
            LANE_SHORT
        } else if b <= 24 {
            LANE_REGULAR
        } else {
            continue;
        };
        let gross = mdl_gross(*c, b, lane);
        if gross <= 0.0 {
            continue;
        }
        let ci = if is_symbol { 0.0 } else { pmi_profile(s, &counts) };
        let score = gross + (gross * 0.35).min(ci * 0.05);
        covered.insert(s.clone());
        candidates.push(Candidate { s: s.clone(), lane, score });
    }
    for (s, c) in &counts.phrases {
        if *c < cfg.min_count as u64 {
            continue;
        }
        let b = s.len();
        let lane = if b <= 64 { LANE_STRUCT } else { LANE_EXTENDED };
        let gross = mdl_gross(*c, b, lane);
        if gross <= 0.0 {
            continue;
        }
        let first: String = s.chars().next().unwrap_or('\0').to_string();
        let ci = pmi_profile(&first, &counts);
        let score = gross + (gross * 0.35).min(ci * 0.05);
        candidates.push(Candidate { s: s.clone(), lane, score });
    }
    candidates.sort_by(|x, y| {
        y.score
            .partial_cmp(&x.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.s.cmp(&y.s))
    });
    candidates.truncate(cfg.candidate_cap);
    // Приём кандидатов: уникальность против базового словаря и пула, пределы слоёв
    let mut trained: [Vec<String>; 5] = Default::default();
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cand in &candidates {
        if base.find(&cand.s).is_some() || !taken.insert(cand.s.clone()) {
            continue;
        }
        let lane = cand.lane;
        if trained[lane].len() >= cfg.caps[lane] {
            continue;
        }
        trained[lane].push(cand.s.clone());
    }
    // Слоговый слой (PS14): слоги несловарных кириллических слов уходят в
    // short-слой (2-байтные коды). DP-кодировщик сам выберет «слово или слоги»
    // по стоимости через trie. Слог ценен повторами между разными словами,
    // поэтому считаются все OOV-вхождения, а не только точные повторы слова.
    if cfg.syllables > 0 {
        let mut syll_counts: std::collections::HashMap<String, u64> = Default::default();
        for (s, c) in &counts.atoms {
            if covered.contains(s) || !is_cyrillic_word(s) {
                continue;
            }
            for syl in syllabify(s) {
                if syl.len() >= 4 {
                    *syll_counts.entry(syl).or_insert(0) += c;
                }
            }
        }
        let mut syl: Vec<(u64, usize, String)> = syll_counts
            .into_iter()
            .map(|(s, c)| (c, s.len(), s))
            .filter(|&(c, b, _)| mdl_gross(c, b, LANE_SHORT) > 0.0)
            .collect();
        // детерминизм: грубая сортировка по MDL-гейну, ничьи — по строке
        syl.sort_by(|x, y| {
            let gx = mdl_gross(x.0, x.1, LANE_SHORT);
            let gy = mdl_gross(y.0, y.1, LANE_SHORT);
            gy.partial_cmp(&gx)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| x.2.cmp(&y.2))
        });
        let want = cfg
            .syllables
            .min(cfg.caps[LANE_SHORT].saturating_sub(trained[LANE_SHORT].len()));
        let mut added = 0usize;
        for (_, _, s) in syl {
            if added >= want {
                break;
            }
            if base.find(&s).is_some() || !taken.insert(s.clone()) {
                continue;
            }
            trained[LANE_SHORT].push(s);
            added += 1;
        }
    }
    build_with_trained(&trained)
}

fn build_with_trained(trained: &[Vec<String>; 5]) -> Result<Dictionary, &'static str> {
    let refs: [Vec<&str>; 5] = [
        trained[0].iter().map(|s| s.as_str()).collect(),
        trained[1].iter().map(|s| s.as_str()).collect(),
        trained[2].iter().map(|s| s.as_str()).collect(),
        trained[3].iter().map(|s| s.as_str()).collect(),
        trained[4].iter().map(|s| s.as_str()).collect(),
    ];
    Dictionary::build(
        [
            crate::vocab::SYMBOL_STATIC,
            crate::vocab::SHORT_STATIC,
            crate::vocab::REGULAR_STATIC,
            crate::vocab::STRUCT_STATIC,
            crate::vocab::EXTENDED_STATIC,
        ],
        [&refs[0], &refs[1], &refs[2], &refs[3], &refs[4]],
    )
    .map_err(|_| "обучение переполнило кодовое пространство")
}

fn clone_dictionary(d: &Dictionary) -> Dictionary {
    let static_counts = d.static_counts();
    let mut trained: [Vec<String>; 5] = Default::default();
    for (lane, list) in d.lanes_snapshot().iter().enumerate() {
        trained[lane] = list[static_counts[lane]..].iter().map(|s| s.to_string()).collect();
    }
    build_with_trained(&trained).expect("клонирование корректного словаря")
}

/// Служебный отчёт: число обучаемых записей по слоям.
pub fn trained_report(dict: &Dictionary) -> [usize; 5] {
    dict.trained_counts()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn syllabify_russian_words() {
        assert_eq!(syllabify("компьютер"), vec!["ко", "мпью", "тер"]);
        assert_eq!(syllabify("молоко"), vec!["мо", "ло", "ко"]);
        assert_eq!(syllabify("дом"), vec!["дом"]);
        assert_eq!(syllabify("я"), vec!["я"]);
        assert_eq!(syllabify("всплеск"), vec!["всплеск"]);
    }

    #[test]
    fn syllable_layer_takes_oov_words() {
        // слова ниже порога min_count=3 не попадают в словный словарь,
        // но их слоги обязаны оказаться в short-слое
        let mut text = String::new();
        text.push_str("перегрузка памяти\n");
        for _ in 0..60 {
            text.push_str("обычный текст идёт здесь постоянно\n");
        }
        text.push_str("перегрузка памяти\n");
        let cfg = TrainConfig::default();
        let dict = train(text.as_bytes(), &Dictionary::static_only(), &cfg).unwrap();
        let short = dict.lane(LANE_SHORT);
        assert!(
            short.iter().any(|s| &**s == "гру" || &**s == "зка" || &**s == "руз"),
            "слоги OOV-слова должны попасть в short-слой: {:?}",
            &short[..short.len().min(20)]
        );
        // детерминизм обучения
        let dict2 = train(text.as_bytes(), &Dictionary::static_only(), &cfg).unwrap();
        assert_eq!(dict.lane(LANE_SHORT), dict2.lane(LANE_SHORT));
        // roundtrip с слоговым словарём
        let (frame, _) = codec::encode(text.as_bytes(), &dict).unwrap();
        let back = codec::decode(&frame).unwrap();
        assert_eq!(back, text.as_bytes());
    }

    #[test]
    fn syllables_off_keeps_backward_compat() {
        let mut cfg = TrainConfig::default();
        cfg.syllables = 0;
        let text = "перегрузка памяти и обычный текст".repeat(50);
        let dict = train(text.as_bytes(), &Dictionary::static_only(), &cfg).unwrap();
        let (frame, _) = codec::encode(text.as_bytes(), &dict).unwrap();
        assert_eq!(codec::decode(&frame).unwrap(), text.as_bytes());
    }
}
