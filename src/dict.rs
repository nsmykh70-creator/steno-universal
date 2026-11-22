//! Словарь кодека: статические + обучаемые записи, серализация обучаемой части
//! кадра, кодирование/декодирование токенов, uvarint и дайджест FNV-1a.

use crate::trie::{pack_entry, Trie};

/// Метка кадра STENO-UV (STUV1).
pub const MAGIC: &[u8] = b"STUV1\x00";

pub const LANE_SYMBOL: usize = 0;
pub const LANE_SHORT: usize = 1;
pub const LANE_REGULAR: usize = 2;
pub const LANE_STRUCT: usize = 3;
pub const LANE_EXTENDED: usize = 4;

/// Названия слоёв (для отчётов и ошибок).
pub const LANE_NAMES: [&str; 5] = ["symbol", "short", "regular", "struct", "extended"];

/// Ёмкости кодовых пространств по диапазонам первого байта:
/// 0x80..0xBF (64), 0xC0..0xDF×256, 0xE0..0xEF×65536, 0xF0..0xF7×16.7M, 0xF8..0xFB×4.29G.
pub const WIRE_CAPS: [u64; 5] = [64, 8_192, 1_048_576, 134_217_728, 17_179_869_184];

/// Пределы декодера на число обучаемых записей слоя (санитарная защита кадра).
const DECODE_COUNT_CAPS: [usize; 5] = [32, 8_192, 1_048_576, 4_194_304, 4_194_304];

/// Максимальная длина одной словарной записи в кадре.
pub const ENTRY_MAX_LEN: usize = 256;

/// FNV-1a 64-bit: контроль целостности заголовка кадра (обнаружение порчи,
/// не криптография — в std нет SHA-256).
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Uvarint (LEB128): запись и чтение с проверкой границ и длины.
pub fn put_uvarint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

pub fn varint_len(v: u64) -> usize {
    let bits = if v == 0 { 0 } else { 64 - v.leading_zeros() as usize };
    ((bits + 6) / 7).clamp(1, 10)
}

/// Чтение uvarint. `Err` — усечение, слишком длинная последовательность.
pub fn get_uvarint(data: &[u8], pos: &mut usize) -> Result<u64, &'static str> {
    let mut v: u64 = 0;
    let mut shift = 0u32;
    for _ in 0..10 {
        if *pos >= data.len() {
            return Err("усечённый uvarint");
        }
        let b = data[*pos];
        *pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
    }
    Err("uvarint длиннее 10 байт")
}

/// Словарь кодека: объединённые записи по слоям (статические занимают первые
/// ID, обучаемые дописаны следом) + построенное префиксное дерево.
pub struct Dictionary {
    lanes: [Vec<Box<str>>; 5],
    static_counts: [usize; 5],
    trie: Trie,
    trained_bytes: usize,
    /// Число отброшенных дубликатов статического словаря (диагностика).
    pub deduped: usize,
}

impl Dictionary {
    /// Сборка из статических и обучаемых списков. Дубликаты по всем слоям
    /// отбрасываются (остаётся первое вхождение). Ошибка — при переполнении
    /// кодового пространства слоя.
/// Нормализация записи словаря (PS17): записи с восстанавливаемым регистром
/// хранятся в нижнем регистре (регистр восстанавливается флагами кадра
/// TAG_CAP/TAG_UPPER), смешанные и небуквенные — остаются как есть.
/// Выравнивание байтов гарантируется case_normalize.
fn norm_entry(e: &str) -> String {
    let (n, _) = crate::codec::case_normalize(e.as_bytes());
    match String::from_utf8(n) {
        Ok(s) => s,
        Err(_) => e.to_string(),
    }
}

