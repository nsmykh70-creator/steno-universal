//! Кодек STENO-UV: точный DP-кодировщик, защищённый декодер, эталон
//! фиксированных 3-байтовых токенов (fixed-3B) и статистика распределения
//! ширин токенов.
//!
//! Гарантии: однозначное определение длины токена по первому байту; отсутствие
//! паник; все ошибки формата — `Result<_, &'static str>`; побайтовая
//! обратимость для произвольных байтовых последовательностей (не только UTF-8).

use crate::dict::{
    get_uvarint, put_uvarint, varint_len, Dictionary, MAGIC, LANE_EXTENDED, LANE_REGULAR,
    LANE_SHORT, LANE_STRUCT, LANE_SYMBOL,
};
use crate::trie::unpack_entry;

/// Максимальная длина литеральной цепочки в DP (ограничивает перебор).
const RUN_MAX: usize = 32;

/// Теги первого байта.
const TAG_RUN: u8 = 0xFC;
const TAG_RAW: u8 = 0xFD;

/// Коды операций DP (старшие 3 бита упакованного слова).
const OP_ASCII: u32 = 0;
const OP_TOKEN: u32 = 1; // +1 == lane (1..=5)
const OP_RUN: u32 = 6;
const OP_RAW: u32 = 7;

const ARG_MASK: u32 = 0x1FFF_FFFF;

/// Статистика кодирования одного кадра.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncodeStats {
    pub ascii: u64,
    pub symbol: u64,
    pub short: u64,
    pub regular: u64,
    pub struct_: u64,
    pub extended: u64,
    pub run: u64,
    pub run_bytes: u64,
    pub raw_byte: u64,
    pub payload: u64,
    pub dict_bytes: u64,
    pub frame: u64,
    pub tokens: u64,
}

impl EncodeStats {
    /// Число токенов конкретной ширины: [1Б, 2Б, 3Б, 4Б, 5Б].
    pub fn by_width(&self) -> [u64; 5] {
        [self.ascii + self.symbol, self.short, self.regular, self.struct_, self.extended]
    }
}

fn pack(op: u32, arg: u32) -> u32 {
    (op << 29) | (arg & ARG_MASK)
}

