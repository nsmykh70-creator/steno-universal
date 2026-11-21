//! Частотное перекодирование управляющего потока (эксперимент PS10).
//!
//! Схема: ctrl-поток разбирается на события (токен словаря, серия ASCII,
//! RUN-цепочка, RAW). Токены перенумеровываются по частоте (ранг 0 — самый
//! частый), поверх последовательности символов работает адаптивный
//! арифметический (range) кодер: главный поток — контекст порядка 1
//! (предыдущий символ, биннинг до 255 + спец-контексты), априорное
//! распределение — геометрическое по рангу. Длины серий ASCII (с эскейпом
//! в отдельный байтовый поток) и RUN кодируются тем же кодером в отдельных
//! моделях порядка 1. Декодер восстанавливает ctrl байт-в-байт; модуль
//! проверяется roundtrip'ом внутри frec_encode и юнит-тестами.

use std::collections::HashMap;

use crate::codec::token_width_from_lead;
use crate::dict::{get_uvarint, put_uvarint};

const TAG_RUN: u8 = 0xFC;
const TAG_RAW: u8 = 0xFD;

/// Символы алфавита: 0 — серия ASCII, 1 — RUN, 2 — RAW, 3+i — токен ранга i.
const SYM_A: u32 = 0;
const SYM_R: u32 = 1;
const SYM_W: u32 = 2;
/// Спец-контексты порядка 1: 256 — после A, 257 — после R, 258 — после W,
/// 259 — начало потока. Токены: контекст = min(символ, 255).
const CTX_AFTER_A: usize = 256;
const CTX_AFTER_R: usize = 257;
const CTX_AFTER_W: usize = 258;
const CTX_START: usize = 259;
const N_CTX: usize = 260;
/// Порог адаптивного деления частот пополам.
const FREQ_LIMIT: u32 = 1 << 17;
/// A-серия: символы 0..61 — длина 1..62, символ 62 (A_ESC) — длина ≥ 63,
/// точное значение уходит в поток эскейпов (uvarint от k-62).
const A_ESC: u32 = 62;
const A_MAX_DIRECT: u32 = 62;
/// Модель длин A: 63 символа + стартовый контекст 63.
const A_START_CTX: usize = 63;
/// Модель длин R: символы 0..29 (длина 3..32) + стартовый контекст 30.
const R_START_CTX: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Event {
    Tok(u64),
    A(u32),
    R(u32),
    W,
}

/// Глобальный ID токена: (шина << 40) | id — обратим в байты токена.
fn token_gid(bytes: &[u8]) -> u64 {
    let lead = bytes[0];
    match lead {
        0x80..=0xBF => (lead - 0x80) as u64,
        0xC0..=0xDF => (1u64 << 40) | ((((lead - 0xC0) as u64) << 8) | bytes[1] as u64),
        0xE0..=0xEF => {
            (2u64 << 40)
                | (((lead - 0xE0) as u64) << 16
                    | (bytes[1] as u64) << 8
                    | bytes[2] as u64)
        }
        0xF0..=0xF7 => {
            (3u64 << 40)
                | (((lead - 0xF0) as u64) << 24
                    | (bytes[1] as u64) << 16
                    | (bytes[2] as u64) << 8
                    | bytes[3] as u64)
        }
        _ => {
            (4u64 << 40)
                | (((lead - 0xF8) as u64) << 32
                    | (bytes[1] as u64) << 24
                    | (bytes[2] as u64) << 16
                    | (bytes[3] as u64) << 8
                    | bytes[4] as u64)
        }
    }
}

