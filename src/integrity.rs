//! PS21: конверт целостности SU21 вокруг архивов STENO-UV.
//!
//! Заимствовано из Stage20 (STENO-P8): CRC-64/ECMA-182 вокруг полного
//! контейнера + исходная длина файла. Декодер проверяет длину и контрольную
//! сумму декодированных байтов ДО выдачи результата, порча архива
//! детектируется, а не молча отдаёт битый файл. CRC не является
//! криптографической аутентификацией: от намеренной модификации защита —
//! отдельная задача (MAC/AEAD, вне рамок PS21).
//!
//! Формат конверта (21 байт + архив):
//! `SU21` (4) | версия u8 (1) | исходная длина u64 BE (8) | CRC-64 BE (8)

/// Магия конверта SU21.
pub const MAGIC: &[u8; 4] = b"SU21";
/// Версия формата конверта.
pub const VERSION: u8 = 1;
/// Длина заголовка конверта: магия + версия + длина + CRC-64.
pub const HEADER_LEN: usize = 4 + 1 + 8 + 8;

/// Ошибки конверта целостности.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityError {
    /// Данных меньше заголовка или оборван по середине поля.
    Truncated,
    /// Магия не SU21 (для строгого unwrap; не-wrapped данные идут мимо).
    InvalidHeader,
    /// Версия конверта выше поддерживаемой.
    UnsupportedVersion,
    /// Длина декодированных байтов не совпала с заявленной в конверте.
    LengthMismatch,
    /// CRC-64 декодированных байтов не совпал с конвертом.
    ChecksumMismatch,
}

impl std::fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntegrityError::Truncated => write!(f, "конверт оборван"),
            IntegrityError::InvalidHeader => write!(f, "магия не SU21"),
            IntegrityError::UnsupportedVersion => write!(f, "неподдерживаемая версия конверта"),
            IntegrityError::LengthMismatch => write!(f, "длина декода не совпала с конвертом"),
            IntegrityError::ChecksumMismatch => write!(f, "CRC-64 декода не совпал: архив повреждён"),
        }
    }
}

impl std::error::Error for IntegrityError {}

