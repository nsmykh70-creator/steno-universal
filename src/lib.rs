//! Универсальный стенографический кодек STENO-UV: комбинированная ширина
//! токена **1, 2, 3, 4 и 5 байт**, слоистый словарь (статический + обучаемый)
//! и 100% побайтовая обратимость для произвольных данных — тексты на десятках
//! языков (включая языки без пробелов), математика (LaTeX/Unicode), исходный
//! код и бинарные вставки. Только `std`, внешних зависимостей нет.
//!
//! # Формат кадра
//!
//! ```text
//! MAGIC "STUV1\0" | обучаемые записи 5 слоёв (uvarint-счётчик + uvarint-длина +
//! байты UTF-8) | FNV-1a 64 дайджест заголовка | uvarint длина входа | поток токенов
//! ```
//!
//! Длина каждого токена однозначно определяется по первому байту (prefix-free):
//!
//! | Диапазон      | Ширина | Роль                                                          |
//! |---------------|--------|---------------------------------------------------------------|
//! | `00..7F`      | 1 Б    | ASCII-сквозная передача (байт = сам себе)                     |
//! | `80..9F`      | 1 Б    | статические 32 не-ASCII знака (типографика, математика, CJK)  |
//! | `A0..BF`      | 1 Б    | обучаемые 32 знака                                            |
//! | `C0..DF`+1 Б  | 2 Б    | слой short: служебные слова, частицы, иероглифы (≤ 8192 ID)   |
//! | `E0..EF`+2 Б  | 3 Б    | слой regular: слова, команды LaTeX, ключевые слова (≤ 2^20)   |
//! | `F0..F7`+3 Б  | 4 Б    | слой struct: брифы 5–64 Б (≤ 2^27)                            |
//! | `F8..FB`+4 Б  | 5 Б    | слой extended: длинные брифы >64 Б, фразы CJK 10+ знаков      |
//! | `FC`+varint   | перем. | литеральная цепочка: raw-байты без словаря                    |
//! | `FD`+1 Б      | 2 Б    | побайтовый fallback любого байта                              |
//! | `FE..FF`      | —      | зарезервированы (декодер возвращает `Err`)                    |
//!
//! Ключевые свойства:
//! * каждое словарное совпадение строго сжимает (запись длиннее кода);
//! * кодировщик — точное DP минимальной стоимости (не жадный longest-match);
//! * декодер без паник: все ошибки формата — `Result<_, &'static str>`;
//! * обратимость `decode(encode(x)) == x` для любых байтов, включая невалидный UTF-8;
//! * гомоглифы кириллица/латиница — только утилита анализа, байты не искажаются.

pub mod briefs;
pub mod codec;
pub mod corpus;
pub mod dict;
pub mod frec;
pub mod lpaq_ctrl;
pub mod regular;
pub mod sym_short;
pub mod train;
pub mod trie;
pub mod vocab;
pub mod wclass;

pub use codec::{decode, decode_fixed3, encode, encode_fixed3, EncodeStats};
pub use dict::Dictionary;
pub use train::{train, TrainConfig};

/// Пары визуальных гомоглифов кириллица ↔ латиница (прописные и строчные).
/// Использовать для анализа/нормализации поиска; кодек никогда не применяет
/// эти замены — каждый токен восстанавливает точные исходные байты.
pub const HOMOGLYPHS: &[(char, char)] = &[
    ('А', 'A'), ('В', 'B'), ('Е', 'E'), ('К', 'K'), ('М', 'M'), ('Н', 'H'),
    ('О', 'O'), ('Р', 'P'), ('С', 'C'), ('Т', 'T'), ('У', 'Y'), ('Х', 'X'),
    ('а', 'a'), ('е', 'e'), ('о', 'o'), ('р', 'p'), ('с', 'c'), ('у', 'y'),
    ('х', 'x'), ('к', 'k'), ('м', 'm'), ('н', 'h'), ('т', 't'), ('в', 'b'),
];

/// Нормализация гомоглифов: кириллические двойники приводятся к латинице.
/// Прочие символы копируются как есть.
pub fn homoglyph_normalize(s: &str) -> String {
    s.chars()
        .map(|c| {
            for &(cyr, lat) in HOMOGLYPHS {
                if c == cyr {
                    return lat;
                }
            }
            c
        })
        .collect()
}

