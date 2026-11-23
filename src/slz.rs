//! SLZ (PS22): быстрый универсальный LZ77+Huffman кодек класса zstd-lite.
//!
//! Цель: в разы быстрее xz по сжатию при качестве на уровне gzip и лучше,
//! универсальность по типам данных (текст, бинарные, БД-записи), окно
//! 16 МиБ для дальних повторов (у gzip всего 32 КиБ), конверт SU21
//! сверху для целостности.
//!
//! Поток: `SLZ1` + uvarint(исходная длина) + uvarint(чанков). Чанк — до
//! 1 МиБ входа (таблицы Хаффмана адаптивны по чанкам; хеш-таблицы окна —
//! глобальные, дальние повторы между чанками находятся). В чанке:
//! литералы (канонический Хаффман, 2^15-таблица декода) + последовательности
//! с тремя Хаффман-таблицами: ран литералов, селектор офсета (0..3 — слот
//! MTF-кэша, 4+log2 — новый офсет), длина совпадения; экстра-биты идут
//! выровненным байтовым потоком в порядке следования.

const MINMATCH: usize = 4;
const MAXMATCH: usize = 1 << 20;
const WINDOW: usize = 1 << 24;
const BLOCK: usize = 1 << 20;
const HASH_BITS: u32 = 22;
const MAX_CODE_LEN: u32 = 15;
const MTF_SLOTS: usize = 4;
const NOPOS: u32 = u32::MAX;
/// символы 0..15 — точные значения, 16+log2 — бакет с экстра-битами
const EXACT: u32 = 16;
/// селектор rep0: совпадение по «последнему офсету» mtf[0] — офсет не
/// передаётся и MTF не сдвигается. 0..3 — слоты MTF, 4+log — новый офсет
/// (log ≤ 24 → sel ≤ 28), 29 остаётся свободным
const REP_SEL: u32 = 29;

pub const MAGIC: &[u8; 4] = b"SLZ2";
/// PS24.1: словарный режим. SLZ3 = `SLZ3` + uvarint(исходная длина) +
/// uvarint(длина словаря) + uvarint(CRC-64 словаря) + uvarint(чанков) +
/// чанки. Содержимое словаря в поток НЕ пишется — обе стороны передают
/// его отдельно (--dict); энкодер прогревает хеш-цепочки словарём
/// (prime-проход, эмиссии нет), совпадения могут ссылаться на хвост
/// словаря (офсет больше позиции в выходе). Магия SLZ2 не переиспользуется,
/// поэтому декодер PS23.4 честно отвергает SLZ3 («магия не SLZ2») —
/// откат на старый релиз безопасен.
pub const MAGIC3: &[u8; 4] = b"SLZ3";

/// пресеты уровней скорости: (глубина цепочки, lazy-глубина, GOOD_LEN,
/// lazy2 — проверять i+1 даже при уже найденном совпадении). Уровень 1 —
/// greedy без lazy (прямой конкурент lz4), 6 — дефолт PS23.2.
fn level_params(level: usize) -> (usize, usize, usize, bool) {
    match level {
        0 | 1 => (8, 0, 16, false),
        2 | 3 => (16, 8, 24, false),
        4..=9 => (32, 16, 48, false),
        10..=14 => (64, 32, 96, true),
        _ => (128, 48, 256, true),
    }
}

#[inline]
fn put_uvarint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
fn get_uvarint(data: &[u8], pos: &mut usize) -> Result<u64, &'static str> {
    let mut v = 0u64;
    let mut shift = 0u32;
    loop {
        let b = *data.get(*pos).ok_or("slz: обрыв varint")?;
        *pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b < 0x80 {
            return Ok(v);
        }
        shift += 7;
        // PS24.1: в заголовке SLZ3 едет CRC-64 словаря — полное u64 требует
        // до 10 байт varint (старый лимит 56 бит резал старший бит хеша)
        if shift > 63 {
            return Err("slz: varint слишком длинный");
        }
    }
}

/// MSB-first битовый писатель.
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter { out: Vec::new(), acc: 0, nbits: 0 }
    }
    #[inline]
    fn write(&mut self, code: u32, len: u32) {
        debug_assert!(len == 0 || (code >> len) == 0);
        self.acc = (self.acc << len) | code as u64;
        self.nbits += len;
        while self.nbits >= 8 {
            self.nbits -= 8;
            self.out.push((self.acc >> self.nbits) as u8);
        }
    }
    fn flush(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            let pad = 8 - self.nbits;
            self.out.push(((self.acc << pad) as u8) & 0xFF);
        }
        self.out
    }
}

/// Читатель с окном 64 бита и 15-битным подсмотром для таблицы Хаффмана.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    nbits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, acc: 0, nbits: 0 }
    }
    #[inline]
    fn refill(&mut self) {
        while self.nbits <= 56 && self.pos < self.data.len() {
            self.acc = (self.acc << 8) | self.data[self.pos] as u64;
            self.pos += 1;
            self.nbits += 8;
        }
    }
    #[inline]
    fn peek(&mut self, n: u32) -> u32 {
        self.refill();
        if self.nbits == 0 {
            return 0;
        }
        let take = n.min(self.nbits);
        let v = ((self.acc >> (self.nbits - take)) & ((1u64 << take) - 1)) as u32;
        if take < n {
            // конец потока: дополняем нулями снизу, код выровнен по старшим битам
            v << (n - take)
        } else {
            v
        }
    }
    #[inline]
    fn consume(&mut self, n: u32) {
        let take = n.min(self.nbits);
        self.nbits -= take;
        self.acc &= (1u64 << self.nbits) - 1;
    }
    #[inline]
    fn decode_sym(&mut self, table: &DecodeTable) -> Result<u8, &'static str> {
        let snap = (self.acc, self.nbits, self.pos);
        let r = self.decode_sym_new(table);
        #[cfg(test)]
        if let Some(full) = &table.full {
            let post = (self.acc, self.nbits, self.pos);
            self.acc = snap.0;
            self.nbits = snap.1;
            self.pos = snap.2;
            let old = self.decode_sym_full(full);
            let fullpost = (self.acc, self.nbits, self.pos);
            // позиции могут расходиться на фазу refill (лишний байт в acc) —
            // сравниваем только фактически потреблённые биты
            let c_new = post.2 as i64 * 8 - post.1 as i64;
            let c_old = fullpost.2 as i64 * 8 - fullpost.1 as i64;
            if r != old || c_new != c_old {
                let (a, nb, p) = snap;
                let mut probe = BitReader { data: self.data, pos: p, acc: a, nbits: nb };
                let idx10 = probe.peek(10);
                let e10 = table.fast[idx10 as usize];
                let mut probe2 = BitReader { data: self.data, pos: p, acc: a, nbits: nb };
                let idx15 = probe2.peek(15);
                let e15 = full[idx15 as usize];
                panic!(
                    "расхождение: r={r:?} old={old:?} idx10={idx10:#x} fast={e10:#06x} idx15={idx15:#x} full={e15:#06x}"
                );
            }
            self.acc = post.0;
            self.nbits = post.1;
            self.pos = post.2;
        }
        r
    }

    fn decode_sym_full(&mut self, table: &[u16]) -> Result<u8, &'static str> {
        let packed = table[self.peek(MAX_CODE_LEN) as usize];
        let len = (packed & 0xF) as u32;
        if len == 0 {
            return Err("slz: недопустимый код Хаффмана");
        }
        self.consume(len);
        Ok((packed >> 4) as u8)
    }

    fn decode_sym_new(&mut self, table: &DecodeTable) -> Result<u8, &'static str> {
        // PS24.2: двухуровневая схема — короткие коды (≤10 бит) за один
        // lookup в fast-таблице 2^10 (в L1); длинные — 10 бит → индекс
        // под-таблицы, ещё 5 бит → символ. Старая таблица 2^15 (64 КиБ ×3)
        // жила в L2 и давала промах на каждый символ.
        let e = table.fast[self.peek(10) as usize];
        if e & 0x8000 == 0 {
            let len = (e & 0xF) as u32;
            if len == 0 {
                return Err("slz: недопустимый код Хаффмана");
            }
            self.consume(len);
            return Ok((e >> 4) as u8);
        }
        self.consume(10);
        let e2 = table.subs[(e & 0x7FFF) as usize][self.peek(5) as usize];
        let len = (e2 & 0xF) as u32;
        if len == 0 {
            return Err("slz: недопустимый код Хаффмана");
        }
        // десяток бит уже потреблён — добираем только остаток кода
        // (consume(len) здесь рассинхронизировал поток на 10 бит
        // на каждом длинном коде; поймано кросс-проверкой таблиц)
        self.consume(len - 10);
        Ok((e2 >> 4) as u8)
    }
}