/// Обратное восстановление байтов токена по глобальному ID.
fn gid_bytes(gid: u64, out: &mut Vec<u8>) {
    let lane = (gid >> 40) as u8;
    let id = gid & ((1u64 << 40) - 1);
    match lane {
        0 => out.push(0x80 + id as u8),
        1 => {
            out.push(0xC0 + (id >> 8) as u8);
            out.push(id as u8);
        }
        2 => {
            out.push(0xE0 + (id >> 16) as u8);
            out.push((id >> 8) as u8);
            out.push(id as u8);
        }
        3 => {
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

fn parse_events(ctrl: &[u8]) -> Result<Vec<Event>, &'static str> {
    let mut ev = Vec::new();
    let mut i = 0usize;
    while i < ctrl.len() {
        let b = ctrl[i];
        if b == 0x00 {
            i += 1;
            let k = get_uvarint(ctrl, &mut i)?;
            if k == 0 {
                return Err("пустая серия ASCII в ctrl");
            }
            ev.push(Event::A(k as u32));
        } else if b == TAG_RUN {
            i += 1;
            let l = get_uvarint(ctrl, &mut i)?;
            if l == 0 {
                return Err("пустая RUN-цепочка в ctrl");
            }
            ev.push(Event::R(l as u32));
        } else if b == TAG_RAW {
            i += 1;
            ev.push(Event::W);
        } else if b < 0x80 {
            // в ctrl нет литеральных байтов — они живут в потоке данных
            return Err("байт данных в ctrl (ожидается маркер или токен)");
        } else if b >= 0xFC {
            return Err("зарезервированный байт в ctrl");
        } else {
            let width = token_width_from_lead(b);
            if i + width > ctrl.len() {
                return Err("усечённый токен в ctrl");
            }
            ev.push(Event::Tok(token_gid(&ctrl[i..i + width])));
            i += width;
        }
    }
    Ok(ev)
}

/// Арифметический кодер (LZMA-style range coder, 32 бита) — также
/// используется CM-модулем lpaq_ctrl для бинарного кодирования.
pub(crate) struct RcEnc {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    out: Vec<u8>,
}

impl RcEnc {
    pub(crate) fn new() -> Self {
        RcEnc { low: 0, range: u32::MAX, cache: 0, cache_size: 1, out: Vec::new() }
    }
    pub(crate) fn encode(&mut self, cum: u32, freq: u32, tot: u32) {
        let r = (self.range / tot) as u64;
        self.low += cum as u64 * r;
        self.range = (r * freq as u64) as u32;
        while self.range < (1 << 24) {
            self.shift_low();
            self.range <<= 8;
        }
    }
    fn shift_low(&mut self) {
        if self.low < 0xFF00_0000 || self.low > 0xFFFF_FFFF {
            let carry = (self.low >> 32) as u8;
            self.out.push(self.cache.wrapping_add(carry));
            while self.cache_size > 1 {
                self.out.push(0xFFu8.wrapping_add(carry));
                self.cache_size -= 1;
            }
            self.cache = (self.low >> 24) as u8;
            self.cache_size = 0;
        }
        self.cache_size += 1;
        self.low = (self.low << 8) & 0xFFFF_FFFF;
    }
    pub(crate) fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

pub(crate) struct RcDec<'a> {
    range: u32,
    code: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> RcDec<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut d = RcDec { range: u32::MAX, code: 0, data, pos: 0 };
        for _ in 0..5 {
            d.code = (d.code << 8) | d.next();
        }
        d
    }
    fn next(&mut self) -> u32 {
        let b = if self.pos < self.data.len() { self.data[self.pos] } else { 0 };
        self.pos += 1;
        b as u32
    }
    pub(crate) fn thr(&self, tot: u32) -> u32 {
        let r = self.range / tot;
        let v = self.code / r;
        if v >= tot {
            tot - 1
        } else {
            v
        }
    }
    pub(crate) fn update(&mut self, cum: u32, freq: u32, tot: u32) {
        let r = self.range / tot;
        self.code -= cum * r;
        self.range = r * freq;
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next();
        }
    }
}

/// Адаптивная частотная модель: дерево Фенвика + массив счётчиков.
/// Априорное распределение — геометрическое по рангу символа.
struct Model {
    tree: Vec<u32>,
    counts: Vec<u32>,
    total: u32,
}