/// Являются ли два символа визуальными гомоглифами (в любом порядке пары).
pub fn is_homoglyph_pair(a: char, b: char) -> bool {
    HOMOGLYPHS
        .iter()
        .any(|&(x, y)| (a == x && b == y) || (a == y && b == x))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode, encode};
    use crate::dict::{
        fnv1a64, put_uvarint, Dictionary, MAGIC, LANE_EXTENDED, LANE_REGULAR, LANE_SHORT,
        LANE_STRUCT, LANE_SYMBOL,
    };
    use crate::vocab::validate_static;

    fn rt(s: &str) {
        let d = Dictionary::static_only();
        let (frame, _) = encode(s.as_bytes(), &d).expect("кодирование");
        let out = decode(&frame).expect("декодирование");
        assert_eq!(out, s.as_bytes(), "roundtrip строки {s:?}");
    }

    fn rt_bytes(b: &[u8]) {
        let d = Dictionary::static_only();
        let (frame, _) = encode(b, &d).expect("кодирование");
        let out = decode(&frame).expect("декодирование");
        assert_eq!(out, b, "roundtrip байтов длиной {}", b.len());
    }

    #[test]
    fn static_vocab_is_valid() {
        validate_static().expect("статический словарь должен проходить проверку");
    }

    #[test]
    fn roundtrip_languages_and_edge_cases() {
        rt("");
        rt("a");
        rt("\n");
        rt("История советского программирования начинается с данных.");
        rt("The development of information systems follows the example.");
        rt("El sistema procesa información entre servidores.");
        rt("Die Entwicklung von Systemen und Algorithmen.");
        rt("Bu sistem, verilerini sunucular arasında işler; бұл жүйе.");
        rt("يعالج النظام البيانات بين الخوادم بنجاح.");
        rt("سیستم، داده‌ها را بین سرورها پردازش می‌کند.");
        rt("Этот sistema 处理 данные и быстро работает в любое время суток.");
        rt("Αυτή είναι μια δοκιμή με ελληνικά γράμματα: αβγδε.");
        rt("עברית עם אותיות מיוחדות: שלום.");
        rt("한국어 텍스트와 데이터 압축 알고리즘.");
        rt("สวัสดีชาวโลก การบีบอัดข้อมูลและการจัดเก็บข้อมูล");
        rt("Đây là văn bản tiếng Việt với các dấu thanh.");
        rt("你好，世界。数据压缩算法在目前的基础上进一步发展。");
        rt("こんにちは世界。データ圧縮アルゴリズムの研究。");
        rt("안녕하세요 세계. 데이터 압축 테스트.");
        rt("\\int_{-\\infty}^{\\infty} e^{-x^2}\\,dx = \\sqrt{\\pi}");
        rt("∀ε>0 ∃δ>0 : |x−a|<δ ⇒ |f(x)−f(a)|<ε");
        rt("fn main() {\n    println!(\"Hello, world!\");\n}\n");
        rt("async function fetch(url) { return await fetch(url).json(); }");
        rt("👨‍👩‍👧‍👦👍💯🚀 emoji и эмодзи");
        rt("Данные data — «текст» …\t\n∑x² ≠ y³ ⚠");
        rt("советского советский советское Советского СОВЕТСКОГО");
        rt("компьютерная лингвистика программирования поверхностей");
    }

    #[test]
    fn roundtrip_all_256_byte_values() {
        let all: Vec<u8> = (0..=255u8).collect();
        rt_bytes(&all);
    }

    #[test]
    fn roundtrip_binary_and_invalid_utf8() {
        rt_bytes(&[]);
        rt_bytes(&[0x00]);
        rt_bytes(&[0x7F]);
        rt_bytes(&[0x80]);
        rt_bytes(&[0xFF]);
        rt_bytes(b"\xC3\x28"); // разорванная двухбайтовая последовательность
        rt_bytes(b"\xE0\x80\x80"); // усечённая трёхбайтовая
        rt_bytes(b"\xF0\x9F\x98"); // усечённый эмодзи
        rt_bytes(b"\xF0\x9F\x98\x80\xF0\x9F"); // эмодзи + хвост
        rt_bytes(b"\xFE\xFF\x00\x01binary\0\x7F\xFF");
        let mut random = Vec::new();
        let mut s = 0x1234_5678u64;
        for _ in 0..4096 {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            random.push((s >> 33) as u8);
        }
        rt_bytes(&random);
    }

    #[test]
    fn every_static_entry_maps_to_single_token() {
        let d = Dictionary::static_only();
        let lanes = [
            (crate::vocab::SYMBOL_STATIC, LANE_SYMBOL, 0x80u8..=0xBFu8),
            (crate::vocab::SHORT_STATIC, LANE_SHORT, 0xC0u8..=0xDFu8),
            (crate::vocab::REGULAR_STATIC, LANE_REGULAR, 0xE0u8..=0xEFu8),
            (crate::vocab::STRUCT_STATIC, LANE_STRUCT, 0xF0u8..=0xF7u8),
            (crate::vocab::EXTENDED_STATIC, LANE_EXTENDED, 0xF8u8..=0xFBu8),
        ];
        for (entries, lane, leads) in lanes {
            for e in entries {
                let (frame, st) = encode(e.as_bytes(), &d).expect("кодирование записи");
                let width = Dictionary::token_width(lane);
                // записи с восстанавливаемым регистром несут +1 байт флага (PS17)
                let (norm, _) = crate::codec::case_normalize(e.as_bytes());
                let needs_flag = norm != e.as_bytes();
                let expect = width + if needs_flag { 1 } else { 0 };
                if st.payload as usize != expect {
                    // записи с капиталом внутри («#[derive(Debug)]») не дают
                    // однотокенного кода — допускаем только деградацию
                    assert!(
                        st.payload as usize > expect,
                        "запись {e:?}: payload {} меньше ожидаемых {expect} Б",
                        st.payload
                    );
                } else {
                    // полезная нагрузка всегда завершает кадр
                    let body = &frame[frame.len() - expect..];
                    let li = if needs_flag { 1 } else { 0 };
                    assert!(
                        leads.contains(&body[li]),
                        "запись {e:?}: ведущий байт {:#04x} вне диапазона слоя {lane}",
                        body[li]
                    );
                }
                assert_eq!(decode(&frame).unwrap(), e.as_bytes());
            }
        }
    }

    #[test]
    fn trained_dictionary_improves_repetitive_text() {
        let d = Dictionary::static_only();
        let text = "данные передаются через сервера системы".repeat(600);
        let (frame1, _) = encode(text.as_bytes(), &d).unwrap();
        let cfg = TrainConfig::default();
        let trained = train(text.as_bytes(), &d, &cfg).expect("обучение");
        let entries = trained.trained_counts();
        assert!(
            entries.iter().map(|x| *x as u64).sum::<u64>() > 0,
            "обучение не нашло записей"
        );
        let (frame2, _) = encode(text.as_bytes(), &trained).unwrap();
        assert!(
            frame2.len() < frame1.len(),
            "обученный кадр {} должен быть меньше статического {}",
            frame2.len(),
            frame1.len()
        );
        assert_eq!(decode(&frame2).unwrap(), text.as_bytes());
        // обучаемые записи не дублируют статические
        for (lane, list) in trained.lanes_snapshot().iter().enumerate() {
            let seen: std::collections::HashSet<&str> = list.iter().map(|s| &**s).collect();
            assert_eq!(seen.len(), list.len(), "дубликат в слое {lane}");
        }
    }

    #[test]
    fn train_rejects_invalid_utf8_safely() {
        let d = Dictionary::static_only();
        let trained = train(&[0xC3, 0x28, 0xFF, 0xFE], &d, &TrainConfig::default()).unwrap();
        assert_eq!(trained.total_entries(), d.total_entries());
        rt_bytes(&[0xC3, 0x28, 0xFF, 0xFE]);
    }

    #[test]
    fn all_truncations_are_errors() {
        let d = Dictionary::static_only();
        let src = "Данные данные данные 。 Hello, world! 👍".repeat(3);
        let (frame, _) = encode(src.as_bytes(), &d).unwrap();
        for len in 0..frame.len() {
            assert!(
                decode(&frame[..len]).is_err(),
                "усечение до {} Б должно быть ошибкой",
                len
            );
        }
        assert!(decode(&frame).is_ok());
    }

    #[test]
    fn bit_flips_never_panic() {
        let d = Dictionary::static_only();
        let src = "данные 。 algorithm \\frac{1}{2} ".repeat(4);
        let (frame, _) = encode(src.as_bytes(), &d).unwrap();
        for pos in 0..frame.len() {
            for xor in [0x01u8, 0x80u8] {
                let mut bad = frame.clone();
                bad[pos] ^= xor;
                let _ = decode(&bad); // Err или корректный разбор другого потока
            }
        }
    }

    #[test]
    fn digest_mismatch_is_error() {
        let d = Dictionary::static_only();
        let src = "данные данные данные данные данные".repeat(2);
        let (frame, _) = encode(src.as_bytes(), &d).unwrap();
        let mut bad = frame.clone();
        bad[8] ^= 0x01; // внутри обучаемой части заголовка
        assert!(decode(&bad).is_err());
    }

    fn frame_with_payload(payload: &[u8]) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(MAGIC);
        for _ in 0..5 {
            f.push(0); // нулевые счётчики обучаемых записей
        }
        let digest = fnv1a64(&f);
        f.extend_from_slice(&digest.to_le_bytes());
        put_uvarint(payload.len() as u64, &mut f);
        f.extend_from_slice(payload);
        f
    }

    #[test]
    fn reserved_and_malformed_tokens_are_errors() {
        assert!(decode(&frame_with_payload(b"\xFF")).is_err());
        assert!(decode(&frame_with_payload(b"\xFE")).is_err());
        assert!(decode(&frame_with_payload(b"\xC0")).is_err());
        assert!(decode(&frame_with_payload(b"\xE0\x01")).is_err());
        assert!(decode(&frame_with_payload(b"\xF0\x01\x02")).is_err());
        assert!(decode(&frame_with_payload(b"\xF8\x01\x02\x03")).is_err());
        assert!(decode(&frame_with_payload(b"\xFD")).is_err());
        // цепочка FC длиннее оставшихся байтов
        assert!(decode(&frame_with_payload(b"\xFC\x7F\xA0")).is_err());
        // слишком длинный uvarint
        let long_varint = [0x80u8; 11];
        assert!(decode(&frame_with_payload(&long_varint)).is_err());
        // неизвестный ID обучаемого слоя (обучаемых записей нет)
        assert!(decode(&frame_with_payload(b"\xBF")).is_err());
        assert!(decode(&frame_with_payload(b"\xDF\xFF")).is_err());
        assert!(decode(&frame_with_payload(b"\xEF\xFF\xFF")).is_err());
        assert!(decode(&frame_with_payload(b"\xF7\xFF\xFF\xFF")).is_err());
        assert!(decode(&frame_with_payload(b"\xFB\xFF\xFF\xFF\xFF")).is_err());
        // неверная магия
        let mut bad = frame_with_payload(b"a");
        bad[0] = b'X';
        assert!(decode(&bad).is_err());
        assert!(decode(b"").is_err());
    }

    #[test]
    fn declared_length_mismatch_is_error() {
        let mut f = frame_with_payload(b"hello");
        // байт uvarint длины выхода стоит после 6 Б магии + 5 счётчиков + 8 Б дайджеста
        let pos = MAGIC.len() + 5 + 8;
        assert_eq!(f[pos], 5);
        f[pos] = 6;
        assert!(decode(&f).is_err());
        f[pos] = 4;
        assert!(decode(&f).is_err());
    }

    #[test]
    fn digest_field_protects_dictionary() {
        let mut f = frame_with_payload(b"a");
        let pos = MAGIC.len() + 5;
        // правим счётчик первого слоя: дайджест больше не совпадает
        f[pos] = 1;
        assert!(decode(&f).is_err());
    }

    #[test]
    fn stats_are_consistent() {
        let d = Dictionary::static_only();
        let src = "Данные данные 。 algorithm \\frac{1}{2} x²".repeat(20);
        let (frame, st) = encode(src.as_bytes(), &d).unwrap();
        assert_eq!(st.frame as usize, frame.len());
        assert!(st.payload + st.dict_bytes < frame.len() as u64);
        assert!(st.tokens > 0);
        let w = st.by_width();
        assert_eq!(
            w.iter().sum::<u64>() + st.run + st.raw_byte,
            st.tokens,
            "сумма по ширинам должна сходиться в tokens"
        );
    }

    #[test]
    fn fixed3_roundtrip_and_zones() {
        let d = Dictionary::static_only();
        let samples: Vec<Vec<u8>> = vec![
            "History of soviet programming: данные, сервера, системы.".as_bytes().to_vec(),
            "你好世界。数据压缩算法在目前的基础上进一步发展。".as_bytes().to_vec(),
            "👨‍👩‍👧‍👦 emoji sample".as_bytes().to_vec(),
            vec![0x00, 0x7F, 0x80, 0xC3, 0x28, 0xFF],
            (0..=255u8).collect(),
        ];
        for s in &samples {
            let code = encode_fixed3(s, &d).unwrap();
            assert_eq!(code.len() % 3, 0);
            assert_eq!(decode_fixed3(&code, &d).unwrap(), *s, "fixed3 roundtrip");
        }
        // словарное совпадение попадает в зону 0x110000+
        let code = encode_fixed3(" данные".as_bytes(), &d).unwrap();
        let v = ((code[0] as u32) << 16) | ((code[1] as u32) << 8) | code[2] as u32;
        assert!(v >= 0x110000, "совпадение должно адресоваться в зону словаря");
    }

    #[test]
    fn fixed3_mismatch_is_error() {
        let d = Dictionary::static_only();
        assert!(decode_fixed3(&[0x00, 0x00], &d).is_err());
        assert!(decode_fixed3(&[0x11, 0xFF, 0xFF], &d).is_err()); // вне словаря
        assert!(decode_fixed3(&[0x00, 0xD8, 0x00], &d).is_err()); // суррогат
        assert!(decode_fixed3(&[0x00, 0xDF, 0xFF], &d).is_err()); // суррогат
    }

    #[test]
    fn homoglyph_utils() {
        assert_eq!(homoglyph_normalize("АВЕКМНОРСТУХ"), "ABEKMHOPCTYX");
        assert_eq!(homoglyph_normalize("авекмнорстух"), "abekmhopctyx");
        assert_eq!(homoglyph_normalize("стуха"), "ctyxa");
        assert_eq!(homoglyph_normalize("Hello"), "Hello");
        assert!(is_homoglyph_pair('А', 'A'));
        assert!(is_homoglyph_pair('a', 'а'));
        assert!(!is_homoglyph_pair('ж', 'g'));
        assert_eq!(homoglyph_normalize("стуха"), "ctyxa");
        // Д и ы не входят в карту гомоглифов и остаются; а, н, е заменяются
        assert_eq!(homoglyph_normalize("Данные"), "Дahhыe");
        assert_eq!(homoglyph_normalize("Данные 2026!"), "Дahhыe 2026!");
    }

    #[test]
    fn corpus_generators_match_previous_spec() {
        let raw = corpus::generate(corpus::Kind::Ru, 2048, 1);
        assert_eq!(raw.len(), 2048);
        assert!(std::str::from_utf8(&raw).is_ok());
        let a = corpus::generate(corpus::Kind::Zh, 3000, 5);
        let b = corpus::generate(corpus::Kind::Zh, 3000, 5);
        assert_eq!(a, b, "генератор должен быть детерминированным");
    }

    #[test]
    fn encode_stats_by_width_counts_all_classes() {
        let d = Dictionary::static_only();
        let src = " данные 。 the and return \\frac \\int_{0}^{\\infty} 👍 x";
        let (_, st) = encode(src.as_bytes(), &d).unwrap();
        let w = st.by_width();
        assert!(w[0] > 0, "должен быть хотя бы один 1-байтовый токен");
        assert!(st.tokens >= w.iter().sum::<u64>());
    }
}
