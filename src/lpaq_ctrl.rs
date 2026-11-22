//! CM-кодер класса lpaq/zpaq для ctrl-потока (cmz2, PS16).
//!
//! Бинарное контекстное смешивание: каждый бит байта предсказывают модели —
//! порядок 0 (частичный байт), хешированные порядки 1–3 и match-модель
//! с длинными контекстами (хеш последних 8 байтов → двухслотовый бакет
//! позиций в истории; второй слот хранит предпоследнее вхождение, поэтому
//! дальние повторы находятся даже при перезаписи ближнего слота).
//! Порядки 4–6 сняты ради скорости (цель PS16): 5 предсказаний на бит
//! вместо 8, индексы контекстов кэшируются между predict и update.
//! История матча не ограничена окном (позиции u32 — до 4 ГБ).
//! Предсказания смешиваются логистическим миксером и калибруются двумя
//! APM-стадиями. Кодирование — бинарный range coder (общий с frec).
//! Декодер повторяет цепочку моделей по декодированным битам; roundtrip
//! проверяется внутри cmz_encode.

use crate::dict::{get_uvarint, put_uvarint};
use crate::frec::{RcDec, RcEnc};

const COUNT_LIMIT: u32 = 255;
const MATCH_MINL: usize = 8;
const MATCH_MAXLEN: usize = 65535;
/// размер хеш-таблицы позиций match-модели (ключей; у каждого 2 слота)
const MTAB_BITS: u32 = 23;
/// размер таблиц счётчиков хешированных контекстов
const CTAB_BITS: u32 = 22;
/// число хешированных порядков: 1, 2, 3
const NORD: usize = 3;
/// число входов миксера: порядок 0 + NORD контекстов + матч
const NIN: usize = NORD + 2;
/// версия файлового формата cmz
pub const CMZ_VERSION: u64 = 2;

/// Адаптивный счётчик: 12-битная вероятность + счётчик наблюдений.
struct Ctr {
    p: u16,
    c: u16,
}

impl Ctr {
    fn new() -> Self {
        Ctr { p: 2048, c: 0 }
    }
    #[inline]
    fn pr(&self) -> u32 {
        self.p as u32
    }
    #[inline]
    fn upd(&mut self, bit: u32, rate: &[u32; 256]) {
        let target = if bit != 0 { 4096i32 } else { 0 };
        self.p =
            (self.p as i32 + ((target - self.p as i32) * rate[self.c as usize] as i32 >> 16)) as u16;
        if (self.c as u32) < COUNT_LIMIT {
            self.c += 1;
        }
    }
}

/// Смешение вероятности с адаптивной картой (APM/SSE).
struct Apm {
    p: Vec<u16>,
    c: Vec<u16>,
    groups: usize,
}

impl Apm {
    fn new(groups: usize) -> Self {
        Apm { p: vec![2048; groups * 33], c: vec![0; groups * 33], groups }
    }
    /// pr_in — вероятность миксера (12 бит); возвращает уточнённую.
    fn apply(&mut self, idx: usize, pr_in: u32, rate: &[u32; 256]) -> u32 {
        let qp = ((pr_in * 32 + 2048) / 4096) as usize; // 0..=32
        let i = (idx % self.groups) * 33 + qp;
        let pa = self.p[i] as u32;
        let cc = self.c[i] as u32;
        let out = (pr_in * 16 + pa * (cc + 1)) / (cc + 17);
        self.p[i] = (pr_in as i32
            + (((pa as i32 - pr_in as i32) * rate[cc as usize] as i32) >> 16)) as u16;
        if cc < 255 {
            self.c[i] = (cc + 1) as u16;
        }
        out.max(1).min(4095)
    }
}

/// Логистический миксер: NIN входов, 16 наборов весов (бит × режим матча).
struct Mixer {
    w: Vec<i32>,
    inputs: [i32; NIN],
    set: usize,
    p: u32,
}

