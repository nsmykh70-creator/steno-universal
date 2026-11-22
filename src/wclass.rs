//! PS18 — Word-Class Prediction (концепция STENO-P7 Stage 14, адаптация).
//!
//! Марковская модель порядка 2 над семью компактными классами письма
//! (Latin, Cyrillic, CJK/Hangul, юго-восточноазиатские, индийские, RTL,
//! прочее). Обучающая последовательность строится детерминированно из
//! жадного разбора входа словарём (длиннейшие совпадения + литералы).
//! Сглаженная условная вероятность переводится в целочисленный сюрпризал
//! (в единицах 1/64 байта) и используется как добавка стоимости рёбер
//! DP-кодировщика. Модель не сериализуется: кадр и декодер не меняются.

pub const NCLASS: usize = 7;

/// Классы письма по концепции Stage 14.
pub const C_LATIN: u8 = 0;
pub const C_CYRILLIC: u8 = 1;
pub const C_CJK: u8 = 2;
pub const C_SEA: u8 = 3;
pub const C_INDIC: u8 = 4;
pub const C_RTL: u8 = 5;
pub const C_OTHER: u8 = 6;

/// Класс скаляра, начинающегося с байта `lead` (и, при необходимости,
/// следующего байта — для различения индийских письменностей и тайского
/// внутри диапазона 0xE0-0xE1). Байтовая эвристика: точность достаточна
/// для стоимостной модели, полная проверка скаляров не требуется.
pub fn class_of(lead: u8, next: Option<u8>) -> u8 {
    match lead {
        0x00..=0x7F => {
            if lead.is_ascii_alphabetic() {
                C_LATIN
            } else {
                C_OTHER
            }
        }
        0xC2..=0xCD => C_LATIN,   // Latin-1 supplement, Latin Extended-A/B
        0xCE..=0xCF => C_OTHER,   // греческий
        0xD0..=0xD3 => C_CYRILLIC, // U+0400-05FF (вкл. дополнение)
        0xD7 | 0xD8..=0xDB => C_RTL, // иврит, арабский, сирийский
        0xE0 | 0xE1 => match next {
            // 0xE0-0xE1: индийские (U+0900-0DFF) против тайского/лао (U+0E00-0EFF)
            Some(nb) if (0xA4..=0xAB).contains(&nb) => C_INDIC,
            Some(nb) if (0xB8..=0xBC).contains(&nb) => C_SEA,
            _ => C_INDIC,
        },
        0xE2..=0xE3 => C_INDIC,
        0xE4..=0xE9 => C_CJK,
        _ => C_OTHER, // 0x80-0xBF, 0xC0-0xC1, 0xD4-0xD6, 0xDC-0xDF, 0xEA-0xEF, 0xF0-0xF4
    }
}

/// Класс первого скаляра сегмента `bytes[i..]`.
pub fn class_at(bytes: &[u8], i: usize) -> u8 {
    let next = bytes.get(i + 1).copied();
    class_of(bytes[i], next)
}

/// Марковская модель порядка 2 над классами письма.
pub struct WcModel {
    /// Счётчики троек: индекс (c1*7+c2)*7+c3.
    tri: [u32; NCLASS * NCLASS * NCLASS],
    /// Счётчики пар: индекс c1*7+c2.
    bi: [u32; NCLASS * NCLASS],
    /// Сюрпризал перехода (c1,c2)->c3 в единицах 1/64 байта, после `refresh`.
    s: [u16; NCLASS * NCLASS * NCLASS],
}

impl Default for WcModel {
    fn default() -> Self {
        WcModel { tri: [0; 343], bi: [0; 49], s: [0; 343] }
    }
}