/// Длины кодов Хаффмана с лимитом 15: при переполнении масштабируем частоты.
fn huff_lengths(freq: &[u32]) -> Vec<u8> {
    let mut f = freq.to_vec();
    loop {
        let mut nodes: Vec<u64> = Vec::new();
        let mut sym_of: Vec<u32> = Vec::new();
        let mut left: Vec<u32> = Vec::new();
        let mut right: Vec<u32> = Vec::new();
        for (s, &w) in f.iter().enumerate() {
            if w > 0 {
                nodes.push(w as u64);
                sym_of.push(s as u32);
            }
        }
        let nleaves = nodes.len();
        if nleaves == 0 {
            return vec![0u8; f.len()];
        }
        if nleaves == 1 {
            let mut lens = vec![0u8; f.len()];
            lens[sym_of[0] as usize] = 1;
            return lens;
        }
        let mut lens = vec![0u8; f.len()];
        let mut alive: Vec<bool> = vec![true; nleaves];
        let mut merged = 0;
        while merged < nleaves - 1 {
            let (mut a, mut b) = (usize::MAX, usize::MAX);
            for i in 0..nodes.len() {
                if alive[i] {
                    if a == usize::MAX || nodes[i] < nodes[a] {
                        b = a;
                        a = i;
                    } else if b == usize::MAX || nodes[i] < nodes[b] {
                        b = i;
                    }
                }
            }
            alive[a] = false;
            alive[b] = false;
            nodes.push(nodes[a] + nodes[b]);
            sym_of.push(u32::MAX);
            left.push(a as u32);
            right.push(b as u32);
            alive.push(true);
            merged += 1;
        }
        let mut overflow = false;
        let mut stack: Vec<(usize, u32)> = vec![(nodes.len() - 1, 0)];
        while let Some((n, d)) = stack.pop() {
            if d > MAX_CODE_LEN {
                overflow = true;
                break;
            }
            if sym_of[n] != u32::MAX {
                lens[sym_of[n] as usize] = d as u8;
            } else {
                let k = n - nleaves;
                stack.push((left[k] as usize, d + 1));
                stack.push((right[k] as usize, d + 1));
            }
        }
        if !overflow {
            return lens;
        }
        for v in f.iter_mut() {
            *v = (*v >> 1) | 1;
        }
    }
}

/// Канонические коды из длин (порядок: длина, затем символ).
fn canonical_codes(lens: &[u8]) -> Vec<u32> {
    let mut order: Vec<(u8, u32)> = lens
        .iter()
        .enumerate()
        .filter(|&(_, &l)| l > 0)
        .map(|(s, &l)| (l, s as u32))
        .collect();
    order.sort_unstable();
    let mut codes = vec![0u32; lens.len()];
    let mut code = 0u32;
    let mut prev_len = 0u8;
    for &(len, sym) in &order {
        code <<= (len - prev_len) as u32;
        codes[sym as usize] = code;
        code += 1;
        prev_len = len;
    }
    codes
}

/// Таблица декодирования PS24.2: fast 2^10 входов для кодов ≤10 бит
/// (sym << 4 | len; старший бит 0x8000 — ссылка на под-таблицу) и
/// под-таблицы 2^5 для длинных кодов 11–15 бит (индекс — следующие 5 бит).
/// Рабочий набор ~2 КиБ вместо 64 КиБ — весь decode_sym из L1.
struct DecodeTable {
    fast: Vec<u16>,
    subs: Vec<Vec<u16>>,
    /// только в тестах: старая полная 2^15 таблица для кросс-проверки
    full: Option<Vec<u16>>,
}

fn build_full_table(lens: &[u8], codes: &[u32]) -> Vec<u16> {
    let mut table = vec![0u16; 1usize << MAX_CODE_LEN];
    for (s, &len) in lens.iter().enumerate() {
        if len == 0 {
            continue;
        }
        let shift = MAX_CODE_LEN - len as u32;
        let start = (codes[s] << shift) as usize;
        let end = start + (1usize << shift);
        table[start..end].fill(((s as u16) << 4) | len as u16);
    }
    table
}

fn decode_table(lens: &[u8], codes: &[u32]) -> DecodeTable {
    let mut fast = vec![0u16; 1usize << 10];
    let mut subs: Vec<Vec<u16>> = Vec::new();
    let mut prefixes: Vec<(u32, u16)> = Vec::new(); // (prefix10, индекс)
    for (s, &len) in lens.iter().enumerate() {
        if len == 0 {
            continue;
        }
        let len = len as u32;
        if len <= 10 {
            let shift = 10 - len;
            let start = (codes[s] << shift) as usize;
            let end = start + (1usize << shift);
            fast[start..end].fill(((s as u16) << 4) | len as u16);
        } else {
            let prefix = (codes[s] >> (len - 10)) as usize;
            let idx = match prefixes.iter().find(|(p, _)| *p as usize == prefix) {
                Some((_, i)) => *i,
                None => {
                    let i = subs.len() as u16;
                    subs.push(vec![0u16; 1 << 5]);
                    prefixes.push((prefix as u32, i));
                    // fast индексируется 10 битами peek — один вход на префикс
                    fast[prefix] = 0x8000 | i;
                    i
                }
            };
            // оставшиеся биты кода (len-10 ≤ 5), дополненные нулями снизу:
            // индекс в под-таблице = следующие 5 бит потока
            let low = (codes[s] & ((1u32 << (len - 10)) - 1)) as usize;
            let shift2 = MAX_CODE_LEN - len; // 0..4
            let start2 = low << shift2;
            let end2 = start2 + (1usize << shift2);
            subs[idx as usize][start2..end2].fill(((s as u16) << 4) | len as u16);
        }
    }
    DecodeTable {
        fast,
        subs,
        full: if cfg!(test) {
            Some(build_full_table(lens, codes))
        } else {
            None
        },
    }
}

/// Разложение значения на символ Хаффмана и экстра-биты:
/// value < EXACT → символ = value, экстра нет; иначе символ = EXACT + log2,
/// экстра = value − 2^log2 в log2 битах. Возвращает (символ, биты, значение).
#[inline]
fn bucket(value: u32) -> (u32, u32, u32) {
    if value < EXACT {
        (value, 0, 0)
    } else {
        let log = 31 - value.leading_zeros();
        (EXACT + log, log, value - (1 << log))
    }
}