impl Mixer {
    fn new() -> Self {
        // порядок 0 — сильный калибратор, контексты и матч — умеренно
        let mut w = vec![0i32; 16 * NIN];
        for s in 0..16 {
            w[s * NIN] = 3 << 15;
            for k in 1..NIN {
                w[s * NIN + k] = 1 << 15;
            }
        }
        Mixer { w, inputs: [0; NIN], set: 0, p: 2048 }
    }
    fn mix(&mut self) {
        let base = self.set * NIN;
        let mut dot = 0i64;
        for k in 0..NIN {
            dot += self.w[base + k] as i64 * self.inputs[k] as i64;
        }
        let d = (dot >> 16).clamp(-2047, 2047) as i32;
        self.p = squash(d);
    }
    fn update(&mut self, bit: u32) {
        let err = (bit << 12) as i64 - self.p as i64;
        let base = self.set * NIN;
        for k in 0..NIN {
            let dw = (self.inputs[k] as i64 * err * 10) >> 16;
            self.w[base + k] = (self.w[base + k] as i64 + dw).clamp(-(4 << 20), 4 << 20) as i32;
        }
    }
}

#[inline]
fn squash(x: i32) -> u32 {
    if x >= 2047 {
        4095
    } else if x <= -2047 {
        1
    } else {
        let e = (x as f64) / 256.0;
        (4096.0 / (1.0 + (-e).exp())) as u32
    }
}

#[inline]
fn stretch_of(p: u32, table: &[i32; 4096]) -> i32 {
    table[p.clamp(1, 4095) as usize]
}

fn build_stretch() -> [i32; 4096] {
    let mut t = [0i32; 4096];
    for p in 1..4096 {
        let v = 256.0 * ((p as f64) / (4096.0 - p as f64)).ln();
        t[p] = v.clamp(-2047.0, 2047.0) as i32;
    }
    t
}

fn build_rate() -> [u32; 256] {
    let mut r = [0u32; 256];
    for c in 0..256usize {
        r[c] = (65536.0 / (c as f64 + 1.5)) as u32;
    }
    r
}

/// Match-модель: хеш последних 8 байтов → бакет из 2 позиций.
/// Слот 0 — последнее вхождение, слот 1 — предпоследнее: дальний повтор
/// находится, даже если ближний слот перезаписан частым коротким контекстом.
struct MatchModel {
    table: Vec<[u32; 2]>,
    mask: usize,
    /// позиция байта-продолжения в истории (0 = нет активного матча)
    pos: usize,
    len: usize,
}

impl MatchModel {
    fn new() -> Self {
        MatchModel {
            table: vec![[0u32; 2]; 1 << MTAB_BITS],
            mask: (1 << MTAB_BITS) - 1,
            pos: 0,
            len: 0,
        }
    }
    /// Граница байта i: обновить таблицу и найти матч по 8-байтовому контексту.
    fn boundary(&mut self, buf: &[u8], i: usize, hctx: &mut u64) {
        *hctx = 0;
        self.pos = 0;
        self.len = 0;
        if i >= MATCH_MINL {
            let mut h = 0x9E3779B97F4A7C15u64;
            for k in 0..MATCH_MINL {
                h = (h ^ buf[i - MATCH_MINL + k] as u64).wrapping_mul(0x100000001B3);
            }
            *hctx = h;
            let idx = (h as usize) & self.mask;
            // сначала валидация кандидатов (старые позиции), потом запись новой;
            // строгий знак: pos обязан указывать на уже известный байт
            for slot in 0..2 {
                let cand = self.table[idx][slot] as usize;
                if cand > 0
                    && cand + MATCH_MINL < i
                    && buf[cand..cand + MATCH_MINL] == buf[i - MATCH_MINL..i]
                {
                    self.pos = cand + MATCH_MINL;
                    self.len = MATCH_MINL;
                    break;
                }
            }
            self.table[idx][1] = self.table[idx][0];
            self.table[idx][0] = i as u32;
        }
    }
    /// Обновление по факту бита: несовпадение ломает матч, полный байт продлевает.
    fn bit_update(&mut self, buf: &[u8], j: usize, bit: u32, i_next: usize) {
        if self.pos != 0 {
            let pbit = ((buf[self.pos] >> (7 - j)) & 1) as u32;
            if pbit != bit {
                self.pos = 0;
                self.len = 0;
            } else if j == 7 {
                self.pos += 1;
                self.len = (self.len + 1).min(MATCH_MAXLEN);
                if self.pos >= i_next {
                    self.pos = 0;
                }
            }
        }
    }
}

#[inline]
fn hash_ctx(hprev: u64, partial: u32, salt: u32) -> usize {
    let mut h = hprev ^ (partial as u64).wrapping_mul(0x85EBCA6B);
    h ^= h >> 29;
    h = h.wrapping_mul(0xC2B2AE3D27D4EB4F).wrapping_add(salt as u64);
    (h >> 32) as usize
}

