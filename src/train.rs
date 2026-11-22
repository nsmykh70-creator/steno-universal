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
    /// Предел ONC-частей слогов (onset/coda) в short-слое (PS17; 0 — выключить).
    pub onc_parts: usize,
    /// Отделять основу от флексии частых окончаний (PS17).
    pub stem_flex: bool,
}

impl Default for TrainConfig {
    fn default() -> Self {
        TrainConfig {
            // словарь по умолчанию — 1 ГБ корпуса (PS16): файлы меньше окна
            // обучаются целиком, большие — равномерными выборками
            max_train_chars: 1 << 30,
            windows: 16,
            max_atoms: 10,
            min_phrase_bytes: 7,
            min_count: 3,
            caps: [32, 6000, 20_000, 20_000, 10_000],
            // PS19: пул кандидатов 8000 — оптимум финального размера архива:
            // кадр меньше на 309КБ (19 125 103 против 19 434 439 на fb2),
            // cmz-кадр 7 220 882 против 7 900 725 при 512/6000; дальше 8000
            // (12000/20000) кадр продолжает уменьшаться, но сжатый финал
            // ухудшается — payload мельчает и хуже моделируется
            candidate_cap: 8000,
            syllables: 700,
            // PS17: ONC-части слогов (onset/coda) и основа/флексия
            onc_parts: 400,
            stem_flex: true,
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

/// Защита памяти при корпусах до 1 ГБ: предел уникальных ключей счётчиков.
/// При переполнении новые ключи не добавляются, счёт существующих продолжается.
const MAX_UNIQUE_KEYS: usize = 4_000_000;

fn count_window(text: &str, cfg: &TrainConfig, counts: &mut Counts) {
    let atoms = atomize(text);
    let n = atoms.len();
    for w in atoms.windows(2) {
        let (a, b) = (w[0], w[1]);
        let first = &text[a.0..a.1];
        let second = &text[b.0..b.1];
        let bk = (first.to_string(), second.to_string());
        if counts.bigrams.len() < MAX_UNIQUE_KEYS || counts.bigrams.contains_key(&bk) {
            *counts.bigrams.entry(bk).or_insert(0) += 1;
        }
        let ak = first.to_string();
        if counts.atoms.len() < MAX_UNIQUE_KEYS || counts.atoms.contains_key(&ak) {
            *counts.atoms.entry(ak).or_insert(0) += 1;
        }
        counts.total_bigrams += 1;
    }
    if n > 0 {
        let last = atoms[n - 1];
        let lk = text[last.0..last.1].to_string();
        if counts.atoms.len() < MAX_UNIQUE_KEYS || counts.atoms.contains_key(&lk) {
            *counts.atoms.entry(lk).or_insert(0) += 1;
        }
    }
    for width in 2..=cfg.max_atoms.min(n) {
        for i in 0..=(n - width) {
            let start = atoms[i].0;
            let end = atoms[i + width - 1].1;
            let phrase = &text[start..end];
            if phrase.len() < cfg.min_phrase_bytes {
                continue;
            }
            // проводной лимит декодера: фразы длиннее в словарь не попадают
            if phrase.len() > crate::dict::ENTRY_MAX_LEN {
                continue;
            }
            if counts.phrases.len() >= MAX_UNIQUE_KEYS && !counts.phrases.contains_key(phrase) {
                continue;
            }
            *counts.phrases.entry(phrase.to_string()).or_insert(0) += 1;
        }
    }
}

/// Суммарная PMI-поддержка по первому атому, предвычисленная одним проходом
/// по биграммам (PS16: раньше сканировалась вся карта на каждого кандидата —
/// O(кандидаты × биграммы) не масштабировалась на корпус 1 ГБ).
fn pmi_sums(counts: &Counts) -> std::collections::HashMap<&str, f64> {
    let mut out: std::collections::HashMap<&str, f64> = Default::default();
    let total = counts.total_bigrams.max(1) as f64;
    for (pair, joint) in &counts.bigrams {
        let a = counts.atoms.get(&pair.0).copied().unwrap_or(1) as f64;
        let b = counts.atoms.get(&pair.1).copied().unwrap_or(1) as f64;
        let ratio = *joint as f64 * total / (a * b);
        if ratio > 1.0 {
            *out.entry(pair.1.as_str()).or_insert(0.0) += *joint as f64 * ratio.log2();
        }
    }
    out
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
    // PS17: обучение по регистр-нормализованной копии — счётчики «Анна»/«анна»
    // сливаются, записи словаря хранятся в нижнем регистре, регистр
    // восстанавливается флагами кадра (codec::case_normalize).
    let norm_bytes = crate::codec::case_normalize(input).0;
    let text = match std::str::from_utf8(&norm_bytes) {
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
    let psums = pmi_sums(&counts);
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
        let ci = if is_symbol { 0.0 } else { psums.get(s.as_str()).copied().unwrap_or(0.0) };
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
        let ci = psums.get(first.as_str()).copied().unwrap_or(0.0);
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
    // Параллельно считаются ONC-части слогов (PS17): onset/coda собирают
    // OOV-слоги, которых нет в слоговом словаре, без «дыр» словаря.
    let mut onc_counts: std::collections::HashMap<String, u64> = Default::default();
    if cfg.syllables > 0 {
        let mut syll_counts: std::collections::HashMap<String, u64> = Default::default();
        for (s, c) in &counts.atoms {
            if covered.contains(s) || !is_cyrillic_word(s) {
                continue;
            }
            let sylls = syllabify(s);
            for syl in &sylls {
                // слог = слово с одним гласным ядром может быть огромным
                // (длинные прогоны без пробелов) — проводной лимит декодера
                if syl.len() >= 4 && syl.len() <= crate::dict::ENTRY_MAX_LEN {
                    *syll_counts.entry(syl.clone()).or_insert(0) += c;
                }
            }
            if cfg.onc_parts > 0 {
                for syl in &sylls {
                    for part in onc_parts(syl) {
                        // 1-буквенные части не дают выигрыша против RAW
                        if part.len() >= 4 && part.len() <= crate::dict::ENTRY_MAX_LEN {
                            *onc_counts.entry(part).or_insert(0) += c;
                        }
                    }
                }
            }
        }
        let uniq_syll_count = syll_counts.len();
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
        eprintln!(
            "syll-debug: уникальных слогов с len>=4: {}, прошедших MDL: {}, want: {}",
            uniq_syll_count,
            syl.len(),
            want
        );
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
        // ONC-части (PS17): заполняют остаток short-лимита после целых слогов
        if cfg.onc_parts > 0 {
            let mut onc: Vec<(u64, usize, String)> = onc_counts
                .into_iter()
                .map(|(s, c)| (c, s.len(), s))
                .filter(|&(c, b, _)| mdl_gross(c, b, LANE_SHORT) > 0.0)
                .collect();
            onc.sort_by(|x, y| {
                let gx = mdl_gross(x.0, x.1, LANE_SHORT);
                let gy = mdl_gross(y.0, y.1, LANE_SHORT);
                gy.partial_cmp(&gx)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| x.2.cmp(&y.2))
            });
            let want_onc =
                cfg.onc_parts.min(cfg.caps[LANE_SHORT].saturating_sub(trained[LANE_SHORT].len()));
            let mut added_onc = 0usize;
            for (_, _, s) in onc {
                if added_onc >= want_onc {
                    break;
                }
                if base.find(&s).is_some() || !taken.insert(s.clone()) {
                    continue;
                }
                trained[LANE_SHORT].push(s);
                added_onc += 1;
            }
        }
    }
    // Основа/флексия (PS17): у OOV-слов отбрасываются частые русские
    // окончания; основы идут в regular-слой (3-байтовые коды), окончания —
    // в short (2-байтовые). DP через trie сам соберёт «основа+окончание».
    if cfg.stem_flex {
        let mut stem_counts: std::collections::HashMap<String, u64> = Default::default();
        let mut end_counts: std::collections::HashMap<String, u64> = Default::default();
        for (s, c) in &counts.atoms {
            if covered.contains(s) || !is_cyrillic_word(s) {
                continue;
            }
            let b = s.len();
            // короче 8 байт — зона слов и слогов; длиннее 24 — прогоны
            if b < 8 || b > 24 {
                continue;
            }
            if let Some((stem, end)) = strip_ending(s) {
                *stem_counts.entry(stem).or_insert(0) += c;
                *end_counts.entry(end).or_insert(0) += c;
            }
        }
        let mut ends: Vec<(u64, usize, String)> = end_counts
            .into_iter()
            .map(|(s, c)| (c, s.len(), s))
            .filter(|&(c, b, _)| mdl_gross(c, b, LANE_SHORT) > 0.0)
            .collect();
        ends.sort_by(|x, y| {
            let gx = mdl_gross(x.0, x.1, LANE_SHORT);
            let gy = mdl_gross(y.0, y.1, LANE_SHORT);
            gy.partial_cmp(&gx)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| x.2.cmp(&y.2))
        });
        let want_end = cfg.caps[LANE_SHORT].saturating_sub(trained[LANE_SHORT].len());
        let mut added_end = 0usize;
        for (_, _, s) in ends {
            if added_end >= want_end {
                break;
            }
            if base.find(&s).is_some() || !taken.insert(s.clone()) {
                continue;
            }
            trained[LANE_SHORT].push(s);
            added_end += 1;
        }
        let mut stems: Vec<(u64, usize, String)> = stem_counts
            .into_iter()
            .map(|(s, c)| (c, s.len(), s))
            .filter(|&(c, b, _)| mdl_gross(c, b, LANE_REGULAR) > 0.0)
            .collect();
        stems.sort_by(|x, y| {
            let gx = mdl_gross(x.0, x.1, LANE_REGULAR);
            let gy = mdl_gross(y.0, y.1, LANE_REGULAR);
            gy.partial_cmp(&gx)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| x.2.cmp(&y.2))
        });
        let want_stem = cfg.caps[LANE_REGULAR].saturating_sub(trained[LANE_REGULAR].len());
        let mut added_stem = 0usize;
        for (_, _, s) in stems {
            if added_stem >= want_stem {
                break;
            }
            if base.find(&s).is_some() || !taken.insert(s.clone()) {
                continue;
            }
            trained[LANE_REGULAR].push(s);
            added_stem += 1;
        }
    }
    build_with_trained(&trained)
}

/// Гласные русского языка (общие для syllabify и onc_parts).
const VOWELS: [char; 10] = ['а', 'е', 'ё', 'и', 'о', 'у', 'ы', 'э', 'ю', 'я'];

/// ONC-части слога (PS17): согласный кластер до гласного ядра (onset) и
/// после него (coda). Части короче двух букв не дают выигрыша против
/// RAW-байтов и не возвращаются; ядро-одиночка тоже.
pub fn onc_parts(syl: &str) -> Vec<String> {
    let chars: Vec<char> = syl.chars().collect();
    let mut out = Vec::new();
    if let Some(k) = chars.iter().position(|&c| VOWELS.contains(&c)) {
        let onset: String = chars[..k].iter().collect();
        let coda: String = chars[k + 1..].iter().collect();
        if onset.chars().count() >= 2 {
            out.push(onset);
        }
        if coda.chars().count() >= 2 {
            out.push(coda);
        }
    }
    out
}

/// Частые русские окончания для отбрасывания флексии (PS17).
const RU_ENDINGS: [&str; 51] = [
    "ами", "ями", "ого", "его", "ому", "ему", "ыми", "ими",
    "ая", "яя", "ое", "ее", "ые", "ие", "ий", "ый", "ой", "ей",
    "ом", "ем", "ах", "ях", "ам", "ям", "ов", "ев", "ою", "ею",
    "ую", "юю", "ых", "их", "ье", "ья", "ью", "ся", "сь",
    "а", "я", "у", "ю", "е", "о", "и", "ы", "ь", "й",
    "ет", "ют", "ат", "ит",
];

/// Отбрасывание окончания: длинные приоритетнее, основа — не короче трёх букв.
/// Возвращает (основа, окончание).
pub fn strip_ending(word: &str) -> Option<(String, String)> {
    let mut best: Option<(usize, &str)> = None;
    for e in RU_ENDINGS.iter() {
        if word.ends_with(e) {
            let el = e.chars().count();
            let wl = word.chars().count();
            if wl - el >= 3 && best.map(|(bel, _)| el > bel).unwrap_or(true) {
                best = Some((el, e));
            }
        }
    }
    let (_, e) = best?;
    let stem_len = word.len() - e.len();
    Some((word[..stem_len].to_string(), e.to_string()))
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

    #[test]
    fn phrases_over_wire_limit_are_rejected_and_decode_survives() {
        // фраза из 12 слов по 25 букв (~324 Б) > ENTRY_MAX_LEN(256):
        // обучение обязано её отфильтровать, кадр обязан декодироваться
        let word = "а".repeat(25);
        let phrase = std::iter::repeat(&word[..])
            .take(12)
            .collect::<Vec<_>>()
            .join(" ");
        let text = format!("{}\n", phrase).repeat(30);
        let dict = train(text.as_bytes(), &Dictionary::static_only(), &TrainConfig::default()).unwrap();
        for lane in dict.lanes_snapshot().iter() {
            for s in lane.iter() {
                assert!(s.len() <= crate::dict::ENTRY_MAX_LEN);
            }
        }
        let (frame, _) = codec::encode(text.as_bytes(), &dict).unwrap();
        assert_eq!(codec::decode(&frame).unwrap(), text.as_bytes());
    }

    #[test]
    fn long_single_nucleus_run_is_filtered() {
        // прогон из 300 согласных + «а»: один слог = весь прогон (>256 Б),
        // обучение обязано его отфильтровать по проводному лимиту
        let run = format!("{}а", "б".repeat(300));
        let text = format!("{} хвост текст\n", run).repeat(5);
        let dict = train(text.as_bytes(), &Dictionary::static_only(), &TrainConfig::default()).unwrap();
        for lane in dict.lanes_snapshot().iter() {
            for s in lane.iter() {
                assert!(s.len() <= crate::dict::ENTRY_MAX_LEN);
            }
        }
        let (frame, _) = codec::encode(text.as_bytes(), &dict).unwrap();
        assert_eq!(codec::decode(&frame).unwrap(), text.as_bytes());
    }
}