/// CRC32-инструкция (sse4.2) даёт более равномерное распределение и
/// дешевле умножения; без sse4.2 — multiply-shift fallback.
#[cfg(target_arch = "x86_64")]
static SSE42: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[cfg(target_arch = "x86_64")]
#[inline]
fn has_sse42() -> bool {
    *SSE42.get_or_init(|| std::arch::is_x86_feature_detected!("sse4.2"))
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.2")]
unsafe fn crc32_u32(v: u32) -> u32 {
    core::arch::x86_64::_mm_crc32_u32(0, v)
}

/// prefetch (T0): идея проверена замерами и отклонена — на глубокой
/// цепочке кандидатов fb2 он давал +8…15% ко времени enc (данные почти
/// всегда уже в кэше, а инструкции prefetch стоят тактов). Функция
/// удалена; CRC32-хеш и unchecked-загрузки остались.

#[inline]
fn hash4(data: &[u8], i: usize) -> usize {
    debug_assert!(i + 4 <= data.len());
    // PS24.2: unchecked-загрузка — границы доказаны вызывающими
    // (insert/best_at/prime проверяют i+4 ≤ len); срез data[i..i+4]
    // стоил проверку границ на каждый вызов хеша
    let v = u32::from_le_bytes(unsafe {
        (data.as_ptr().add(i) as *const [u8; 4]).read_unaligned()
    });
    #[cfg(target_arch = "x86_64")]
    {
        if has_sse42() {
            // crc32 — биекция на u32, любые биты распределены равномерно
            (unsafe { crc32_u32(v) } >> (32 - HASH_BITS)) as usize
        } else {
            (v.wrapping_mul(2654435761) >> (32 - HASH_BITS)) as usize
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        (v.wrapping_mul(2654435761) >> (32 - HASH_BITS)) as usize
    }
}

struct Seq {
    /// литеральный ран перед этим совпадением
    run: u32,
    /// REP_SEL — rep0; 0..3 — слот MTF; 4+log — новый офсет с остатком
    sel: u32,
    extra_off: u32,
    len_m: u32, // len - MINMATCH
}

/// Токенизатор с глобальным окном: head/prev/mtf живут между чанками,
/// поэтому дальние повторы через границу чанка находятся.
struct Tok {
    head: Vec<u32>,
    /// PS24.1: отдельная таблица кандидатов из словаря — цепочки входа не
    /// загрязняются повторами словаря (как dictMatchState у zstd); пустая
    /// вектор = режим без словаря, ноль накладных расходов
    head_dict: Vec<u32>,
    prev: Vec<u32>,
    /// PS24.2: маска кольца prev = prev.len() - 1. Кольцо ужато до
    /// степени двойки ≥ длины входа: для файлов ≤16 МиБ ни один слот
    /// не перезаписывается — поведение идентично WINDOW-кольцу, а CLI
    /// на малых файлах ест память по размеру данных, а не 64 МиБ всегда
    mask: usize,
    mtf: [u32; MTF_SLOTS],
    i: usize,
    lit_run: u32,
    /// параметры уровня скорости (см. level_params)
    depth: usize,
    lazy_depth: usize,
    good_len: usize,
    /// lazy2: lazy-проверка i+1 даже при найденном совпадении (уровни 12+)
    lazy2: bool,
}

impl Tok {
    fn with_params(
        depth: usize,
        lazy_depth: usize,
        good_len: usize,
        lazy2: bool,
        prev_size: usize,
    ) -> Self {
        Tok {
            head: vec![NOPOS; 1usize << HASH_BITS],
            head_dict: Vec::new(),
            prev: vec![NOPOS; prev_size],
            mask: prev_size - 1,
            mtf: [1, 2, 3, 4],
            i: 0,
            lit_run: 0,
            depth,
            lazy_depth,
            good_len,
            lazy2,
        }
    }

    /// PS24.1: прогрев словарём — позиции в head_dict, эмиссии нет. Вставка
    /// в обратном порядке: головой цепочки становится самая ранняя позиция
    /// словаря (вход обычно ссылается на словарь в порядке роста записей).
    fn prime(&mut self, data: &[u8], end: usize) {
        if self.head_dict.is_empty() {
            self.head_dict = vec![NOPOS; 1usize << HASH_BITS];
        }
        for i in (0..end).rev() {
            if i + MINMATCH <= data.len() {
                let h = hash4(data, i);
                self.prev[i & self.mask] = self.head_dict[h];
                self.head_dict[h] = i as u32;
            }
        }
        self.i = end;
    }

    #[inline]
    fn insert(&mut self, input: &[u8], i: usize) {
        if i + MINMATCH <= input.len() {
            let h = hash4(input, i);
            self.prev[i & self.mask] = self.head[h];
            self.head[h] = i as u32;
        }
    }

    /// вставка позиций совпадения в хеш-цепочки; при длинном совпадении
    /// вставляем только края (по 16 позиций) — середина рана находит
    /// совпадение через край, а на RLE-участках это экономит до 90%
    /// вставок (иначе 1 МиБ нулей = 1 МиБ вставок)
    #[inline]
    fn insert_match(&mut self, input: &[u8], from: usize, len: usize) {
        if len <= 32 {
            for k in from..from + len {
                self.insert(input, k);
            }
        } else {
            for k in from..from + 16 {
                self.insert(input, k);
            }
            for k in from + len - 16..from + len {
                self.insert(input, k);
            }
        }
    }

    /// длина совпадения входа с позиции p с кандидатом c (u64-словами:
    /// 8 байт за шаг, хвост — побайтово; границы доказаны: p+maxlen <= n,
    /// c < p => c+maxlen < n)
    fn extend_len(&self, input: &[u8], c: usize, p: usize) -> usize {
        let maxlen = (input.len() - p).min(MAXMATCH);
        debug_assert!(c < p && p + maxlen <= input.len());
        let mut l = 0usize;
        while l + 8 <= maxlen {
            let a = u64::from_le_bytes(unsafe {
                (input.as_ptr().add(c + l) as *const [u8; 8]).read_unaligned()
            });
            let b = u64::from_le_bytes(unsafe {
                (input.as_ptr().add(p + l) as *const [u8; 8]).read_unaligned()
            });
            let x = a ^ b;
            if x == 0 {
                l += 8;
            } else {
                l += (x.trailing_zeros() >> 3) as usize;
                break;
            }
        }
        while l < maxlen && input[c + l] == input[p + l] {
            l += 1;
        }
        l
    }

    /// длина совпадения по «последнему офсету» (rep0): офсет mtf[0],
    /// при кодировании он бесплатен — селектор один вместо слота+длины
    fn rep_len(&self, input: &[u8], p: usize) -> usize {
        let off = self.mtf[0] as usize;
        if off == 0 || p < off || p + MINMATCH > input.len() {
            return 0;
        }
        self.extend_len(input, p - off, p)
    }

    /// минимальная длина совпадения по расстоянию: дальние короткие
    /// совпадения дороже литералов — офсет >1 МиБ должен покрыть ≥5 Б,
    /// >4 МиБ — ≥8 Б, иначе кандидат пропускается
    #[inline]
    fn min_match_len(dist: usize) -> usize {
        if dist > (1 << 22) {
            8
        } else if dist > (1 << 20) {
            5
        } else {
            MINMATCH
        }
    }

    /// лучший кандидат с позиции p: (len, off). Две цепочки: вход (head) и
    /// словарь (head_dict, PS24.1).
    fn best_at(&self, input: &[u8], p: usize, depth: usize) -> (usize, usize) {
        if p + MINMATCH > input.len() {
            return (0, 0);
        }
        let (mut bl, mut bo) = (0usize, 0usize);
        let mut cand = self.head[hash4(input, p)];
        let mut d = depth;
        while cand != NOPOS && d > 0 {
            let c = cand as usize;
            if p - c > WINDOW {
                break;
            }
            let l = self.extend_len(input, c, p);
            let minlen = Self::min_match_len(p - c);
            if l >= minlen && l > bl {
                bl = l;
                bo = p - c;
                if bl >= self.good_len {
                    break;
                }
            }
            cand = self.prev[c & self.mask];
            d -= 1;
        }
        if !self.head_dict.is_empty() {
            let mut cand = self.head_dict[hash4(input, p)];
            let mut d = depth;
            while cand != NOPOS && d > 0 {
                let c = cand as usize;
                if p - c > WINDOW {
                    break;
                }
                let l = self.extend_len(input, c, p);
                let minlen = Self::min_match_len(p - c);
                if l >= minlen && l > bl {
                    bl = l;
                    bo = p - c;
                    if bl >= self.good_len {
                        break;
                    }
                }
                cand = self.prev[c & self.mask];
                d -= 1;
            }
        }
        (bl, bo)
    }

    fn emit(&mut self, seqs: &mut Vec<Seq>, off: usize, len: usize) {
        let len_m = (len - MINMATCH) as u32;
        if let Some(s) = self.mtf.iter().position(|&o| o as usize == off) {
            seqs.push(Seq { run: self.lit_run, sel: s as u32, extra_off: 0, len_m });
            let v = self.mtf[s];
            self.mtf.copy_within(0..s, 1);
            self.mtf[0] = v;
        } else {
            let log = 31 - (off as u32).leading_zeros();
            let rest = (off as u32) & ((1u32 << log) - 1);
            seqs.push(Seq { run: self.lit_run, sel: 4 + log, extra_off: rest, len_m });
            for k in (1..MTF_SLOTS).rev() {
                self.mtf[k] = self.mtf[k - 1];
            }
            self.mtf[0] = off as u32;
        }
        self.lit_run = 0;
    }

    /// rep0-совпадение: селектор REP_SEL, офсет mtf[0] не передаётся и
    /// MTF не сдвигается (офсет остаётся «последним» для следующих rep0)
    fn emit_rep(&mut self, seqs: &mut Vec<Seq>, len: usize) {
        let len_m = (len - MINMATCH) as u32;
        seqs.push(Seq {
            run: self.lit_run,
            sel: REP_SEL,
            extra_off: 0,
            len_m,
        });
        self.lit_run = 0;
    }

    /// Токенизация до target_end (совпадения могут пересекать границу чанка).
    fn run_chunk(&mut self, input: &[u8], target_end: usize) -> (Vec<u8>, Vec<Seq>) {
        let n = input.len();
        let mut lits: Vec<u8> = Vec::new();
        let mut seqs: Vec<Seq> = Vec::new();
        while self.i < target_end {
            let i = self.i;
            if i + MINMATCH > n {
                lits.push(input[i]);
                self.lit_run += 1;
                self.i += 1;
                continue;
            }
            // rep0: сильное совпадение по последнему офсету (≥8) принимаем
            // сразу — обход цепочек кандидатов не нужен
            let rl = self.rep_len(input, i);
            if rl >= 8 {
                self.emit_rep(&mut seqs, rl);
                self.insert_match(input, i, rl);
                self.i += rl;
                continue;
            }
            let (bl, bo) = self.best_at(input, i, self.depth);
            // rep0 против кандидата. Стоимость обычного офсета ~log2(off)
            // экстра-бит, литерал ~5.5 бит, но близкий тай-брейк rep'ом
            // фрагментирует длинные матчи (замер fb2: enc −32%) — поэтому:
            // rep берём при равной длине (та же покрышка, минус символ) и
            // при кандидатe лишь незначительно длиннее, но только если
            // кандидат ДАЛЁКИЙ (off > 64 КиБ, где экстра-бит много)
            let rep_wins = rl >= MINMATCH
                && (bl < rl
                    || (bl == rl && bo > 0)
                    || (bo > (1 << 16)
                        && bl < rl + (32 - (bo as u32).leading_zeros() as usize + 3) / 5));
            if rep_wins {
                // слабый rep0 — берём, только если он длиннее кандидата
                self.emit_rep(&mut seqs, rl);
                self.insert_match(input, i, rl);
                self.i += rl;
                continue;
            }
            // lazy-1: короткое совпадение — проверяем позицию i+1; lazy2
            // (высокие уровни) проверяет i+1 и при найденном совпадении:
            // длинный матч может блокировать ещё более длинный шагом дальше.
            // PS24.2: rep0 на lazy-позиции проверен замером и ОТКЛОНЁН —
            // enc −16…17% на fb2/m10 при выигрыше 0.0–0.04 п.п. (rep почти
            // всегда совпадает с кандидатом через шаг, а extend_len на
            // rep-офсете читает холодные строки кэша)
            if self.lazy_depth > 0
                && (self.lazy2 || bl < self.good_len)
                && i + 1 + MINMATCH <= n
            {
                let (l2, o2) = self.best_at(input, i + 1, self.lazy_depth);
                if l2 >= MINMATCH && l2 > bl {
                    lits.push(input[i]);
                    self.lit_run += 1;
                    self.insert(input, i);
                    self.i += 1;
                    self.emit(&mut seqs, o2, l2);
                    self.insert_match(input, self.i, l2);
                    self.i += l2;
                    continue;
                }
            }
            if bl >= MINMATCH {
                self.emit(&mut seqs, bo, bl);
                self.insert_match(input, i, bl);
                self.i += bl;
            } else {
                lits.push(input[i]);
                self.lit_run += 1;
                self.insert(input, i);
                self.i += 1;
            }
        }
        // хвостовые литералы чанка идут в «хвост» этого же чанка (декодер
        // выводит их целиком), счётчик рана не должен перетекать в следующий
        self.lit_run = 0;
        (lits, seqs)
    }
}

/// Кодирование полного SLZ-потока уровнем по умолчанию (без конверта SU21).
pub fn encode(input: &[u8]) -> Vec<u8> {
    encode_level(input, 6)
}

/// PS24.1: кодирование со словарём (формат SLZ3). Словарь прогревает
/// хеш-цепочки (prime-проход по dict без эмиссии последовательностей),
/// после чего обычная токенизация входа находит совпадения, источники
/// которых лежат в словаре. Офсеты — расстояния в виртуальном потоке
/// `dict ++ input`, поэтому декодер при off > позиции выхода читает
/// из хвоста словаря. Пустой вход или пустой словарь → обычный SLZ2.
pub fn encode_dict(input: &[u8], dict: &[u8], level: usize) -> Vec<u8> {
    if input.is_empty() || dict.is_empty() {
        return encode_level(input, level);
    }
    let n = input.len();
    let dn = dict.len();
    let mut full = Vec::with_capacity(dn + n);
    full.extend_from_slice(dict);
    full.extend_from_slice(input);
    let mut out = Vec::with_capacity(n / 2 + 64);
    out.extend_from_slice(MAGIC3);
    put_uvarint(n as u64, &mut out);
    put_uvarint(dn as u64, &mut out);
    put_uvarint(crate::integrity::crc64(dict), &mut out);
    let nchunks = n.div_ceil(BLOCK).max(1);
    put_uvarint(nchunks as u64, &mut out);
    let (depth, lazy_depth, good_len, lazy2) = level_params(level);
    // PS24.2: кольцо prev — степень двойки от длины (dict + вход);
    // для файлов ≤16 МиБ это идентично WINDOW-кольцу без перезаписей
    let prev_size = (dn + n).next_power_of_two().min(WINDOW);
    let mut tok = Tok::with_params(depth, lazy_depth, good_len, lazy2, prev_size);
    // prime-проход: позиции словаря в отдельную таблицу head_dict (в
    // обратном порядке), эмиссии нет — декодер имеет тот же словарь
    tok.prime(&full, dn);
    for _ in 0..nchunks {
        let chunk_start = tok.i;
        let target_end = (tok.i + BLOCK).min(dn + n);
        let chunk_len = target_end - chunk_start;
        let (lits, seqs) = tok.run_chunk(&full, target_end);
        let mut enc = Vec::with_capacity(chunk_len / 2 + 512);
        encode_chunk(&mut enc, &lits, &seqs);
        if enc.len() < chunk_len {
            out.push(0);
            out.extend_from_slice(&enc);
        } else {
            out.push(1);
            out.extend_from_slice(&full[chunk_start..target_end]);
            tok.mtf = [1, 2, 3, 4];
        }
    }
    out
}

/// Кодирование с уровнем скорости 1 (fast, конкурент lz4) … 19 (глубокие
/// цепочки + lazy). Формат потока от уровня не зависит.
pub fn encode_level(input: &[u8], level: usize) -> Vec<u8> {
    let n = input.len();
    let mut out = Vec::with_capacity(n / 2 + 64);
    out.extend_from_slice(MAGIC);
    put_uvarint(n as u64, &mut out);
    let nchunks = n.div_ceil(BLOCK).max(1);
    put_uvarint(nchunks as u64, &mut out);
    let (depth, lazy_depth, good_len, lazy2) = level_params(level);
    // PS24.2: кольцо prev — степень двойки от длины входа (см. Tok.mask)
    let prev_size = n.next_power_of_two().min(WINDOW);
    let mut tok = Tok::with_params(depth, lazy_depth, good_len, lazy2, prev_size);
    for _ in 0..nchunks {
        let chunk_start = tok.i;
        let target_end = (tok.i + BLOCK).min(n);
        let chunk_len = target_end - chunk_start;
        let (lits, seqs) = tok.run_chunk(input, target_end);
        // raw-escape: если чанк не сжался, пишем его как есть (флаг 1) —
        // декодер копирует chunk_len байт; гарантирует «не хуже хранения»
        let mut enc = Vec::with_capacity(chunk_len / 2 + 512);
        encode_chunk(&mut enc, &lits, &seqs);
        if enc.len() < chunk_len {
            out.push(0);
            out.extend_from_slice(&enc);
        } else {
            out.push(1);
            out.extend_from_slice(&input[chunk_start..target_end]);
            // токены этого чанка выброшены — ресет MTF синхронен декодеру
            tok.mtf = [1, 2, 3, 4];
        }
    }
    out
}

/// Частоты бакетов: раны литералов, селекторы (включая rep0=29), длины.
fn seq_freqs(
    seqs: &[Seq],
) -> (
    [u32; EXACT as usize + 25],
    [u32; EXACT as usize + 14], // 30: слоты MTF + 4+log + rep0
    [u32; EXACT as usize + 22],
) {
    let mut fr = [0u32; EXACT as usize + 25];
    let mut fs = [0u32; EXACT as usize + 14];
    // len_m до MAXMATCH-4 → log до 20 → символ EXACT+20=36; запас до 21
    let mut fl = [0u32; EXACT as usize + 22];
    for s in seqs {
        fr[bucket(s.run).0 as usize] += 1;
        fs[s.sel as usize] += 1;
        fl[bucket(s.len_m).0 as usize] += 1;
    }
    (fr, fs, fl)
}

fn encode_chunk(out: &mut Vec<u8>, lits: &[u8], seqs: &[Seq]) {
    put_uvarint(lits.len() as u64, out);
    put_uvarint(seqs.len() as u64, out);
    if !lits.is_empty() {
        let mut freq = vec![0u32; 256];
        for &b in lits {
            freq[b as usize] += 1;
        }
        let lens = huff_lengths(&freq);
        let codes = canonical_codes(&lens);
        let mut bw = BitWriter::new();
        for &b in lits {
            bw.write(codes[b as usize], lens[b as usize] as u32);
        }
        let lit_bits = bw.flush();
        put_uvarint(lit_bits.len() as u64, out);
        out.extend_from_slice(&lens);
        out.extend_from_slice(&lit_bits);
    }
    if seqs.is_empty() {
        return;
    }
    let (fr, fs, fl) = seq_freqs(seqs);
    let lr = huff_lengths(&fr);
    let ls = huff_lengths(&fs);
    let ll = huff_lengths(&fl);
    let cr = canonical_codes(&lr);
    let cs = canonical_codes(&ls);
    let cl = canonical_codes(&ll);
    let mut rbw = BitWriter::new();
    let mut sbw = BitWriter::new();
    let mut lbw = BitWriter::new();
    let mut extra: Vec<u8> = Vec::with_capacity(seqs.len() * 4);
    let put_extra = |extra: &mut Vec<u8>, bits: u32, val: u32| {
        if bits > 0 {
            let n = bits.div_ceil(8);
            for k in 0..n {
                extra.push((val >> (8 * k)) as u8);
            }
        }
    };
    for s in seqs {
        let (sym, bits, val) = bucket(s.run);
        rbw.write(cr[sym as usize], lr[sym as usize] as u32);
        put_extra(&mut extra, bits, val);
        sbw.write(cs[s.sel as usize], ls[s.sel as usize] as u32);
        // rep0 (REP_SEL) не имеет экстра-бит: офсет не передаётся вообще
        if s.sel >= 4 && s.sel != REP_SEL {
            let log = s.sel - 4;
            put_extra(&mut extra, log, s.extra_off);
        }
        let (lsym, lbits, lval) = bucket(s.len_m);
        lbw.write(cl[lsym as usize], ll[lsym as usize] as u32);
        put_extra(&mut extra, lbits, lval);
    }
    let rbits = rbw.flush();
    let sbits = sbw.flush();
    let lbits = lbw.flush();
    // каждая таблица со своим потоком: [α][длины][размер потока][поток]
    for (lens_arr, bits) in [(&lr, &rbits), (&ls, &sbits), (&ll, &lbits)] {
        put_uvarint(lens_arr.len() as u64, out);
        out.extend_from_slice(lens_arr);
        put_uvarint(bits.len() as u64, out);
        out.extend_from_slice(bits);
    }
    put_uvarint(extra.len() as u64, out);
    out.extend_from_slice(&extra);
}

/// Декодирование SLZ-потока. `max_output` — предел размера выхода;
/// возвращает исходные байты.
pub fn decode(data: &[u8], max_output: usize) -> Result<Vec<u8>, &'static str> {
    decode_stream(data, &[], max_output)
}

/// PS24.1: декодирование SLZ3-потока со словарём. Словарь проверяется
/// по длине и CRC-64; совпадения с off > позиции выхода читаются из
/// его хвоста.
pub fn decode_dict(
    data: &[u8],
    dict: &[u8],
    max_output: usize,
) -> Result<Vec<u8>, &'static str> {
    if dict.is_empty() {
        return decode(data, max_output);
    }
    decode_stream(data, dict, max_output)
}