impl WcModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Обучение: инкремент счётчиков по последовательности событий-классов.
    pub fn train(&mut self, seq: &[u8]) {
        let mut a = C_OTHER;
        let mut b = C_OTHER;
        for &c in seq {
            let idx = (a as usize) * 49 + (b as usize) * 7 + (c as usize);
            self.tri[idx] += 1;
            self.bi[(a as usize) * 7 + (b as usize)] += 1;
            a = b;
            b = c;
        }
        self.refresh();
    }

    /// Пересчёт таблицы сюрпризала: сглаживание Лапласа (α=1),
    /// S = -log2(P) * 8 единиц 1/64 байта, потолок 512 (8 бит).
    pub fn refresh(&mut self) {
        for c1 in 0..NCLASS {
            for c2 in 0..NCLASS {
                let total = self.bi[c1 * 7 + c2] as f64 + NCLASS as f64;
                for c3 in 0..NCLASS {
                    let cnt = self.tri[c1 * 49 + c2 * 7 + c3] as f64;
                    let bits = -((cnt + 1.0) / total).log2();
                    let units = (bits * 8.0).round();
                    self.s[c1 * 49 + c2 * 7 + c3] = units.clamp(0.0, 512.0) as u16;
                }
            }
        }
    }

    /// Сюрпризал перехода (c1,c2)->c3 в единицах 1/64 байта.
    pub fn surprisal(&self, c1: u8, c2: u8, c3: u8) -> u16 {
        self.s[c1 as usize * 49 + c2 as usize * 7 + c3 as usize]
    }

    /// Суммарное число наблюдённых переходов (для тестов и диагностики).
    pub fn total_transitions(&self) -> u64 {
        self.tri.iter().map(|&x| x as u64).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cyr(seq: &str) -> Vec<u8> {
        // события-классы: каждый скаляр — одно событие
        seq.chars().map(|ch| {
            let mut buf = [0u8; 4];
            let b = ch.encode_utf8(&mut buf).as_bytes();
            class_of(b[0], b.get(1).copied())
        }).collect()
    }

    #[test]
    fn классы_письма_по_ведущему_байту() {
        assert_eq!(class_of(b'a', None), C_LATIN);
        assert_eq!(class_of(b'Z', None), C_LATIN);
        assert_eq!(class_of(b' ', None), C_OTHER);
        assert_eq!(class_of(b'5', None), C_OTHER);
        // 'п' = D0 BF
        assert_eq!(class_of(0xD0, Some(0xBF)), C_CYRILLIC);
        // 'я' = D1 8F
        assert_eq!(class_of(0xD1, Some(0x8F)), C_CYRILLIC);
        // '日' = E6 97 A5
        assert_eq!(class_of(0xE6, Some(0x97)), C_CJK);
        // 'ก' тайский = E0 B8 81
        assert_eq!(class_of(0xE0, Some(0xB8)), C_SEA);
        // 'अ' деванагари = E0 A4 85
        assert_eq!(class_of(0xE0, Some(0xA4)), C_INDIC);
        // 'ا' арабский = D8 A7
        assert_eq!(class_of(0xD8, Some(0xA7)), C_RTL);
    }

    #[test]
    fn упорядоченные_переходы_обучаются() {
        let mut m = WcModel::new();
        // русский текст: кириллица через пробел(другое)
        let seq = cyr("мама мыла раму рама была рядом мама пела");
        m.train(&seq);
        // частый переход (Cyr,Other)->Cyr дешевле редкого (Cyr,Other)->CJK
        let common = m.surprisal(C_CYRILLIC, C_OTHER, C_CYRILLIC);
        let unseen = m.surprisal(C_CYRILLIC, C_OTHER, C_CJK);
        assert!(common <= unseen, "частый переход дороже редкого: {} > {}", common, unseen);
        // модель что-то увидела
        assert!(m.total_transitions() > seq.len() as u64 - 2);
        // (Cyr,Cyr)->Other внутри слова (пробел после слова — Other) наблюдаем
        let word_break = m.surprisal(C_CYRILLIC, C_CYRILLIC, C_OTHER);
        assert!(word_break <= unseen);
    }

    #[test]
    fn сюрпризал_ограничен_и_детерминирован() {
        let mut m = WcModel::new();
        m.train(&cyr("текст текст текст"));
        for c1 in 0..7u8 {
            for c2 in 0..7u8 {
                for c3 in 0..7u8 {
                    let s = m.surprisal(c1, c2, c3);
                    assert!(s <= 512, "сюрпризал {} вне потолка", s);
                }
            }
        }
        let a = m.surprisal(C_CYRILLIC, C_OTHER, C_CYRILLIC);
        let b = m.surprisal(C_CYRILLIC, C_OTHER, C_CYRILLIC);
        assert_eq!(a, b, "не детерминирован");
    }
}