/// Точный кодировщик: динамическое программирование минимальной стоимости по
/// байтовым позициям (ASCII-сквозная передача, словарные токены, литеральные
/// цепочки, побайтовый RAW). Возвращает кадр и статистику.
pub fn encode(input: &[u8], dict: &Dictionary) -> Result<(Vec<u8>, EncodeStats), &'static str> {
    let n = input.len();
    if n > u32::MAX as usize / 4 {
        return Err("вход слишком велик для DP-кодировщика");
    }
    let mut cost = vec![u32::MAX; n + 1];
    let mut ops = vec![0u32; n + 1];
    cost[n] = 0;
    let trie = dict.trie();
    for i in (0..n).rev() {
        let b = input[i];
        let mut best = u32::MAX;
        let mut best_op = 0u32;
        if b < 0x80 {
            if cost[i + 1] != u32::MAX {
                best = 1 + cost[i + 1];
                best_op = pack(OP_ASCII, 0);
            }
        } else if cost[i + 1] != u32::MAX {
            // одиночный RAW-байт дешевле литеральной цепочки из одного байта
            best = 2 + cost[i + 1];
            best_op = pack(OP_RAW, 0);
        }
        if b >= 0x80 {
            // литеральная цепочка выгодна с 3 байт (1 и 2 байта проигрывают RAW)
            let max_l = RUN_MAX.min(n - i);
            for l in 3..=max_l {
                let next = cost[i + l];
                if next == u32::MAX {
                    continue;
                }
                let c = 1 + varint_len(l as u64) as u32 + l as u32 + next;
                if c < best {
                    best = c;
                    best_op = pack(OP_RUN, l as u32);
                }
            }
        }
        for (len, entry) in trie.matches_at(input, i) {
            let (lane, id) = unpack_entry(entry);
            let next = cost[i + len];
            if next == u32::MAX {
                continue;
            }
            let c = Dictionary::token_width(lane) as u32 + next;
            // `<=`: при равной стоимости предпочитаем более длинное совпадение
            if c <= best {
                best = c;
                best_op = pack(OP_TOKEN + lane as u32, id);
            }
        }
        cost[i] = best;
        ops[i] = best_op;
    }
    if n > 0 && cost[0] == u32::MAX {
        return Err("DP не нашёл пути кодирования");
    }
    // Эмиссия токенов по обратным указателям
    let mut payload = Vec::with_capacity(n / 2 + 16);
    let mut st = EncodeStats::default();
    let mut i = 0usize;
    while i < n {
        let packed = ops[i];
        let op = packed >> 29;
        let arg = packed & ARG_MASK;
        match op {
            OP_ASCII => {
                payload.push(input[i]);
                i += 1;
                st.ascii += 1;
            }
            OP_RUN => {
                let l = arg as usize;
                payload.push(TAG_RUN);
                put_uvarint(l as u64, &mut payload);
                payload.extend_from_slice(&input[i..i + l]);
                i += l;
                st.run += 1;
                st.run_bytes += l as u64;
            }
            OP_RAW => {
                payload.push(TAG_RAW);
                payload.push(input[i]);
                i += 1;
                st.raw_byte += 1;
            }
            lane_op => {
                let lane = (lane_op - OP_TOKEN) as usize;
                let id = arg as u64;
                let entry_len = dict.entry(lane, id).len();
                dict.encode_token(lane, id, &mut payload);
                i += entry_len;
                match lane {
                    LANE_SYMBOL => st.symbol += 1,
                    LANE_SHORT => st.short += 1,
                    LANE_REGULAR => st.regular += 1,
                    LANE_STRUCT => st.struct_ += 1,
                    LANE_EXTENDED => st.extended += 1,
                    _ => unreachable!("lane 0..4 проверяется при упаковке entry"),
                }
            }
        }
    }
    // Сборка кадра: магия + обучаемый словарь + дайджест + длина входа + полезная нагрузка
    let trained = dict.serialize_trained_frame();
    let mut frame = Vec::with_capacity(MAGIC.len() + trained.len() + 9 + payload.len());
    frame.extend_from_slice(MAGIC);
    frame.extend_from_slice(&trained);
    let digest = crate::dict::fnv1a64(&frame);
    frame.extend_from_slice(&digest.to_le_bytes());
    put_uvarint(n as u64, &mut frame);
    frame.extend_from_slice(&payload);
    st.payload = payload.len() as u64;
    st.dict_bytes = trained.len() as u64;
    st.frame = frame.len() as u64;
    st.tokens = st.ascii + st.symbol + st.short + st.regular + st.struct_ + st.extended + st.run + st.raw_byte;
    Ok((frame, st))
}

/// Декодер кадра: полная проверка формата, без паник, ошибки — `&'static str`.
pub fn decode(frame: &[u8]) -> Result<Vec<u8>, &'static str> {
    if frame.len() < MAGIC.len() || &frame[..MAGIC.len()] != MAGIC {
        return Err("неверная метка кадра");
    }
    let mut pos = MAGIC.len();
    let dict = Dictionary::parse_trained_frame(frame, &mut pos)?;
    if pos + 8 > frame.len() {
        return Err("усечённый дайджест кадра");
    }
    let mut digest_bytes = [0u8; 8];
    digest_bytes.copy_from_slice(&frame[pos..pos + 8]);
    let expected = u64::from_le_bytes(digest_bytes);
    if crate::dict::fnv1a64(&frame[..pos]) != expected {
        return Err("дайджест словаря не совпал");
    }
    pos += 8;
    let out_len = get_uvarint(frame, &mut pos)? as usize;
    let mut out: Vec<u8> = Vec::with_capacity(out_len.min(64 * 1024 * 1024));
    while pos < frame.len() {
        let lead = frame[pos];
        pos += 1;
        match lead {
            0x00..=0x7F => out.push(lead),
            0x80..=0xBF => {
                let id = (lead - 0x80) as u64;
                append_entry(&dict, LANE_SYMBOL, id, &mut out)?;
            }
            0xC0..=0xDF => {
                if pos >= frame.len() {
                    return Err("усечённый 2-байтовый токен");
                }
                let id = (((lead - 0xC0) as u64) << 8) | frame[pos] as u64;
                pos += 1;
                append_entry(&dict, LANE_SHORT, id, &mut out)?;
            }
            0xE0..=0xEF => {
                if pos + 2 > frame.len() {
                    return Err("усечённый 3-байтовый токен");
                }
                let id = (((lead - 0xE0) as u64) << 16)
                    | ((frame[pos] as u64) << 8)
                    | frame[pos + 1] as u64;
                pos += 2;
                append_entry(&dict, LANE_REGULAR, id, &mut out)?;
            }
            0xF0..=0xF7 => {
                if pos + 3 > frame.len() {
                    return Err("усечённый 4-байтовый токен");
                }
                let id = (((lead - 0xF0) as u64) << 24)
                    | ((frame[pos] as u64) << 16)
                    | ((frame[pos + 1] as u64) << 8)
                    | frame[pos + 2] as u64;
                pos += 3;
                append_entry(&dict, LANE_STRUCT, id, &mut out)?;
            }
            0xF8..=0xFB => {
                if pos + 4 > frame.len() {
                    return Err("усечённый 5-байтовый токен");
                }
                let id = (((lead - 0xF8) as u64) << 32)
                    | ((frame[pos] as u64) << 24)
                    | ((frame[pos + 1] as u64) << 16)
                    | ((frame[pos + 2] as u64) << 8)
                    | frame[pos + 3] as u64;
                pos += 4;
                append_entry(&dict, LANE_EXTENDED, id, &mut out)?;
            }
            TAG_RUN => {
                let len = get_uvarint(frame, &mut pos)? as usize;
                if len == 0 {
                    return Err("литеральная цепочка нулевой длины");
                }
                if pos + len > frame.len() {
                    return Err("литеральная цепочка выходит за пределы кадра");
                }
                out.extend_from_slice(&frame[pos..pos + len]);
                pos += len;
            }
            TAG_RAW => {
                if pos >= frame.len() {
                    return Err("усечённый RAW-байт");
                }
                out.push(frame[pos]);
                pos += 1;
            }
            _ => return Err("зарезервированный тег первого байта"),
        }
        if out.len() > out_len {
            return Err("выход превышает объявленную длину");
        }
    }
    if out.len() != out_len {
        return Err("выход короче объявленной длины");
    }
    Ok(out)
}