fn decode_stream(data: &[u8], dict: &[u8], max_output: usize) -> Result<Vec<u8>, &'static str> {
    let dict_mode = !dict.is_empty();
    if data.len() < 7 || &data[..4] != (if dict_mode { MAGIC3 } else { MAGIC }) {
        return Err(if dict_mode {
            "slz: магия не SLZ3"
        } else {
            "slz: магия не SLZ2"
        });
    }
    let mut pos = 4usize;
    let orig_len = get_uvarint(data, &mut pos)? as usize;
    if orig_len > max_output {
        return Err("slz: заявленная длина превышает лимит");
    }
    if dict_mode {
        let dict_len = get_uvarint(data, &mut pos)? as usize;
        let dict_crc = get_uvarint(data, &mut pos)?;
        if dict_len != dict.len() || crate::integrity::crc64(dict) != dict_crc {
            return Err("slz: словарь не совпал (длина/CRC-64)");
        }
    }
    let nchunks = get_uvarint(data, &mut pos)? as usize;
    let mut out: Vec<u8> = Vec::with_capacity(orig_len);
    let mut mtf: [u32; MTF_SLOTS] = [1, 2, 3, 4];
    for _ in 0..nchunks {
        // флаг чанка: 0 — обычный (Хаффман), 1 — raw-escape (копия как есть)
        if *data.get(pos).ok_or("slz: обрыв флага чанка")? != 0 {
            pos += 1;
            let raw_len = BLOCK.min(orig_len.checked_sub(out.len()).ok_or("slz: выход превысил заявленную длину")?);
            if pos + raw_len > data.len() {
                return Err("slz: обрыв raw-чанка");
            }
            out.extend_from_slice(&data[pos..pos + raw_len]);
            pos += raw_len;
            // токены raw-чанка декодер не видит — MTF синхронно ресетится
            // на обеих сторонах (энкодер делает то же при escape)
            mtf = [1, 2, 3, 4];
            continue;
        }
        pos += 1;
        let lit_count = get_uvarint(data, &mut pos)? as usize;
        let seq_count = get_uvarint(data, &mut pos)? as usize;
        // литералы чанка — отдельный буфер с перемежением ранами
        let mut lits: Vec<u8> = Vec::with_capacity(lit_count.min(1 << 20));
        if lit_count > 0 {
            let lbits_len = get_uvarint(data, &mut pos)? as usize;
            if pos + 256 > data.len() {
                return Err("slz: обрыв таблицы Хаффмана");
            }
            let lens: [u8; 256] = data[pos..pos + 256].try_into().unwrap();
            pos += 256;
            if pos + lbits_len > data.len() {
                return Err("slz: обрыв литералов");
            }
            let codes = canonical_codes(&lens);
            let table = decode_table(&lens, &codes);
            let mut br = BitReader::new(&data[pos..pos + lbits_len]);
            pos += lbits_len;
            for _ in 0..lit_count {
                lits.push(br.decode_sym(&table)?);
            }
        }
        if seq_count > 0 {
            // три таблицы: раны, селекторы, длины
            let mut tables = Vec::with_capacity(3);
            let mut streams: [&[u8]; 3] = [&[], &[], &[]];
            for t in 0..3 {
                let alpha = get_uvarint(data, &mut pos)? as usize;
                if pos + alpha > data.len() {
                    return Err("slz: обрыв таблицы последовательностей");
                }
                let lens = data[pos..pos + alpha].to_vec();
                pos += alpha;
                let blen = get_uvarint(data, &mut pos)? as usize;
                if pos + blen > data.len() {
                    return Err("slz: обрыв потока символов");
                }
                let codes = canonical_codes(&lens);
                tables.push(decode_table(&lens, &codes));
                streams[t] = &data[pos..pos + blen];
                pos += blen;
            }
            let extra_len = get_uvarint(data, &mut pos)? as usize;
            if pos + extra_len > data.len() {
                return Err("slz: обрыв экстра-бит");
            }
            let extra = &data[pos..pos + extra_len];
            pos += extra_len;
            let mut rb = BitReader::new(streams[0]);
            let mut sb = BitReader::new(streams[1]);
            let mut lb = BitReader::new(streams[2]);
            let mut ep = 0usize;
            let mut lit_pos = 0usize;
            for _ in 0..seq_count {
                // ран литералов перед совпадением
                let rsym = rb.decode_sym(&tables[0])? as u32;
                let run = if rsym < EXACT {
                    rsym as usize
                } else {
                    let k = rsym - EXACT;
                    let mut v = 0u32;
                    for j in 0..k.div_ceil(8) {
                        v |= (*extra.get(ep).ok_or("slz: обрыв экстра-бит")? as u32) << (8 * j);
                        ep += 1;
                    }
                    (((1u32 << k) | (v & ((1 << k) - 1))) as usize)
                };
                if lit_pos + run > lits.len() {
                    return Err("slz: литеральный ран превышает буфер литералов");
                }
                out.extend_from_slice(&lits[lit_pos..lit_pos + run]);
                lit_pos += run;
                // офсет
                let sel = sb.decode_sym(&tables[1])? as u32;
                let off: usize = if sel == REP_SEL {
                    // rep0: офсет — mtf[0], сам офсет не передаётся,
                    // MTF не сдвигается
                    mtf[0] as usize
                } else if (sel as usize) < MTF_SLOTS {
                    let s = sel as usize;
                    let v = mtf[s];
                    mtf.copy_within(0..s, 1);
                    mtf[0] = v;
                    v as usize
                } else {
                    let log = sel - 4;
                    let mut v = 0u32;
                    for j in 0..log.div_ceil(8) {
                        v |= (*extra.get(ep).ok_or("slz: обрыв экстра-бит")? as u32) << (8 * j);
                        ep += 1;
                    }
                    let v2 = ((1u32 << log) | (v & ((1 << log) - 1))) as usize;
                    for k in (1..MTF_SLOTS).rev() {
                        mtf[k] = mtf[k - 1];
                    }
                    mtf[0] = v2 as u32;
                    v2
                };
                // длина
                let lsym = lb.decode_sym(&tables[2])? as u32;
                let len_m = if lsym < EXACT {
                    lsym as usize
                } else {
                    let k = lsym - EXACT;
                    let mut v = 0u32;
                    for j in 0..k.div_ceil(8) {
                        v |= (*extra.get(ep).ok_or("slz: обрыв экстра-бит")? as u32) << (8 * j);
                        ep += 1;
                    }
                    (((1u32 << k) | (v & ((1 << k) - 1))) as usize)
                };
                let len = len_m + MINMATCH;
                if out.len() + len > orig_len {
                    return Err("slz: выход превышает заявленную длину");
                }
                if off > out.len() {
                    // источник за пределами выхода — в словаре (PS24.1)
                    if off > out.len() + dict.len() {
                        return Err("slz: офсет за пределами словаря");
                    }
                    let from_dict = off - out.len();
                    let base = out.len();
                    let ds = dict.len() - from_dict;
                    if from_dict >= len {
                        // источник целиком в словаре — прямой копир
                        out.extend_from_slice(&dict[ds..ds + len]);
                    } else {
                        // источник начинается в словаре и продолжается в
                        // уже записанный выход: прогрессирующая копия,
                        // источник растёт вместе с записью
                        out.resize(base + len, 0);
                        for k in 0..from_dict {
                            out[base + k] = dict[ds + k];
                        }
                        for k in from_dict..len {
                            out[base + k] = out[k - from_dict];
                        }
                    }
                } else if off >= len {
                    let start = out.len() - off;
                    out.extend_from_within(start..start + len);
                } else {
                    let start = out.len() - off;
                    // перекрывающийся повтор (off < len): off==1 — fill одним
                    // байтом; иначе u64-словами копируем первые off байт из
                    // исходных данных, хвост — из уже записанного участка
                    let base = out.len();
                    if off == 1 {
                        let b = out[start];
                        out.resize(base + len, b);
                    } else {
                        out.resize(base + len, 0);
                        let mut k = 0usize;
                        while k + 8 <= off {
                            out.copy_within(start + k..start + k + 8, base + k);
                            k += 8;
                        }
                        while k < off {
                            out[base + k] = out[start + k];
                            k += 1;
                        }
                        while k < len {
                            let nn = (len - k).min(off).min(8);
                            out.copy_within(base + k - off..base + k - off + nn, base + k);
                            k += nn;
                        }
                    }
                }
            }
            // хвостовые литералы чанка
            out.extend_from_slice(&lits[lit_pos..]);
        } else {
            out.extend_from_slice(&lits);
        }
    }
    if out.len() != orig_len {
        return Err("slz: длина выхода не совпала с заголовком");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(data: &[u8]) {
        let enc = encode(data);
        let dec = decode(&enc, data.len() + 1).expect("декодирование должно пройти");
        assert_eq!(dec, data, "roundtrip не сошёлся ({} Б → {} Б)", data.len(), enc.len());
    }

    #[test]
    fn roundtrip_empty_and_small() {
        rt(b"");
        rt(b"a");
        rt(b"aaaa");
        rt(&[0u8; 3]);
        rt(&[7u8; 16]); // run=16 — граница EXACT-бакета
    }

    #[test]
    fn roundtrip_all_byte_values() {
        let v: Vec<u8> = (0..=255u8).collect();
        rt(&v);
        rt(&v.repeat(64)); // RLE-совпадения через границу чанка не попадают (16К)
    }

    #[test]
    fn roundtrip_random_and_text() {
        let mut rng = 12345u64;
        let mut rnd = Vec::with_capacity(1 << 20);
        for _ in 0..1 << 20 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            rnd.push((rng >> 33) as u8);
        }
        rt(&rnd); // несжимаемые данные
        let mut text = Vec::new();
        for k in 0..100_000u32 {
            text.extend_from_slice(format!("строка {} с повторяющимся текстом\n", k % 997).as_bytes());
        }
        rt(&text); // текст с близкими и дальними повторами
    }

    #[test]
    fn far_repeat_across_window_edge() {
        let chunk: Vec<u8> = (0..(1 << 20)).map(|i| (i * 7 % 251) as u8).collect();
        let mut v = chunk.clone();
        v.extend(std::iter::repeat(0xABu8).take(100));
        v.extend_from_slice(&chunk); // дальний повтор с офсетом чуть больше 1 МиБ
        rt(&v);
    }

    #[test]
    fn match_crossing_chunk_boundary() {
        // 2.5 МиБ: повтор первых 1.2 МиБ в конце — совпадение начинается
        // в чанке 2 и уходит за его границу; офсет > 1 МиБ
        let first: Vec<u8> = (0..1_200_000).map(|i| ((i * 13) % 253) as u8).collect();
        let mut v = first.clone();
        v.extend_from_slice(&first[..900_000]);
        rt(&v);
    }

    #[test]
    fn long_and_boundary_matches() {
        for len in [MINMATCH - 1, MINMATCH, MINMATCH + 1, 15, 16, 17, 255, 256, 1000, MAXMATCH] {
            let mut v = vec![9u8; 300];
            v.extend(std::iter::repeat(7u8).take(len));
            v.extend_from_slice(&vec![9u8; 300]);
            rt(&v);
        }
    }

    #[test]
    fn decode_rejects_bad_input() {
        assert!(decode(b"", 100).is_err());
        assert!(decode(b"SLZ0\x01", 100).is_err());
        let enc = encode("повтор повтор повтор".as_bytes());
        assert!(decode(&enc, 4).is_err(), "лимит выхода должен срабатывать");
        let mut cut = enc.clone();
        cut.truncate(cut.len() / 2);
        assert!(decode(&cut, 1000).is_err(), "обрыв должен быть ошибкой");
    }

    #[test]
    fn overlapping_matches_reconstruct_rle() {
        let v = vec![5u8; 10_000]; // сплошной перекрывающийся повтор
        rt(&v);
    }

    #[test]
    fn runs_longer_than_64k() {
        // раны > прежнего предела MAXMATCH=65535: чередование проверяет
        // и разбиение длинных совпадений на токены, и «редкие вставки»
        let mut v = Vec::new();
        for _ in 0..3 {
            v.extend(std::iter::repeat(b'a').take(70_000));
            v.extend_from_slice(b"SEP\n");
            v.extend(std::iter::repeat(b' ').take(130_000));
            v.extend_from_slice(b";");
        }
        rt(&v);
    }

    /// Живое сравнение с lz4/snappy (зависимости в Cargo.toml существуют
    /// только для бенчей и не влияют на формат): смешанные данные ~768 КиБ
    /// (RLE + текстовый паттерн + периодика + псевдослучайное); печатает
    /// таблицу размеров, проверяет roundtrip и «не хуже худшего из
    /// fast-конкурентов».
    #[test]
    fn live_codec_comparison() {
        let mut v = Vec::with_capacity(1 << 20);
        v.extend(std::iter::repeat(0u8).take(1 << 16)); // RLE
        v.extend(
            b"the quick brown fox jumps over the lazy dog. "
                .iter()
                .cycle()
                .take(1 << 17),
        ); // текстовый паттерн
        v.extend((0u32..(1 << 19)).map(|i| (i.wrapping_mul(2654435761) % 251) as u8)); // периодика
        v.extend((0u64..(1 << 18)).map(|i| (i.wrapping_mul(6364136223846793005) >> 33) as u8)); // псевдослучайное
        let slz = encode(&v);
        let lz4 = lz4_flex::compress_prepend_size(&v);
        let snappy = snap::raw::Encoder::new().compress_vec(&v).unwrap();
        let pct = |b: usize| b as f64 * 100.0 / v.len() as f64;
        println!("live_codec_comparison: вход {} Б", v.len());
        println!("  slz:    {} Б ({:.1}%)", slz.len(), pct(slz.len()));
        println!("  lz4:    {} Б ({:.1}%)", lz4.len(), pct(lz4.len()));
        println!("  snappy: {} Б ({:.1}%)", snappy.len(), pct(snappy.len()));
        assert_eq!(decode(&slz, v.len()).unwrap(), v, "slz roundtrip");
        let worst = lz4.len().max(snappy.len());
        assert!(
            slz.len() <= worst,
            "slz ({}) не должен быть хуже худшего fast-кодека ({}) на смешанных данных",
            slz.len(),
            worst
        );
    }

    /// Raw-escape: на псевдослучайных данных (3 чанка) контейнер не
    /// распухает — каждый несжимаемый чанк пишется как есть.
    #[test]
    fn raw_escape_on_incompressible() {
        let n = (BLOCK * 5 / 2 / 4) * 4;
        let v: Vec<u8> = (0..n)
            .map(|i| (i as u64 * 6364136223846793005 >> 33) as u8)
            .collect();
        let enc = encode(&v);
        println!(
            "raw_escape: вход {} Б → {} Б (перерасход {} Б)",
            v.len(),
            enc.len(),
            enc.len() - v.len()
        );
        assert_eq!(decode(&enc, v.len()).unwrap(), v);
        assert!(
            enc.len() <= v.len() + 64,
            "raw-escape: перерасход {} Б недопустим",
            enc.len() - v.len()
        );
    }

    /// Все уровни скорости корректно кодируют и декодируют; формат общий.
    #[test]
    fn levels_roundtrip() {
        let mut v = Vec::new();
        v.extend(
            b"the quick brown fox jumps over the lazy dog. "
                .iter()
                .cycle()
                .take(50_000),
        );
        v.extend((0..20_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8));
        for level in [1usize, 3, 6, 12, 19] {
            let enc = encode_level(&v, level);
            assert_eq!(decode(&enc, v.len()).unwrap(), v, "уровень {level}");
            println!("уровень {level}: {} Б", enc.len());
        }
    }

    /// rep0: одинаковые интервалы между копиями блока — повторные копии
    /// берутся по «последнему офсету» без передачи самого офсета.
    #[test]
    fn rep0_offsets_reconstruct() {
        let mut v = Vec::new();
        let block: Vec<u8> = (0..64u32).map(|i| b'A' + (i * 7 % 26) as u8).collect();
        v.push(b'X');
        v.extend_from_slice(&block);
        for _ in 0..10 {
            v.push(b'Y');
            v.extend_from_slice(&block);
        }
        rt(&v);
    }

    /// PS24.1 словарный режим: записи ссылаются на содержимое словаря;
    /// словарь улучшает сжатие; без словаря и с чужим словарём — ошибки.
    #[test]
    fn dict_roundtrip_and_refs() {
        let mut dict = Vec::new();
        for k in 0..4000 {
            dict.extend_from_slice(
                format!(
                    "{{\"key{k}\": \"value-{}-строка\", \"cat\": \"категория{}\"}}\n",
                    k % 77,
                    k % 13
                )
                .as_bytes(),
            );
        }
        let mut input = Vec::new();
        for k in 0..2000u32 {
            input.extend_from_slice(
                format!(
                    "{{\"key{}\": \"value-{}-строка\", \"cat\": \"категория{}\", \"n\": {}}}\n",
                    k % 4000,
                    k % 77,
                    k % 13,
                    k
                )
                .as_bytes(),
            );
        }
        let enc_nodict = encode_level(&input, 6);
        let enc = encode_dict(&input, &dict, 6);
        assert!(
            enc.len() < enc_nodict.len(),
            "словарь должен улучшать сжатие: {} против {}",
            enc.len(),
            enc_nodict.len()
        );
        let dec = decode_dict(&enc, &dict, input.len() + 1).expect("декод со словарём");
        assert_eq!(dec, input);
        assert!(
            decode(&enc, input.len() + 1).is_err(),
            "SLZ3 без словаря должен быть ошибкой"
        );
        let wrong: Vec<u8> = dict.iter().map(|&b| b.wrapping_add(1)).collect();
        assert!(
            decode_dict(&enc, &wrong, input.len() + 1).is_err(),
            "словарь той же длины, но другой — должен отвергаться по CRC-64"
        );
    }

    /// источник совпадения в хвосте словаря: целиком в словаре и
    /// пересекающий границу dict→out (прогрессирующая копия, len > from_dict).
    #[test]
    fn dict_source_and_crossing() {
        // целиком в словаре: dict кончается "0123456789", вход начинается им же
        let dict = b"HEAD-HEAD-HEAD-0123456789".to_vec();
        let input = b"0123456789XYZ-random-junk-0123456789XYZ".to_vec();
        let enc = encode_dict(&input, &dict, 6);
        let dec = decode_dict(&enc, &dict, input.len() + 1).expect("полный dict-источник");
        assert_eq!(dec, input);

        // пересечение границы: dict кончается "ABCAB", вход — период 5;
        // энкодер находит матч off=5, источник = dict-хвост + рост из out
        let dict2 = b"QWERTY-ABCAB".to_vec();
        let input2 = b"ABCAB".repeat(20);
        let enc2 = encode_dict(&input2, &dict2, 6);
        let dec2 = decode_dict(&enc2, &dict2, input2.len() + 1).expect("пересечение dict→out");
        assert_eq!(dec2, input2);
    }

    /// словарный режим на всех характерных уровнях (greedy, lazy, lazy2).
    #[test]
    fn dict_levels_roundtrip() {
        let mut dict = Vec::new();
        for k in 0..500 {
            dict.extend_from_slice(format!("запись-{}-шаблон-категория{}\n", k % 40, k % 7).as_bytes());
        }
        let mut input = Vec::new();
        for k in 0..600u32 {
            input.extend_from_slice(
                format!("запись-{}-шаблон-категория{} значение{}\n", k % 500, k % 7, k).as_bytes(),
            );
        }
        for level in [1usize, 6, 19] {
            let enc = encode_dict(&input, &dict, level);
            let dec = decode_dict(&enc, &dict, input.len() + 1).expect("уровень");
            assert_eq!(dec, input, "уровень {level}");
        }
    }
}