    pub fn build(
        static_lanes: [&[&str]; 5],
        trained_lanes: [&[&str]; 5],
    ) -> Result<Dictionary, String> {
        let mut lanes: [Vec<Box<str>>; 5] = Default::default();
        let mut static_counts = [0usize; 5];
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut deduped = 0usize;
        for (lane, list) in static_lanes.iter().enumerate() {
            for s in *list {
                let ns = Self::norm_entry(s);
                if !seen.insert(ns.clone()) {
                    deduped += 1;
                    continue;
                }
                static_counts[lane] += 1;
                lanes[lane].push(ns.into_boxed_str());
            }
        }
        let mut trained_bytes = 0usize;
        for (lane, list) in trained_lanes.iter().enumerate() {
            for s in *list {
                let ns = Self::norm_entry(s);
                if !seen.insert(ns.clone()) {
                    deduped += 1;
                    continue;
                }
                if lanes[lane].len() as u64 >= WIRE_CAPS[lane] {
                    return Err(format!(
                        "слой {} переполнен: {} записей при ёмкости {}",
                        LANE_NAMES[lane],
                        lanes[lane].len(),
                        WIRE_CAPS[lane]
                    ));
                }
                let bytes = ns.as_bytes();
                trained_bytes += varint_len(bytes.len() as u64) + bytes.len();
                lanes[lane].push(ns.into_boxed_str());
            }
        }
        let mut trie = Trie::new();
        for (lane, list) in lanes.iter().enumerate() {
            for (id, s) in list.iter().enumerate() {
                trie.insert(s.as_bytes(), pack_entry(lane, id as u32));
            }
        }
        Ok(Dictionary { lanes, static_counts, trie, trained_bytes, deduped })
    }

    /// Словарь только со статическими записями (комплект поставки кодека).
    pub fn static_only() -> Dictionary {
        Dictionary::build(
            [
                crate::vocab::SYMBOL_STATIC,
                crate::vocab::SHORT_STATIC,
                crate::vocab::REGULAR_STATIC,
                crate::vocab::STRUCT_STATIC,
                crate::vocab::EXTENDED_STATIC,
            ],
            [&[]; 5],
        )
        .expect("статический словарь корректен")
    }

    pub fn lane(&self, lane: usize) -> &[Box<str>] {
        &self.lanes[lane]
    }

    /// Полный снимок слоёв (объединённые статические + обучаемые записи).
    pub fn lanes_snapshot(&self) -> &[Vec<Box<str>>; 5] {
        &self.lanes
    }

    pub fn static_counts(&self) -> [usize; 5] {
        self.static_counts
    }

    pub fn trained_counts(&self) -> [usize; 5] {
        let mut out = [0usize; 5];
        for (i, c) in self.static_counts.iter().enumerate() {
            out[i] = self.lanes[i].len() - c;
        }
        out
    }

    pub fn trained_bytes(&self) -> usize {
        self.trained_bytes
    }

    pub fn total_entries(&self) -> usize {
        self.lanes.iter().map(|l| l.len()).sum()
    }

    pub fn entry(&self, lane: usize, id: u64) -> &str {
        &self.lanes[lane][id as usize]
    }

    pub fn trie(&self) -> &Trie {
        &self.trie
    }

    /// Поиск записи (для обучения: исключение дубликатов).
    pub fn find(&self, s: &str) -> Option<(usize, u64)> {
        for (lane, list) in self.lanes.iter().enumerate() {
            if let Some(id) = list.iter().position(|x| &**x == s) {
                return Some((lane, id as u64));
            }
        }
        None
    }

    /// Плоский индекс записи (эталон fixed-3B адресует записи сквозным номером).
    pub fn flat_index(&self, lane: usize, id: u64) -> u32 {
        let mut base = 0usize;
        for l in 0..lane {
            base += self.lanes[l].len();
        }
        (base + id as usize) as u32
    }

    pub fn entry_by_flat(&self, flat: u32) -> &str {
        let mut i = flat as usize;
        for list in &self.lanes {
            if i < list.len() {
                return &list[i];
            }
            i -= list.len();
        }
        unreachable!("flat index вне словаря проверяется при декодировании")
    }

    pub fn flat_count(&self) -> u32 {
        self.total_entries() as u32
    }