fn append_entry(
    dict: &Dictionary,
    lane: usize,
    id: u64,
    out: &mut Vec<u8>,
) -> Result<(), &'static str> {
    let list = dict.lane(lane);
    if id as usize >= list.len() {
        return Err("токен ссылается на неизвестный ID словаря");
    }
    out.extend_from_slice(dict.entry(lane, id).as_bytes());
    Ok(())
}

/// Эталон «фиксированные 3 байта на токен»: каждое совпадение словаря и каждый
/// литеральный символ кодируются ровно 3 байтами (big-endian 24 бита):
/// 0x000000..0x10FFFF — кодовая точка Unicode; 0x110000..0x1FFFFF — плоский
/// индекс словарной записи; 0x200000..0x2000FF — «сырой» байт (0x80..0xFF и
/// произвольные байты при невалидном UTF-8).
pub fn encode_fixed3(input: &[u8], dict: &Dictionary) -> Result<Vec<u8>, &'static str> {
    if dict.flat_count() > 0x1FFFFF - 0x110000 + 1 {
        return Err("словарь слишком велик для эталона fixed-3B");
    }
    let text = std::str::from_utf8(input).ok();
    let mut out = Vec::with_capacity(input.len() * 3 / 2 + 16);
    let push = |v: u32, out: &mut Vec<u8>| {
        out.push((v >> 16) as u8);
        out.push((v >> 8) as u8);
        out.push(v as u8);
    };
    match text {
        Some(text) => {
            let mut i = 0usize;
            while i < input.len() {
                match dict.trie().longest_at(input, i) {
                    Some((len, entry)) => {
                        let (lane, id) = unpack_entry(entry);
                        push(0x110000 + dict.flat_index(lane, id as u64), &mut out);
                        i += len;
                    }
                    None => {
                        let ch = text[i..].chars().next().ok_or("некорректный UTF-8")?;
                        push(ch as u32, &mut out);
                        i += ch.len_utf8();
                    }
                }
            }
        }
        None => {
            // невалидный UTF-8: побайтовый режим
            for &b in input {
                push(if b < 0x80 { b as u32 } else { 0x200000 + b as u32 }, &mut out);
            }
        }
    }
    Ok(out)
}