#[cfg(test)]
mod huff2lv {
    use super::*;
    /// декод каждого символа из его собственного кода через 2-уровневую таблицу
    #[test]
    fn huff2lv_per_symbol() {
        // частоты, дающие весь спектр длин 1..15
        let mut freq = vec![0u32; 300];
        for (i, f) in freq.iter_mut().enumerate() {
            *f = 1u32 << (14 - (i % 15)).min(14);
        }
        let lens = huff_lengths(&freq);
        // отладка: гистограмма и проверка Крафта
        let mut hist = [0u32; 16];
        let mut kraft = 0u64;
        for &l in &lens {
            if l > 0 {
                hist[l as usize] += 1;
                kraft += 1u64 << (15 - l);
            }
        }
        println!("hist: {:?}", hist);
        println!("kraft sum (must be <= 1<<15 = {}): {}", 1u32 << 15, kraft);
        let codes = canonical_codes(&lens);
        let t = decode_table(&lens, &codes);
        for (s, &l) in lens.iter().enumerate().take(8) {
            println!("sym {s}: len {} code {:x}", l, codes[s]);
        }
        println!("sym 240: len {} code {:x}", lens[240], codes[240]);
        let mut maxlen = 0u8;
        for (s, &len) in lens.iter().enumerate() {
            if len == 0 {
                continue;
            }
            maxlen = maxlen.max(len);
            let len = len as u32;
            let code = codes[s];
            let mut br = BitReader {
                data: &[0u8; 8],
                pos: 8,
                acc: code as u64,
                nbits: len,
            };
            let got = br
                .decode_sym(&t)
                .unwrap_or_else(|e| panic!("sym {s} len {len} код {code:x}: {e}"));
            assert_eq!(got, s as u8, "sym {s} len {len} код {code:x}");
        }
        assert!(maxlen >= 14, "спектр длин должен доходить до длинных кодов");
    }