struct CmState {
    hc: [u64; NORD + 1],
    stretch: [i32; 4096],
    rate: [u32; 256],
    t0: Vec<Ctr>,
    /// таблицы порядков 1..=NORD (индекс k-1)
    th: Vec<Vec<Ctr>>,
    /// таблица match-модели
    tm: Vec<Ctr>,
    mm: MatchModel,
    hctx: u64,
    mixer: Mixer,
    apm1: Apm,
    apm2: Apm,
    /// кэш индексов контекстов текущего бита (predict → update без пере-хеша)
    li: [usize; NORD],
    /// кэш индекса match-счётчика текущего бита
    lm: usize,
}

impl CmState {
    fn new() -> Self {
        let n = 1usize << CTAB_BITS;
        CmState {
            hc: [0; NORD + 1],
            stretch: build_stretch(),
            rate: build_rate(),
            t0: (0..256).map(|_| Ctr::new()).collect(),
            th: (0..NORD).map(|_| (0..n).map(|_| Ctr::new()).collect()).collect(),
            tm: (0..n).map(|_| Ctr::new()).collect(),
            mm: MatchModel::new(),
            hctx: 0,
            mixer: Mixer::new(),
            apm1: Apm::new(256),
            apm2: Apm::new(1 << 15),
            li: [0; NORD],
            lm: 0,
        }
    }
    #[inline]
    fn match_idx(&self, c0: u32, pb: u32) -> usize {
        hash_ctx(
            self.hctx ^ (pb as u64).wrapping_mul(0x27D4EB2F165667C5),
            c0 ^ ((self.mm.len as u32) << 8),
            77,
        ) & ((1 << CTAB_BITS) - 1)
    }
    /// Предсказание бита j при частичном байте c0; buf — уже известные байты.
    fn predict(&mut self, j: usize, c0: u32, buf: &[u8]) -> u32 {
        let st = &self.stretch;
        let p0 = self.t0[c0 as usize].pr();
        self.mixer.inputs[0] = stretch_of(p0, st);
        for k in 0..NORD {
            let idx = hash_ctx(self.hc[k + 1], c0, (k + 1) as u32) & ((1 << CTAB_BITS) - 1);
            self.li[k] = idx;
            let pk = self.th[k][idx].pr();
            self.mixer.inputs[k + 1] = stretch_of(pk, st);
        }
        let mut pm = 2048u32;
        self.lm = 0;
        if self.mm.pos != 0 {
            let pb = buf[self.mm.pos] as u32;
            let idx = self.match_idx(c0, pb);
            self.lm = idx;
            pm = self.tm[idx].pr();
        }
        self.mixer.inputs[NIN - 1] = stretch_of(pm, st);
        self.mixer.set = j * 2 + if self.mm.pos != 0 { 1 } else { 0 };
        self.mixer.mix();
        let mut p = self.mixer.p;
        p = self.apm1.apply(c0 as usize, p, &self.rate);
        p = self.apm2.apply(hash_ctx(self.hc[1], c0, 99) & ((1 << 15) - 1), p, &self.rate);
        p
    }
    /// Обновление всех моделей по факту бита (индексы кэшированы predict'ом).
    fn update(&mut self, j: usize, c0: u32, bit: u32, buf: &[u8], i_next: usize) {
        self.t0[c0 as usize].upd(bit, &self.rate);
        for k in 0..NORD {
            self.th[k][self.li[k]].upd(bit, &self.rate);
        }
        if self.mm.pos != 0 {
            self.tm[self.lm].upd(bit, &self.rate);
        }
        self.mixer.update(bit);
        self.mm.bit_update(buf, j, bit, i_next);
    }
    fn byte_done(&mut self, b: u8) {
        let old = self.hc;
        self.hc[0] = b as u64;
        for k in 1..=NORD {
            self.hc[k] = old[k - 1].wrapping_mul(0x9E3779B97F4A7C15)
                ^ (b as u64).wrapping_add(k as u64);
        }
    }
}

/// Кодирование ctrl-потока CM-кодером с внутренней проверкой декодированием.
pub fn cmz_encode(ctrl: &[u8]) -> Result<Vec<u8>, &'static str> {
    let out = cmz_encode_raw(ctrl);
    let back = cmz_decode(&out, ctrl.len())?;
    if back != ctrl {
        return Err("cmz: декодирование не совпало с исходным ctrl");
    }
    Ok(out)
}

