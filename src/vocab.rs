//! Статический словарь крейта `steno_uv`, разбитый по слоям ширин токенов.
//!
//! Слой 1 (символы, 1 Б): только знаки ≥ 2 байт UTF-8 — иначе 1-байтовый код
//! не даёт выигрыша. Слой 2 (короткие, 2 Б): служебные слова/частицы/иероглифы
//! ≥ 3 байт. Слой 3 (основной, 3 Б): слова/команды ≥ 4 байт. Слои 4–5 (брифы,
//! 4–5 Б): устойчивые конструкции ≥ 5/6 байт.
//!
//! Каждая запись — точная UTF-8 последовательность исходного текста (пробелы
//! значимы). Статические записи не сериализуются в кадре: декодер знает их.
use crate::{briefs, regular, sym_short};

pub use briefs::{EXTENDED_STATIC, STRUCT_STATIC};
pub use regular::REGULAR_STATIC;
pub use sym_short::{SHORT_STATIC, SYMBOL_STATIC};

/// Ширины кодов слоёв в байтах: символ/короткий/основной/структурный/расширенный.
pub const LANE_WIDTHS: [usize; 5] = [1, 2, 3, 4, 5];

/// Проверка статического словаря: уникальность записей и положительный выигрыш
/// против сквозной передачи байтов (длина записи строго больше ширины кода).
pub fn validate_static() -> Result<(), String> {
    let lanes: [(&str, &[&str], usize); 5] = [
        ("symbol", SYMBOL_STATIC, LANE_WIDTHS[0]),
        ("short", SHORT_STATIC, LANE_WIDTHS[1]),
        ("regular", REGULAR_STATIC, LANE_WIDTHS[2]),
        ("struct", STRUCT_STATIC, LANE_WIDTHS[3]),
        ("extended", EXTENDED_STATIC, LANE_WIDTHS[4]),
    ];
    let mut seen = std::collections::HashSet::new();
    for (name, entries, width) in lanes {
        for (i, s) in entries.iter().enumerate() {
            if !seen.insert(*s) {
                return Err(format!("слой {name}: дубликат записи №{i}: {s:?}"));
            }
            if s.len() <= width {
                return Err(format!(
                    "слой {name}: запись №{i} {s:?} ({} Б) не даёт выигрыша при коде {width} Б",
                    s.len()
                ));
            }
            if s.len() > 256 {
                return Err(format!("слой {name}: запись №{i} длиннее 256 Б"));
            }
        }
    }
    Ok(())
}