    /// бисекция: поток только с короткими кодами (под-таблиц нет)
    #[test]
    fn huff2lv_stream_short_codes() {
        let mut v = Vec::new();
        for k in 0..60000u32 {
            v.push((k % 128) as u8);
            if k % 97 == 0 {
                v.push((k % 251 + 1) as u8);
            }
        }
        let enc = encode(&v);
        assert_eq!(decode(&enc, v.len()).unwrap(), v);
    }

    /// бисекция: экстремальный перекос — коды len 1 и len 15 вместе + RLE
    #[test]
    fn huff2lv_stream_extreme_skew() {
        let mut v = Vec::new();
        for k in 0..250000u32 {
            v.push(if k % 2 == 0 { b'a' } else { b'b' });
        }
        // 300 редких байтов (коды до 15 бит)
        for k in 0..300u32 {
            v[(811 * (k + 1)) as usize] = (k % 253 + 2) as u8;
        }
        // RLE-ран на редком байте
        v.extend(std::iter::repeat_n(77u8, 70000));
        let enc = encode(&v);
        assert_eq!(decode(&enc, v.len()).unwrap(), v);
    }

    /// бисекция: почти равномерные 120 символов + один редкий (длинный код)
    #[test]
    fn huff2lv_stream_one_long() {
        let mut v = Vec::new();
        for k in 0..40000u32 {
            v.push((k % 120) as u8);
        }
        // редкий символ 200: 1 на 40000 → код длиной 13-15 бит
        for k in 0..3 {
            v[7777 * (k + 1)] = 200;
        }
        let enc = encode(&v);
        assert_eq!(decode(&enc, v.len()).unwrap(), v);
    }
}