impl Model {
    fn new(alphabet: usize) -> Self {
        let mut m = Model { tree: vec![0; alphabet + 1], counts: vec![0; alphabet], total: 0 };
        for s in 0..alphabet {
            let c = Self::prior(s);
            m.counts[s] = c;
            m.add(s, c as i64);
            m.total += c;
        }
        m
    }
    fn prior(s: usize) -> u32 {
        // лёгкий геометрический приор по рангу: быстро амортизируется
        // адаптивными счётчиками даже в контекстах с сотнями событий
        let sh = (s as u32).min(6);
        (64u32 >> sh).max(1)
    }
    fn add(&mut self, s: usize, delta: i64) {
        let mut i = s + 1;
        while i < self.tree.len() {
            self.tree[i] = (self.tree[i] as i64 + delta) as u32;
            i += i & i.wrapping_neg();
        }
    }
    fn prefix(&self, s: usize) -> u32 {
        let mut sum = 0u32;
        let mut i = s;
        while i > 0 {
            sum += self.tree[i];
            i &= i.wrapping_sub(1);
        }
        sum
    }
    /// Наименьший s с prefix(s+1) > v (спуск по дереву Фенвика).
    fn find(&self, v: u32) -> usize {
        let mut pos = 0usize;
        let mut rem = v + 1;
        let mut pw = 1usize;
        while pw * 2 < self.tree.len() {
            pw *= 2;
        }
        while pw > 0 {
            let nxt = pos + pw;
            if nxt < self.tree.len() && (self.tree[nxt] as u64) < rem as u64 {
                pos = nxt;
                rem -= self.tree[nxt];
            }
            pw >>= 1;
        }
        pos
    }
    fn bump(&mut self, s: usize) {
        self.counts[s] += 1;
        self.add(s, 1);
        self.total += 1;
        if self.total > FREQ_LIMIT {
            for c in self.counts.iter_mut() {
                *c = (*c + 1) / 2;
            }
            self.total = self.counts.iter().sum();
            self.tree = vec![0; self.counts.len() + 1];
            for s in 0..self.counts.len() {
                self.add(s, self.counts[s] as i64);
            }
        }
    }
}

/// Результат кодирования: таблица рангов (глобальные ID токенов по убыванию
/// частоты), главный арифметический поток и поток эскейпов длин A.
pub struct FrecParts {
    pub ftab: Vec<u8>,
    pub fmain: Vec<u8>,
    pub fesc: Vec<u8>,
}

struct Ranks {
    /// символ алфавита (3+) по глобальному ID токена
    sym_of_gid: HashMap<u64, u32>,
    alphabet: usize,
    /// глобальные ID в порядке рангов — передаются декодеру
    ftab: Vec<u8>,
}

fn build_ranks(ev: &[Event]) -> Ranks {
    let mut counts: HashMap<u64, u64> = HashMap::new();
    for e in ev {
        if let Event::Tok(g) = e {
            *counts.entry(*g).or_insert(0) += 1;
        }
    }
    let mut order: Vec<(u64, u64)> = counts.into_iter().collect();
    order.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut sym_of_gid = HashMap::with_capacity(order.len());
    let mut ftab = Vec::with_capacity(order.len() * 3);
    for (i, (gid, _)) in order.iter().enumerate() {
        sym_of_gid.insert(*gid, SYM_W + 1 + i as u32);
        put_uvarint(*gid, &mut ftab);
    }
    Ranks { sym_of_gid, alphabet: 3 + order.len(), ftab }
}

/// Кодирование ctrl-потока. Возвращает части (с проверкой декодированием
/// внутри) и число событий с размером алфавита.
pub fn frec_encode(ctrl: &[u8], order0: bool) -> Result<(FrecParts, u64, u64), &'static str> {
    let ev = parse_events(ctrl)?;
    let ranks = build_ranks(&ev);
    let alpha = ranks.alphabet;

    let parts = frec_encode_events(&ev, &ranks, order0)?;
    let back = frec_decode(&parts, ctrl.len(), order0)?;
    if back != ctrl {
        return Err("frec: декодирование не совпало с исходным ctrl");
    }
    Ok((parts, ev.len() as u64, alpha as u64))
}