    /// Кодирование словарного токена в поток (длина кода = ширина слоя).
    pub fn encode_token(&self, lane: usize, id: u64, out: &mut Vec<u8>) {
        match lane {
            LANE_SYMBOL => {
                let lead = if id < 32 { 0x80 + id as u8 } else { 0xA0 + (id - 32) as u8 };
                out.push(lead);
            }
            LANE_SHORT => {
                out.push(0xC0 + (id >> 8) as u8);
                out.push(id as u8);
            }
            LANE_REGULAR => {
                out.push(0xE0 + (id >> 16) as u8);
                out.push((id >> 8) as u8);
                out.push(id as u8);
            }
            LANE_STRUCT => {
                out.push(0xF0 + (id >> 24) as u8);
                out.push((id >> 16) as u8);
                out.push((id >> 8) as u8);
                out.push(id as u8);
            }
            _ => {
                out.push(0xF8 + (id >> 32) as u8);
                out.push((id >> 24) as u8);
                out.push((id >> 16) as u8);
                out.push((id >> 8) as u8);
                out.push(id as u8);
            }
        }
    }

    /// Длина кода токена слоя в байтах.
    pub fn token_width(lane: usize) -> usize {
        match lane {
            LANE_SYMBOL => 1,
            LANE_SHORT => 2,
            LANE_REGULAR => 3,
            LANE_STRUCT => 4,
            _ => 5,
        }
    }

    /// Сериализация обучаемой части словаря (статические записи не пишутся).
    pub fn serialize_trained_frame(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (lane, trained) in self.trained_counts().iter().enumerate() {
            put_uvarint(*trained as u64, &mut out);
            if *trained == 0 {
                continue;
            }
            let start = self.static_counts[lane];
            for s in &self.lanes[lane][start..] {
                let b = s.as_bytes();
                put_uvarint(b.len() as u64, &mut out);
                out.extend_from_slice(b);
            }
        }
        out
    }

    /// Разбор обучаемой части кадра: возвращает словарь и позицию после неё.
    pub fn parse_trained_frame(frame: &[u8], pos: &mut usize) -> Result<Dictionary, &'static str> {
        let mut trained: [Vec<String>; 5] = Default::default();
        for (lane, out) in trained.iter_mut().enumerate() {
            let count = get_uvarint(frame, pos)? as usize;
            if count > DECODE_COUNT_CAPS[lane] {
                return Err("слишком много обучаемых записей в слое");
            }
            for _ in 0..count {
                let len = get_uvarint(frame, pos)? as usize;
                if len == 0 || len > ENTRY_MAX_LEN || *pos + len > frame.len() {
                    return Err("некорректная длина словарной записи");
                }
                let bytes = &frame[*pos..*pos + len];
                *pos += len;
                match std::str::from_utf8(bytes) {
                    Ok(s) => out.push(s.to_string()),
                    Err(_) => return Err("словарная запись не является UTF-8"),
                }
            }
        }
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
        .map_err(|_| "кадр ссылается на переполненный слой")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uvarint_roundtrip() {
        for v in [0u64, 1, 127, 128, 300, 65_536, u32::MAX as u64, u64::MAX] {
            let mut buf = Vec::new();
            put_uvarint(v, &mut buf);
            let mut pos = 0;
            assert_eq!(get_uvarint(&buf, &mut pos).unwrap(), v);
            assert_eq!(pos, buf.len());
            assert_eq!(buf.len(), varint_len(v));
        }
    }

    #[test]
    fn static_dict_builds() {
        let d = Dictionary::static_only();
        assert!(d.total_entries() > 900, "слишком мало статических записей");
        assert_eq!(d.trained_counts(), [0; 5]);
    }

    #[test]
    fn token_widths_match_layers() {
        assert_eq!(Dictionary::token_width(LANE_SYMBOL), 1);
        assert_eq!(Dictionary::token_width(LANE_SHORT), 2);
        assert_eq!(Dictionary::token_width(LANE_REGULAR), 3);
        assert_eq!(Dictionary::token_width(LANE_STRUCT), 4);
        assert_eq!(Dictionary::token_width(LANE_EXTENDED), 5);
    }
}