pub fn decode_fixed3(code: &[u8], dict: &Dictionary) -> Result<Vec<u8>, &'static str> {
    if code.len() % 3 != 0 {
        return Err("поток fixed-3B не кратен 3 байтам");
    }
    let mut out = Vec::with_capacity(code.len());
    for chunk in code.chunks_exact(3) {
        let v = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        match v {
            0x110000..=0x1FFFFF => {
                let idx = v - 0x110000;
                if idx >= dict.flat_count() {
                    return Err("fixed-3B ссылается на неизвестную запись словаря");
                }
                out.extend_from_slice(dict.entry_by_flat(idx).as_bytes());
            }
            0x200000..=0x2000FF => out.push((v - 0x200000) as u8),
            _ => {
                let ch = char::from_u32(v).ok_or("fixed-3B содержит суррогатный диапазон")?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------
// Разделение потока токенов на управляющий поток и поток данных
// (экспериментальная надстройка для пост-сжатия бинарных данных).
//
// Управляющий поток (ctrl) содержит только структурные байты: байты
// токенов словаря, теги RUN/RAW и маркеры серий ASCII (0x00 + uvarint).
// Поток данных (data) — литеральные байты (ASCII, содержимое RUN, RAW).
// Оба потока детерминированно восстанавливают исходный поток токенов.

/// Результат разделения потока токенов кадра.
pub struct StreamSplit {
    pub ctrl: Vec<u8>,
    pub data: Vec<u8>,
    /// Смещение начала потока токенов в кадре (длина заголовка).
    pub header_len: usize,
    pub ascii_runs: u64,
    pub ascii_bytes: u64,
    pub run_tokens: u64,
    pub run_bytes: u64,
    pub raw_tokens: u64,
    pub token_tokens: u64,
}

/// Ширина токена по лид-байту (0x80..0xBF — 1, 0xC0..0xDF — 2, 0xE0..0xEF — 3,
/// 0xF0..0xF7 — 4, 0xF8..0xFB — 5).
pub fn token_width_from_lead(lead: u8) -> usize {
    match lead {
        0x80..=0xBF => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 5, // 0xF8..=0xFB; 0xFC/0xFD обрабатываются до вызова
    }
}

/// Разделение кадра на ctrl/data. Ошибки формата — `Err`.
pub fn split_streams(frame: &[u8]) -> Result<StreamSplit, &'static str> {
    if frame.len() < MAGIC.len() || &frame[..MAGIC.len()] != MAGIC {
        return Err("неверная метка кадра");
    }
    let mut pos = MAGIC.len();
    Dictionary::parse_trained_frame(frame, &mut pos)?;
    if pos + 8 > frame.len() {
        return Err("усечённый дайджест кадра");
    }
    pos += 8;
    let _out_len = get_uvarint(frame, &mut pos)? as usize;
    let header_len = pos;
    let mut w = StreamSplit {
        ctrl: Vec::with_capacity(frame.len() - header_len),
        data: Vec::new(),
        header_len,
        ascii_runs: 0,
        ascii_bytes: 0,
        run_tokens: 0,
        run_bytes: 0,
        raw_tokens: 0,
        token_tokens: 0,
    };
    while pos < frame.len() {
        let lead = frame[pos];
        if lead < 0x80 {
            let start = pos;
            while pos < frame.len() && frame[pos] < 0x80 {
                pos += 1;
            }
            let k = pos - start;
            w.ctrl.push(0x00);
            put_uvarint(k as u64, &mut w.ctrl);
            w.data.extend_from_slice(&frame[start..pos]);
            w.ascii_runs += 1;
            w.ascii_bytes += k as u64;
        } else if lead == TAG_RUN {
            pos += 1;
            let l = get_uvarint(frame, &mut pos)? as usize;
            if pos + l > frame.len() {
                return Err("усечённая RUN-цепочка");
            }
            w.ctrl.push(TAG_RUN);
            put_uvarint(l as u64, &mut w.ctrl);
            w.data.extend_from_slice(&frame[pos..pos + l]);
            pos += l;
            w.run_tokens += 1;
            w.run_bytes += l as u64;
        } else if lead == TAG_RAW {
            pos += 1;
            if pos >= frame.len() {
                return Err("усечённый RAW-байт");
            }
            w.ctrl.push(TAG_RAW);
            w.data.push(frame[pos]);
            pos += 1;
            w.raw_tokens += 1;
        } else if lead >= 0xFC {
            return Err("зарезервированный лид-байт в потоке токенов");
        } else {
            let width = token_width_from_lead(lead);
            if pos + width > frame.len() {
                return Err("усечённый токен словаря");
            }
            w.ctrl.extend_from_slice(&frame[pos..pos + width]);
            pos += width;
            w.token_tokens += 1;
        }
    }
    Ok(w)
}

/// Обратная сборка потока токенов из ctrl/data и полное восстановление кадра.
pub fn recompose_frame(frame: &[u8], split: &StreamSplit) -> Result<Vec<u8>, &'static str> {
    let payload = payload_from_streams(&split.ctrl, &split.data)?;
    let mut out = Vec::with_capacity(split.header_len + payload.len());
    out.extend_from_slice(&frame[..split.header_len]);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Сборка кадра из отдельных частей: байты заголовка + ctrl + data.
/// Обратная операция к split_streams (заголовок берётся из архива).
pub fn pack_frame(header: &[u8], ctrl: &[u8], data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let payload = payload_from_streams(ctrl, data)?;
    let mut out = Vec::with_capacity(header.len() + payload.len());
    out.extend_from_slice(header);
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Интерливинг ctrl/data в поток токенов кадра (общая логика
/// для recompose_frame и pack_frame).
fn payload_from_streams(ctrl: &[u8], data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut payload = Vec::with_capacity(ctrl.len() + data.len());
    let (mut ci, mut di) = (0usize, 0usize);
    while ci < ctrl.len() {
        let b = ctrl[ci];
        ci += 1;
        if b == 0x00 {
            let k = get_uvarint(ctrl, &mut ci)? as usize;
            if di + k > data.len() {
                return Err("поток данных исчерпан на ASCII-серии");
            }
            payload.extend_from_slice(&data[di..di + k]);
            di += k;
        } else if b == TAG_RUN {
            let l = get_uvarint(ctrl, &mut ci)? as usize;
            if di + l > data.len() {
                return Err("поток данных исчерпан на RUN-цепочке");
            }
            payload.push(TAG_RUN);
            put_uvarint(l as u64, &mut payload);
            payload.extend_from_slice(&data[di..di + l]);
            di += l;
        } else if b == TAG_RAW {
            if di >= data.len() {
                return Err("поток данных исчерпан на RAW-байте");
            }
            payload.push(TAG_RAW);
            payload.push(data[di]);
            di += 1;
        } else {
            let width = token_width_from_lead(b);
            if ci - 1 + width > ctrl.len() {
                return Err("управляющий поток исчерпан на токене");
            }
            payload.extend_from_slice(&ctrl[ci - 1..ci - 1 + width]);
            ci += width - 1;
        }
    }
    if di != data.len() {
        return Err("поток данных не израсходован полностью");
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_frame_roundtrips_split() {
        let d = Dictionary::static_only();
        let src = "проверка сборки кадра из частей: abc 123, ещё текст; словарь и прогоны.".repeat(4);
        let (frame, _) = encode(src.as_bytes(), &d).unwrap();
        let split = split_streams(&frame).unwrap();
        let packed =
            pack_frame(&frame[..split.header_len], &split.ctrl, &split.data).unwrap();
        assert_eq!(packed, frame, "pack_frame обязан восстановить кадр точно");
        assert_eq!(decode(&packed).unwrap(), src.as_bytes());
    }

    #[test]
    fn empty_input_roundtrip() {
        let d = Dictionary::static_only();
        let (frame, st) = encode(b"", &d).unwrap();
        assert_eq!(decode(&frame).unwrap(), b"");
        assert_eq!(st.payload, 0);
        let f3 = encode_fixed3(b"", &d).unwrap();
        assert!(f3.is_empty());
    }

    #[test]
    fn ascii_passthrough_is_identity() {
        let d = Dictionary::static_only();
        let src = b"plain ascii 123 !?();";
        let (frame, st) = encode(src, &d).unwrap();
        assert_eq!(st.payload as usize, src.len(), "ASCII-сквозная передача 1:1");
        assert_eq!(decode(&frame).unwrap(), src);
    }

    #[test]
    fn stream_split_recompose_roundtrip() {
        let d = Dictionary::static_only();
        // смесь: ASCII, токены словаря, RAW-байты, RUN-цепочки (байты ≥0x80)
        let src = b"\xD0\x9F\xD1\x80\xD0\xB8\xD0\xB2\xD0\xB5\xD1\x82 plain 123 \xE2\x82\xAC ok \xD0\xBC\xD0\xB8\xD1\x80 end";
        let (frame, _) = encode(src, &d).unwrap();
        let split = split_streams(&frame).unwrap();
        let rebuilt = recompose_frame(&frame, &split).unwrap();
        assert_eq!(rebuilt, frame, "реконструкция кадра байт-в-байт");
        assert_eq!(decode(&rebuilt).unwrap(), src);
        assert!(split.ascii_runs >= 1);
        assert!(split.token_tokens + split.raw_tokens + split.run_tokens >= 1);
    }
}