fn frec_encode_events(ev: &[Event], ranks: &Ranks, order0: bool) -> Result<FrecParts, &'static str> {
    let alpha = ranks.alphabet;
    let n_ctx = if order0 { 1 } else { N_CTX };
    let mut mm: Vec<Model> = (0..n_ctx).map(|_| Model::new(alpha)).collect();
    let mut ma: Vec<Model> = (0..=A_START_CTX).map(|_| Model::new(63)).collect();
    let mut mr: Vec<Model> = (0..=R_START_CTX).map(|_| Model::new(30)).collect();
    let mut rc = RcEnc::new();
    let mut esc: Vec<u8> = Vec::new();
    let (mut prev_m, mut prev_a, mut prev_r) =
        (if order0 { 0 } else { CTX_START }, A_START_CTX, R_START_CTX);
    for e in ev {
        match *e {
            Event::Tok(g) => {
                let sym = *ranks.sym_of_gid.get(&g).ok_or("токен вне таблицы рангов")?;
                let m = &mut mm[prev_m];
                let (cum, freq, tot) = (m.prefix(sym as usize), m.counts[sym as usize], m.total);
                rc.encode(cum, freq, tot);
                m.bump(sym as usize);
                if !order0 {
                    prev_m = (sym as usize).min(255);
                }
            }
            Event::A(k) => {
                let m = &mut mm[prev_m];
                let (cum, freq, tot) = (m.prefix(SYM_A as usize), m.counts[SYM_A as usize], m.total);
                rc.encode(cum, freq, tot);
                m.bump(SYM_A as usize);
                if !order0 {
                    prev_m = CTX_AFTER_A;
                }
                let sym = (k - 1).min(A_ESC);
                let m = &mut ma[prev_a];
                let (cum, freq, tot) = (m.prefix(sym as usize), m.counts[sym as usize], m.total);
                rc.encode(cum, freq, tot);
                m.bump(sym as usize);
                if k > A_MAX_DIRECT {
                    put_uvarint((k - A_MAX_DIRECT) as u64, &mut esc);
                }
                prev_a = sym as usize;
            }
            Event::R(l) => {
                let m = &mut mm[prev_m];
                let (cum, freq, tot) = (m.prefix(SYM_R as usize), m.counts[SYM_R as usize], m.total);
                rc.encode(cum, freq, tot);
                m.bump(SYM_R as usize);
                if !order0 {
                    prev_m = CTX_AFTER_R;
                }
                let sym = l - 3;
                let m = &mut mr[prev_r];
                let (cum, freq, tot) = (m.prefix(sym as usize), m.counts[sym as usize], m.total);
                rc.encode(cum, freq, tot);
                m.bump(sym as usize);
                prev_r = sym as usize;
            }
            Event::W => {
                let m = &mut mm[prev_m];
                let (cum, freq, tot) = (m.prefix(SYM_W as usize), m.counts[SYM_W as usize], m.total);
                rc.encode(cum, freq, tot);
                m.bump(SYM_W as usize);
                if !order0 {
                    prev_m = CTX_AFTER_W;
                }
            }
        }
    }
    Ok(FrecParts { ftab: ranks.ftab.clone(), fmain: rc.finish(), fesc: esc })
}