const fn build_crc64_table() -> [u64; 256] {
    // CRC-64/ECMA-182, полином 0x42F0_E1EB_A9EA_3693, отражение запрещено
    // (нормальный порядок, как в ECMA-182 и в тестовом векторе Stage20).
    const POLY: u64 = 0x42F0_E1EB_A9EA_3693;
    let mut table = [0u64; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = (i as u64) << 56;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & (1 << 63) != 0 {
                (crc << 1) ^ POLY
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC64_TABLE: [u64; 256] = build_crc64_table();

/// CRC-64/ECMA-182 (табличный, нормальный порядок). Тестовый вектор:
/// `crc64(b"123456789") == 0x6C40_DF5F_0B49_7347`.
pub fn crc64(data: &[u8]) -> u64 {
    let mut crc = 0u64;
    for &byte in data {
        crc = CRC64_TABLE[((crc >> 56) as usize ^ byte as usize)] ^ (crc << 8);
    }
    crc
}

/// Оборачивает готовый архив в конверт SU21. `orig` — исходные байты файла
/// (из них считаются заявленная длина и CRC-64, проверяемые при распаковке).
pub fn wrap(archive: &[u8], orig: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + archive.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(orig.len() as u64).to_be_bytes());
    out.extend_from_slice(&crc64(orig).to_be_bytes());
    out.extend_from_slice(archive);
    out
}

/// Если данные — конверт SU21 поддерживаемой версии, возвращает вложенный
/// архив; иначе None (легаси-архивы идут прежним путём, обратная совместимость).
/// Структуру заголовка не валидирует до конца — полная проверка в `verify`.
pub fn try_unwrap(data: &[u8]) -> Option<&[u8]> {
    if data.len() < HEADER_LEN {
        return None;
    }
    if data.get(..4) != Some(MAGIC.as_slice()) {
        return None;
    }
    if data[4] != VERSION {
        return None;
    }
    Some(&data[HEADER_LEN..])
}

/// Заявленная в конверте исходная длина (для ранних проверок лимитов).
pub fn declared_len(data: &[u8]) -> Result<usize, IntegrityError> {
    if data.len() < HEADER_LEN {
        return Err(IntegrityError::Truncated);
    }
    if data.get(..4) != Some(MAGIC.as_slice()) {
        return Err(IntegrityError::InvalidHeader);
    }
    if data[4] != VERSION {
        return Err(IntegrityError::UnsupportedVersion);
    }
    let expected = u64::from_be_bytes(
        data[5..13]
            .try_into()
            .map_err(|_| IntegrityError::Truncated)?,
    );
    usize::try_from(expected).map_err(|_| IntegrityError::Truncated)
}

/// Полная проверка конверта против декодированных байтов: длина и CRC-64.
/// Вызывается ПОСЛЕ декодирования вложенного архива и ДО выдачи результата.
pub fn verify(data: &[u8], decoded: &[u8]) -> Result<(), IntegrityError> {
    let expected = declared_len(data)?;
    if data.len() < HEADER_LEN {
        return Err(IntegrityError::Truncated);
    }
    let checksum = u64::from_be_bytes(
        data[13..21]
            .try_into()
            .map_err(|_| IntegrityError::Truncated)?,
    );
    if decoded.len() != expected {
        return Err(IntegrityError::LengthMismatch);
    }
    if crc64(decoded) != checksum {
        return Err(IntegrityError::ChecksumMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc64_ecma_check_vector() {
        assert_eq!(crc64(b"123456789"), 0x6C40_DF5F_0B49_7347);
    }

    #[test]
    fn crc64_empty() {
        assert_eq!(crc64(b""), 0);
    }

    #[test]
    fn wrap_unwrap_roundtrip_all_byte_values_and_empty() {
        for orig in [&b""[..], &(0..=255u8).collect::<Vec<u8>>()[..], "Русский текст ∑ 日本語".as_bytes()] {
            let wrapped = wrap("внутренний архив".as_bytes(), orig);
            assert_eq!(wrapped.len(), HEADER_LEN + "внутренний архив".len());
            let inner = try_unwrap(&wrapped).expect("конверт должен распознаваться");
            assert_eq!(inner, "внутренний архив".as_bytes());
            verify(&wrapped, orig).expect("верификация должна проходить");
        }
    }

    #[test]
    fn detects_corrupted_archive_via_decoded_mismatch() {
        let orig = "повторяющийся повторяющийся текст файла".as_bytes();
        let wrapped = wrap("архив".as_bytes(), orig);
        // архив побит порчен → декодер вернёт те же байты, кроме одного бита →
        // длина та же, CRC-64 не сойдётся
        let wrong: Vec<u8> = orig.iter().map(|b| b ^ 1).collect();
        assert_eq!(verify(&wrapped, &wrong), Err(IntegrityError::ChecksumMismatch));
    }

    #[test]
    fn detects_length_mismatch_and_truncation_and_version() {
        let wrapped = wrap("архив".as_bytes(), "исходник".as_bytes());
        let mut bad_len = wrapped.clone();
        bad_len[5] = bad_len[5].wrapping_add(1); // испортили заявленную длину
        assert_eq!(verify(&bad_len, "исходник".as_bytes()), Err(IntegrityError::LengthMismatch));
        assert_eq!(verify(&wrapped[..8], b""), Err(IntegrityError::Truncated));
        let mut bad_ver = wrapped.clone();
        bad_ver[4] = 2;
        assert_eq!(declared_len(&bad_ver), Err(IntegrityError::UnsupportedVersion));
        assert_eq!(verify(&bad_ver, "исходник".as_bytes()), Err(IntegrityError::UnsupportedVersion));
    }

    #[test]
    fn legacy_data_passes_through_unwrapped() {
        for legacy in ["CM-поток без конверта".as_bytes(), &b""[..], &[0u8, 1, 2, 3][..]] {
            assert!(try_unwrap(legacy).is_none());
        }
        // 21 байт заголовка с чужой магией не распознаётся
        let mut not_ours = [0u8; 30];
        not_ours[..4].copy_from_slice(b"S20\0");
        assert!(try_unwrap(&not_ours).is_none());
    }

    #[test]
    fn declared_len_matches_original() {
        let orig = vec![7u8; 12345];
        let wrapped = wrap("архив".as_bytes(), &orig);
        assert_eq!(declared_len(&wrapped), Ok(12345));
    }
}