fn cmz_encode_raw(ctrl: &[u8]) -> Vec<u8> {
    let mut st = CmState::new();
    let mut rc = RcEnc::new();
    let mut buf: Vec<u8> = Vec::with_capacity(ctrl.len());
    for &b in ctrl {
        let i = buf.len();
        st.mm.boundary(&buf, i, &mut st.hctx);
        let mut c0 = 1u32;
        for j in 0..8 {
            let bit = ((b >> (7 - j)) & 1) as u32;
            let p = st.predict(j, c0, &buf);
            let p16 = (p << 4).clamp(16, 65520);
            if bit == 1 {
                rc.encode(0, p16, 65536);
            } else {
                rc.encode(p16, 65536 - p16, 65536);
            }
            st.update(j, c0, bit, &buf, i + 1);
            c0 = c0 * 2 + bit;
        }
        st.byte_done(b);
        buf.push(b);
    }
    rc.finish()
}

/// Файловый формат cmz: uvarint(длина исходного потока) + uvarint(версия) +
/// CM-поток. Длина нужна декодеру, версия — гарантия совместимости моделей.
pub fn cmz_encode_file(ctrl: &[u8]) -> Result<Vec<u8>, &'static str> {
    let body = cmz_encode(ctrl)?;
    let mut out = Vec::with_capacity(body.len() + 8);
    put_uvarint(ctrl.len() as u64, &mut out);
    put_uvarint(CMZ_VERSION, &mut out);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Декодирование файлового cmz-формата (длина и версия читаются из заголовка).
pub fn cmz_decode_file(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut pos = 0usize;
    let n = get_uvarint(data, &mut pos)? as usize;
    let ver = get_uvarint(data, &mut pos)?;
    if ver != CMZ_VERSION {
        return Err("cmz: неизвестная версия формата");
    }
    if n == 0 || pos >= data.len() {
        return Err("cmz: пустой или неверный заголовок");
    }
    cmz_decode(&data[pos..], n)
}

/// Декодирование CM-потока (модели повторяют ход кодера).
pub fn cmz_decode(data: &[u8], n: usize) -> Result<Vec<u8>, &'static str> {    let mut st = CmState::new();
    let mut rc = RcDec::new(data);
    let mut buf: Vec<u8> = Vec::with_capacity(n);
    for _ in 0..n {
        let i = buf.len();
        st.mm.boundary(&buf, i, &mut st.hctx);
        let mut c0 = 1u32;
        let mut b = 0u8;
        for j in 0..8 {
            let p = st.predict(j, c0, &buf);
            let p16 = (p << 4).clamp(16, 65520);
            let v = rc.thr(65536).min(65535);
            let bit = if v < p16 { 1 } else { 0 };
            if bit == 1 {
                rc.update(0, p16, 65536);
            } else {
                rc.update(p16, 65536 - p16, 65536);
            }
            st.update(j, c0, bit, &buf, i + 1);
            c0 = c0 * 2 + bit;
            b = (b << 1) + bit as u8;
        }
        st.byte_done(b);
        buf.push(b);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(data: &[u8]) {
        let out = cmz_encode(data).unwrap();
        let back = cmz_decode(&out, data.len()).unwrap();
        assert_eq!(back, data);
    }

    #[test]
    fn cmz_roundtrip_various() {
        rt(&[]);
        rt(b"a");
        rt(&vec![0xFD; 1000]);
        rt(&[0x00, 0x05, 0x81, 0x9F, 0xFC, 0x10, 0xFD, 0x42]);
    }

    #[test]
    fn cmz_compresses_repeats() {
        // повторяющаяся структура ctrl — match-модель и контексты должны сжать
        let mut rep = Vec::new();
        for _ in 0..300 {
            rep.extend_from_slice(&[0x00, 0x03, 0x85, 0x9A, 0xFD, 0xFC, 0x04, 0xD0, 0xB0]);
        }
        rt(&rep);
        let out = cmz_encode(&rep).unwrap();
        assert!(
            out.len() < rep.len() / 4,
            "cmz должен сжимать повторы: {} → {}",
            rep.len(),
            out.len()
        );
    }

    #[test]
    fn cmz_handles_marker_boundary_bytes() {
        // байты 0xFC/0xFD/0x00 внутри разных позиций и частичный байт-эскейп
        let mut c = Vec::new();
        for b in 0u8..255 {
            c.push(b);
        }
        rt(&c);
    }
}