/// Декодирование: восстанавливает ctrl байт-в-байт.
pub fn frec_decode(parts: &FrecParts, ctrl_len: usize, order0: bool) -> Result<Vec<u8>, &'static str> {
    // таблица рангов → глобальные ID
    let mut gids: Vec<u64> = Vec::new();
    let mut i = 0usize;
    while i < parts.ftab.len() {
        gids.push(get_uvarint(&parts.ftab, &mut i)?);
    }
    let alpha = 3 + gids.len();
    let n_ctx = if order0 { 1 } else { N_CTX };
    let mut mm: Vec<Model> = (0..n_ctx).map(|_| Model::new(alpha)).collect();
    let mut ma: Vec<Model> = (0..=A_START_CTX).map(|_| Model::new(63)).collect();
    let mut mr: Vec<Model> = (0..=R_START_CTX).map(|_| Model::new(30)).collect();
    let mut rc = RcDec::new(&parts.fmain);
    let mut esc_pos = 0usize;
    let mut out: Vec<u8> = Vec::with_capacity(ctrl_len);
    let (mut prev_m, mut prev_a, mut prev_r) =
        (if order0 { 0 } else { CTX_START }, A_START_CTX, R_START_CTX);
    while out.len() < ctrl_len {
        let m = &mut mm[prev_m];
        let tot = m.total;
        let v = rc.thr(tot);
        let sym = m.find(v) as u32;
        let cum = m.prefix(sym as usize);
        let freq = m.counts[sym as usize];
        rc.update(cum, freq, tot);
        m.bump(sym as usize);
        match sym {
            SYM_A => {
                out.push(0x00);
                let m = &mut ma[prev_a];
                let tot = m.total;
                let v = rc.thr(tot);
                let sa = m.find(v) as u32;
                let cum = m.prefix(sa as usize);
                let freq = m.counts[sa as usize];
                rc.update(cum, freq, tot);
                m.bump(sa as usize);
                let k = if sa == A_ESC {
                    A_MAX_DIRECT + get_uvarint(&parts.fesc, &mut esc_pos)? as u32
                } else {
                    sa + 1
                };
                if k == 0 {
                    return Err("frec: нулевая длина серии ASCII");
                }
                put_uvarint(k as u64, &mut out);
                prev_a = sa as usize;
                if !order0 {
                    prev_m = CTX_AFTER_A;
                }
            }
            SYM_R => {
                out.push(TAG_RUN);
                let m = &mut mr[prev_r];
                let tot = m.total;
                let v = rc.thr(tot);
                let sr = m.find(v) as u32;
                let cum = m.prefix(sr as usize);
                let freq = m.counts[sr as usize];
                rc.update(cum, freq, tot);
                m.bump(sr as usize);
                let l = sr + 3;
                put_uvarint(l as u64, &mut out);
                prev_r = sr as usize;
                if !order0 {
                    prev_m = CTX_AFTER_R;
                }
            }
            SYM_W => {
                out.push(TAG_RAW);
                if !order0 {
                    prev_m = CTX_AFTER_W;
                }
            }
            s => {
                let idx = (s - 3) as usize;
                if idx >= gids.len() {
                    return Err("frec: символ вне таблицы рангов");
                }
                gid_bytes(gids[idx], &mut out);
                if !order0 {
                    prev_m = (s as usize).min(255);
                }
            }
        }
    }
    if out.len() != ctrl_len {
        return Err("frec: длина реконструкции не совпала");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(ctrl: &[u8], order0: bool) {
        let (parts, ev, alpha) = frec_encode(ctrl, order0).unwrap();
        let back = frec_decode(&parts, ctrl.len(), order0).unwrap();
        assert_eq!(back, ctrl);
        assert!(ev >= 1 || ctrl.is_empty());
        assert!(alpha >= 3);
    }

    fn synthetic_ctrl() -> Vec<u8> {
        let mut c = Vec::new();
        // серия ASCII из 3 литералов (в ctrl — только маркер и длина)
        c.push(0x00);
        c.push(3);
        // 1-байтовые токены (шина 0)
        c.push(0x81);
        c.push(0x9F);
        // серия ASCII из 2
        c.push(0x00);
        c.push(2);
        // RUN-цепочка длиной 4 (в ctrl — только тег и длина, байты в data)
        c.push(TAG_RUN);
        c.push(4);
        // RAW (в ctrl — только тег)
        c.push(TAG_RAW);
        // токены ширин 2, 3, 4, 5
        c.extend_from_slice(&[0xC0, 0x07]);
        c.extend_from_slice(&[0xE0, 0x00, 0x05]);
        c.extend_from_slice(&[0xF0, 0x00, 0x00, 0x01]);
        c.extend_from_slice(&[0xF8, 0x00, 0x00, 0x00, 0x09]);
        // длинная серия ASCII — эскейп (k = 100)
        c.push(0x00);
        put_uvarint(100, &mut c);
        // граница прямого кодирования (k = 62) и первый эскейп (k = 63)
        c.push(0x00);
        c.push(62);
        c.push(0x00);
        c.push(63);
        c
    }

    #[test]
    fn synthetic_roundtrip() {
        let c = synthetic_ctrl();
        roundtrip(&c, false);
        roundtrip(&c, true);
    }

    #[test]
    fn empty_ctrl_roundtrip() {
        roundtrip(&[], false);
        roundtrip(&[], true);
    }

    #[test]
    fn only_raw_roundtrip() {
        // в ctrl RAW-событие — это только тег 0xFD (байт уходит в data)
        let c = vec![TAG_RAW, TAG_RAW];
        roundtrip(&c, false);
        roundtrip(&c, true);
    }

    #[test]
    fn repeated_tokens_compress_well() {
        // 500 повторов двух токенов — энтропийный кодер должен сжать сильно
        let mut c = Vec::new();
        for _ in 0..500 {
            c.push(0x85);
            c.push(0x9A);
        }
        let (parts, ev, alpha) = frec_encode(&c, false).unwrap();
        let total = parts.ftab.len() + parts.fmain.len() + parts.fesc.len();
        assert!(total < c.len() / 4, "total {total} против ctrl {}", c.len());
        assert_eq!(alpha, 5); // A, R, W + 2 токена
        assert_eq!(ev, 1000);
    }
}
