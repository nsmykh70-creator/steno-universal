//! PS25 «псевдо-избыточность»: обратимые трансформы, переводящие данные
//! из слепого пятна конкретного кодека в тривиально сжимаемый вид
//! (нули / малые значения / повторы), после чего сжатие выполняет обычный
//! SLZ, а при распаковке трансформ снимается.
//!
//! Теория (проверяемая): добавить псевдо-избыточность → сжать → убрать.
//! В терминах теории информации это не может улучшить сжатие НА ВСЕХ
//! данных (иначе нарушается подсчётная аргументация: |C(T(x))| ≥
//! |T(x)| − H(T(x)) и обратимый T не меняет энтропию). Но для КОНКРЕТНОГО
//! кодека и КОНКРЕТНЫХ данных трансформ может попасть в его слепое
//! пятно: период больше окна кодека, структура, ломающая хеши, байты
//! 0xFF там, где кодек ждёт 0x00. Эксперимент должен показать:
//! 1) на честно случайных данных выигрыша НЕТ (контроль энтропийного
//!    аргумента, оверхед только на заголовок);
//! 2) на данных со структурой за пределами окна кодека (период 45К
//!    при окне gzip/lz4 32–64К) трансформ ИСТОРИЧЕСКИЕ даёт выигрыш:
//!    xor_delta с p = период превращает данные в почти нули.
//!
//! Все трансформы сохраняют длину и обратимы за O(N):
//!
//! * variant 0 — identity (без трансформа);
//! * variant 1 — xor_delta(p): y[0..p] = x[0..p], y[i] = x[i] ^ x[i-p].
//!   Данные с периодом p → почти нули (псевдо-избыточность в чистом
//!   виде: структура, которую оконный кодек не видит, становится
//!   тривиальной). Декодер: x[i] = y[i] ^ x[i-p];
//! * variant 2 — diff_stride(p): y[i] = x[i] - x[i-p] (mod 256),
//!   y[0..p] = x[0..p]. p = размер слова (1/2/4) — малые РАЗНОСТИ слов:
//!   монотонные счётчики, метки времени, 16-бит аудио-walk. Именно
//!   структура «малых разностей на дистанции p» невидима 4-байтовым
//!   LZ-хешам — классическое слепое пятно (SLZ/lz4/gzip жмут ~0%);
//! * variant 3 — xor_const(k): y[i] = x[i] ^ k, k = самый частый байт
//!   сэмпла (данные, забитые 0xFF → нули).
//!
//! Выбор варианта: эвристика (доля нулей ≥ 8% или доля байтов < 16
//! ≥ 20% на сэмпле ≤ 256К — у случайных данных 0.4% и 6.3%),
//! затем каждый кандидат реально сжимается SLZ-ом уровня level и
//! сравнивается с несжатым-трансформом-оригиналом; побеждает минимум.
//! Итоговая обёртка SU25: b"SU25" + varint(orig_len) + varint(variant)
//! + varint(param) + SLZ2-поток от y.
//!
//! PS26 (SU26) — «сегрегация» (дополнение теории): данные режутся на
//! блоки (по умолчанию 64К, меньше чанка SLZ 1МиБ). Блоки, которые SLZ
//! жмёт сам (ratio < 70%), идут в поток A без изменений; плохо жмущиеся
//! блоки — в поток B, где к каждому применяется лучший из трансформов
//! (если выигрыш есть; для честного шума — identity). Оба потока жмутся
//! SLZ-ом отдельно: шум перестаёт портить энтропийную статистику
//! сжимаемой части, а на B-блоках со скрытой структурой трансформ
//! открывает сжатие. Формат: b"SU26" + varint(orig_len) + varint(block)
//! + varint(n_blocks) + bitmap(1 бит/блок: 1 = B) + записи (id, param)
//! B-блоков + varint(lenA) + varint(lenB) + поток A + поток B.

use crate::integrity;
use crate::slz;

/// Доля нулей в сэмпле, начиная с которой xor_delta/diff_delta считаются
/// перспективными (у случайных данных ~0.4%).
const ZERO_MIN: f64 = 0.08;
/// Доля байтов < 16, начиная с которой diff_delta считается перспективным
/// (у случайных данных 16/256 = 6.25%).
const SMALL_MIN: f64 = 0.20;
/// Размер сэмпла для эвристики и поиска периода.
const SAMPLE: usize = 256 * 1024;

/// Параметризованный трансформ: номер варианта и его параметр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Variant {
    pub id: u64,
    pub param: u64,
}

/// Обратимый трансформ: y = T(x). Длина y равна длине x.
pub fn transform(v: Variant, x: &[u8]) -> Vec<u8> {
    match v.id {
        0 => x.to_vec(),
        1 => xor_delta(v.param as usize, x),
        2 => diff_stride(v.param.max(1) as usize, x),
        3 => {
            let k = v.param as u8;
            x.iter().map(|&b| b ^ k).collect()
        }
        _ => unreachable!("неизвестный вариант трансформа"),
    }
}

/// Обратный трансформ: x = T⁻¹(y). Длина x равна длине y.
pub fn inverse(v: Variant, y: &[u8]) -> Vec<u8> {
    match v.id {
        0 => y.to_vec(),
        1 => xor_delta_inverse(v.param as usize, y),
        2 => diff_stride_inverse(v.param.max(1) as usize, y),
        3 => {
            let k = v.param as u8;
            y.iter().map(|&b| b ^ k).collect()
        }
        _ => unreachable!("неизвестный вариант трансформа"),
    }
}

/// xor_delta: периодические данные с периодом p → почти нули.
fn xor_delta(p: usize, x: &[u8]) -> Vec<u8> {
    if x.len() < p {
        return x.to_vec(); // шаг больше данных — тождество
    }
    debug_assert!(p >= 1);
    let mut y = Vec::with_capacity(x.len());
    y.extend_from_slice(&x[..p]);
    for i in p..x.len() {
        y.push(x[i] ^ x[i - p]);
    }
    y
}

fn xor_delta_inverse(p: usize, y: &[u8]) -> Vec<u8> {
    if y.len() < p {
        return y.to_vec();
    }
    debug_assert!(p >= 1);
    let mut x = vec![0u8; y.len()];
    x[..p].copy_from_slice(&y[..p]);
    for i in p..y.len() {
        x[i] = y[i] ^ x[i - p];
    }
    x
}

/// diff_stride(p): разность байтов с шагом p (mod 256) → малые значения
/// на данных с медленно меняющимися словами размера p.
fn diff_stride(p: usize, x: &[u8]) -> Vec<u8> {
    if x.len() < p {
        return x.to_vec();
    }
    debug_assert!(p >= 1);
    let mut y = Vec::with_capacity(x.len());
    y.extend_from_slice(&x[..p]);
    for i in p..x.len() {
        y.push(x[i].wrapping_sub(x[i - p]));
    }
    y
}

fn diff_stride_inverse(p: usize, y: &[u8]) -> Vec<u8> {
    if y.len() < p {
        return y.to_vec();
    }
    debug_assert!(p >= 1);
    let mut x = vec![0u8; y.len()];
    x[..p].copy_from_slice(&y[..p]);
    for i in p..y.len() {
        x[i] = y[i].wrapping_add(x[i - p]);
    }
    x
}

/// Доля нулей и доля малых (< 16) байтов в сэмпле данных.
fn stats(data: &[u8]) -> (f64, f64) {
    if data.is_empty() {
        return (0.0, 0.0);
    }
    let sample = &data[..data.len().min(SAMPLE)];
    let (mut z, mut s) = (0usize, 0usize);
    for &b in sample {
        z += (b == 0) as usize;
        s += (b < 16) as usize;
    }
    let n = sample.len() as f64;
    (z as f64 / n, s as f64 / n)
}

/// Поиск периода в данных: хеш-таблица 12-байтных строк сэмпла, для
/// каждого повторного появления ключа расстояние до прошлого попадает
/// в гистограмму; топ-расстояние с ≥ 32 повторами — кандидат периода.
/// Дёшево находит произвольные периоды (не только из списка).
fn scan_period(x: &[u8]) -> Option<usize> {
    let sample = &x[..x.len().min(SAMPLE)];
    if sample.len() < 64 {
        return None;
    }
    // ключ = 12 байт, таблица 2^16 цепочек по последнему расстоянию
    let mut table = vec![0usize; 1 << 16];
    let mut hist = vec![0u32; 1 << 16];
    let mut best = 0usize;
    let mut best_count = 0u32;
    let mask = (1usize << 16) - 1;
    for i in 0..sample.len().saturating_sub(12) {
        let key = &sample[i..i + 12];
        let h = (u32::from_le_bytes([key[0], key[3], key[7], key[11]]) as usize) & mask;
        let prev = table[h];
        if prev != 0 && i > prev {
            let d = i - prev;
            if d >= 16 && d <= 1 << 20 {
                hist[d & mask] += 1;
                if hist[d & mask] > best_count {
                    best_count = hist[d & mask];
                    best = d;
                }
            }
        }
        table[h] = i;
    }
    // 32 повтора ключа на расстоянии d в сэмпле 256К — период d
    // статистически достоверен для периодических данных (для случайных
    // совпадения 12-байтных ключей практически невозможны)
    if best_count >= 32 {
        Some(best)
    } else {
        None
    }
}

/// Список кандидатов-трансформов для данных x: diff_stride 1/2/4,
/// xor_const (если есть доминирующий байт), xor_delta по найденному
/// периоду и степеням двойки. Порядок детерминирован.
fn candidate_variants(x: &[u8]) -> Vec<Variant> {
    let (mut candidates, mut seen) = (Vec::new(), std::collections::HashSet::new());
    let mut push = |v: Variant, c: &mut Vec<Variant>, s: &mut std::collections::HashSet<u64>| {
        if s.insert((v.id << 32) ^ v.param) {
            c.push(v);
        }
    };
    // шаги 1/2/4: байт, 16-бит слово, 32-бит слово (дёшево — эвристика
    // нулей/малых значений отсечёт случайные данные до сжатия)
    push(Variant { id: 2, param: 1 }, &mut candidates, &mut seen);
    push(Variant { id: 2, param: 2 }, &mut candidates, &mut seen);
    push(Variant { id: 2, param: 4 }, &mut candidates, &mut seen);
    let (_, small) = stats(x);
    if small >= 0.10 {
        // мода сэмпла как ключ xor_const
        let mut freq = [0u32; 256];
        for &b in &x[..x.len().min(SAMPLE)] {
            freq[b as usize] += 1;
        }
        let k = freq
            .iter()
            .enumerate()
            .max_by_key(|(_, &f)| f)
            .map(|(i, _)| i as u64)
            .unwrap_or(0);
        push(Variant { id: 3, param: k }, &mut candidates, &mut seen);
    }
    // xor_delta: период из скана + степени двойки
    let mut periods = Vec::new();
    if let Some(p) = scan_period(x) {
        periods.push(p);
    }
    for e in 3..17 {
        periods.push(1usize << e);
    }
    for p in periods {
        if p <= x.len() / 2 {
            push(Variant { id: 1, param: p as u64 }, &mut candidates, &mut seen);
        }
    }
    candidates
}

/// Автовыбор трансформа: сжатие исходника SLZ-ом уровня `level`, затем
/// перебор перспективных кандидатов (эвристика + сжатие), минимум размера.
/// Возвращает (вариант, сжатый поток трансформированных данных).
pub fn pick_and_compress(x: &[u8], level: usize) -> (Variant, Vec<u8>) {
    let base = slz::encode_level(x, level);
    if x.len() < 32 {
        // на малых данных трансформ не окупает даже заголовок
        return (Variant { id: 0, param: 0 }, base);
    }
    let mut best = (Variant { id: 0, param: 0 }, base);
    for v in candidate_variants(x) {
        let y = transform(v, x);
        let (zero, _) = stats(&y);
        let (_, small_y) = stats(&y);
        // эвристика: трансформ без нулей и малых значений не сжимается
        // лучше — отсекаем до дорогостоящего сжатия
        if zero < ZERO_MIN && small_y < SMALL_MIN {
            continue;
        }
        let packed = slz::encode_level(&y, level);
        let header = 4 + varint_len(x.len() as u64) + varint_len(v.id) + varint_len(v.param);
        if packed.len() + header < best.1.len() {
            best = (v, packed);
        }
    }
    best
}

/// Выбор трансформа B-блока при уже известной базовой цене сжатия
/// `base_len` (классификационное сжатие переиспользуется). Возвращает
/// (вариант, трансформированные байты); без выигрыша — identity.
fn pick_transform_only(x: &[u8], level: usize, base_len: usize) -> (Variant, Vec<u8>) {
    if x.len() < 32 {
        return (Variant { id: 0, param: 0 }, x.to_vec());
    }
    let mut best = (Variant { id: 0, param: 0 }, x.to_vec());
    // цена identity = классификационное сжатие блока; каждый кандидат
    // сравнивается с ЛУЧШИМ на данный момент, а не только с исходником
    // (иначе последний «лучше исходника, но хуже выбранного» кандидат
    // перезаписывает победителя)
    let mut best_cost = base_len;
    for v in candidate_variants(x) {
        let y = transform(v, x);
        let (zero, _) = stats(&y);
        let (_, small_y) = stats(&y);
        if zero < ZERO_MIN && small_y < SMALL_MIN {
            continue;
        }
        let packed = slz::encode_level(&y, level);
        // цена записи B-блока в карте SU26: байт id + varint параметра
        let rec = 1 + varint_len(v.param);
        if packed.len() + rec < best_cost {
            best = (v, y);
            best_cost = packed.len() + rec;
        }
    }
    best
}

/// Обёртка SU25: magic + varint(orig_len) + varint(variant) + varint(param)
/// + SLZ2-поток трансформированных данных.
pub fn su25_pack(x: &[u8], level: usize) -> (Variant, Vec<u8>) {
    let (v, stream) = pick_and_compress(x, level);
    let mut out = Vec::with_capacity(stream.len() + 16);
    out.extend_from_slice(b"SU25");
    put_uvarint(x.len() as u64, &mut out);
    put_uvarint(v.id, &mut out);
    put_uvarint(v.param, &mut out);
    out.extend_from_slice(&stream);
    (v, out)
}

/// Распаковка SU25: снимает SLZ2 и трансформ, проверяет длину.
pub fn su25_unpack(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 || &data[..4] != b"SU25" {
        return Err("su25: магия не SU25");
    }
    let mut pos = 4usize;
    let orig_len = get_uvarint(data, &mut pos)? as usize;
    let variant = get_uvarint(data, &mut pos)?;
    let param = get_uvarint(data, &mut pos)?;
    let v = Variant { id: variant, param };
    if v.id > 3 {
        return Err("su25: неизвестный вариант трансформа");
    }
    if (v.id == 1 || v.id == 2) && (v.param == 0 || v.param as usize > orig_len) {
        return Err("su25: некорректный параметр трансформа");
    }
    let y = slz::decode(&data[pos..], orig_len)?;
    let x = inverse(v, &y);
    if x.len() != orig_len {
        return Err("su25: длина после обратного трансформа не совпала");
    }
    Ok(x)
}

fn put_uvarint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn varint_len(v: u64) -> usize {
    let mut n = 1usize;
    let mut v = v >> 7;
    while v > 0 {
        n += 1;
        v >>= 7;
    }
    n
}

fn get_uvarint(data: &[u8], pos: &mut usize) -> Result<u64, &'static str> {
    let mut v = 0u64;
    let mut shift = 0u32;
    for _ in 0..9 {
        let b = *data.get(*pos).ok_or("su25: обрыв varint")?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b < 0x80 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err("su25: varint превышает 64 бита");
        }
    }
    Err("su25: обрыв varint")
}

/// Полная проверка конверта SU21 поверх SU25 (как PS21 для stuv-кадров).
pub fn su25_verify(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    let raw = integrity::try_unwrap(data).ok_or("su25: не конверт SU21")?;
    let x = su25_unpack(raw)?;
    Ok((raw.to_vec(), x))
}

/// Размер блока SU26 по умолчанию: меньше чанка SLZ (1МиБ) в 16 раз —
/// гранулярность сегрегации.
pub const SU26_BLOCK: usize = 65536;
/// Порог классификации (процентов): блок идёт в поток B, если SLZ жмёт
/// его хуже чем до 40% длины. Смысл порога: не «плохо сжимаемый», а
/// «сжимаемый не до предела» — слепые пятна (walk ~50%, счётчики ~40%)
/// тоже должны попасть в B и получить трансформ. Блоки с ratio < 40%
/// (текст, JSON) — сжимаемая часть, поток A без изменений.
const BLOCK_B_MIN_RATIO: usize = 40;

/// SU26 «сегрегация»: блоки данных сортируются в поток A (сжимаемые,
/// не трогаем) и поток B (плохо сжимаемые; к каждому применяется
/// лучший трансформ, если он есть). Потоки жмутся SLZ-ом раздельно:
/// шум B перестаёт портить энтропийную статистику A, а B-блоки со
/// скрытой структурой сжимаются через трансформ.
pub fn su26_pack(x: &[u8], level: usize, block_size: usize) -> Vec<u8> {
    assert!(block_size >= 16, "su26: блок не меньше 16 байт");
    let n_blocks = if x.is_empty() {
        0
    } else {
        x.len().div_ceil(block_size)
    };
    let mut a_data = Vec::new();
    let mut b_data = Vec::new();
    let mut bitmap = vec![0u8; n_blocks.div_ceil(8)];
    let mut b_records: Vec<(u64, u64)> = Vec::new();
    for bi in 0..n_blocks {
        let lo = bi * block_size;
        let hi = (lo + block_size).min(x.len());
        let blk = &x[lo..hi];
        // классификация = сжатие блока (цена переиспользуется в выборе)
        let comp = slz::encode_level(blk, level);
        if comp.len() < blk.len() * BLOCK_B_MIN_RATIO / 100 {
            a_data.extend_from_slice(blk);
        } else {
            bitmap[bi / 8] |= 1 << (bi % 8);
            let (v, y) = pick_transform_only(blk, level, comp.len());
            b_records.push((v.id, v.param));
            b_data.extend_from_slice(&y);
        }
    }
    let stream_a = slz::encode_level(&a_data, level);
    let stream_b = slz::encode_level(&b_data, level);
    let mut out = Vec::with_capacity(stream_a.len() + stream_b.len() + 64);
    out.extend_from_slice(b"SU26");
    put_uvarint(x.len() as u64, &mut out);
    put_uvarint(block_size as u64, &mut out);
    put_uvarint(n_blocks as u64, &mut out);
    out.extend_from_slice(&bitmap);
    for (id, param) in &b_records {
        out.push(*id as u8);
        put_uvarint(*param, &mut out);
    }
    put_uvarint(stream_a.len() as u64, &mut out);
    put_uvarint(stream_b.len() as u64, &mut out);
    out.extend_from_slice(&stream_a);
    out.extend_from_slice(&stream_b);
    out
}

/// Распаковка SU26: раскладывает потоки по битовой карте, снимает
/// трансформы B-блоков, проверяет суммарную длину.
pub fn su26_unpack(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    if data.len() < 5 || &data[..4] != b"SU26" {
        return Err("su26: магия не SU26");
    }
    let mut pos = 4usize;
    let orig_len = get_uvarint(data, &mut pos)? as usize;
    let block_size = get_uvarint(data, &mut pos)? as usize;
    let n_blocks = get_uvarint(data, &mut pos)? as usize;
    if block_size < 16 {
        return Err("su26: нулевой размер блока");
    }
    if n_blocks != if orig_len == 0 { 0 } else { orig_len.div_ceil(block_size) } {
        return Err("su26: число блоков не сходится");
    }
    let bitmap_len = n_blocks.div_ceil(8);
    if data.len() < pos + bitmap_len {
        return Err("su26: обрыв карты блоков");
    }
    let bitmap = &data[pos..pos + bitmap_len];
    pos += bitmap_len;
    // записи B-блоков: (id, param) в порядке возрастания блоков
    let mut b_vars = Vec::new();
    for bi in 0..n_blocks {
        if bitmap[bi / 8] >> (bi % 8) & 1 == 1 {
            let id = *data.get(pos).ok_or("su26: обрыв записи трансформа")? as u64;
            pos += 1;
            let param = get_uvarint(data, &mut pos)?;
            if id > 3 {
                return Err("su26: неизвестный вариант трансформа");
            }
            if (id == 1 || id == 2) && (param == 0 || param as usize > block_size) {
                return Err("su26: некорректный параметр трансформа");
            }
            b_vars.push(Variant { id, param });
        }
    }
    let len_a = get_uvarint(data, &mut pos)? as usize;
    let len_b = get_uvarint(data, &mut pos)? as usize;
    if data.len() != pos + len_a + len_b {
        return Err("su26: длины потоков не сходятся с payload");
    }
    let a = slz::decode(&data[pos..pos + len_a], orig_len)?;
    let b = slz::decode(&data[pos + len_a..], orig_len)?;
    let mut out = vec![0u8; orig_len];
    let (mut ai, mut bi, mut bvi) = (0usize, 0usize, 0usize);
    for blk in 0..n_blocks {
        let lo = blk * block_size;
        let hi = (lo + block_size).min(orig_len);
        let l = hi - lo;
        if bitmap[blk / 8] >> (blk % 8) & 1 == 0 {
            if ai + l > a.len() {
                return Err("su26: поток A короче карты");
            }
            out[lo..hi].copy_from_slice(&a[ai..ai + l]);
            ai += l;
        } else {
            if bi + l > b.len() {
                return Err("su26: поток B короче карты");
            }
            let v = *b_vars.get(bvi).ok_or("su26: нет записи трансформа")?;
            bvi += 1;
            let x = inverse(v, &b[bi..bi + l]);
            bi += l;
            if x.len() != l {
                return Err("su26: длина после инверсии не совпала");
            }
            out[lo..hi].copy_from_slice(&x);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slz::encode_level;

    fn rnd(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (s >> 33) as u8
            })
            .collect()
    }

    #[test]
    fn transforms_roundtrip_edge_cases() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![42u8],
            b"aaaa".to_vec(),
            vec![0u8; 1000],
            vec![255u8; 1000],
            rnd(1 << 20, 7),
            {
                // период 45000 > окна gzip/lz4 — главный сценарий теории
                let block = rnd(45000, 11);
                block.repeat(224)
            },
            {
                // монотонные счётчики с редкими случайными вставками
                let mut v = Vec::new();
                for i in 0u64..200000 {
                    v.extend_from_slice(&i.to_be_bytes());
                }
                v
            },
        ];
        let variants = [
            Variant { id: 0, param: 0 },
            Variant { id: 1, param: 1 },
            Variant { id: 1, param: 45000 },
            Variant { id: 2, param: 1 },
            Variant { id: 2, param: 2 },
            Variant { id: 2, param: 4 },
            Variant { id: 3, param: 0x5a },
        ];
        for (ci, x) in cases.iter().enumerate() {
            for v in variants {
                // p > len нельзя; p = 1 всегда допустим
                if v.id == 1 && (v.param == 0 || v.param as usize > x.len()) {
                    continue;
                }
                let y = transform(v, x);
                assert_eq!(y.len(), x.len(), "длина сохранена, случай {ci} вариант {v:?}");
                assert_eq!(&inverse(v, &y), x, "обратимость, случай {ci} вариант {v:?}");
            }
        }
    }

    #[test]
    fn periodic_data_transform_makes_zeros() {
        let block = rnd(45000, 11);
        let x = block.repeat(100);
        let y = transform(Variant { id: 1, param: 45000 }, &x);
        let (zero, _) = stats(&y);
        // первые 45000 байт сэмпла — копия блока, дальше нули; stats()
        // семплирует первые 256К, поэтому ожидание считаем по сэмплу
        let expect = (x.len().min(SAMPLE) - 45000) as f64 / x.len().min(SAMPLE) as f64;
        // допуск 1e-3: в головном блоке есть случайные нули (~45000/256)
        assert!((zero - expect).abs() < 1e-3, "доля нулей {zero} vs {expect}");
    }

    #[test]
    fn scan_period_finds_arbitrary_period() {
        let block = rnd(3127, 23); // не степень двойки, не из списка
        let x = block.repeat(40);
        assert_eq!(scan_period(&x), Some(3127), "скан находит произвольный период");
        assert_eq!(scan_period(&rnd(1 << 18, 5)), None, "случайные данные без периода");
    }

    #[test]
    fn pick_chooses_transform_only_when_it_wins() {
        // случайные данные: трансформ не должен побить энтропию
        let x = rnd(4 << 20, 1);
        let (v, packed) = pick_and_compress(&x, 6);
        let raw = encode_level(&x, 6);
        assert_eq!(v.id, 0, "на случайных данных вариант identity");
        assert_eq!(packed.len(), raw.len());
        assert!(packed.len() > x.len(), "случайные данные несжимаемы");

        // 16-бит аудио-walk: сэмплы меняются на ±0..4, точных 4-байтных
        // повторов почти нет → SLZ (MINMATCH 4) жмёт ~0% (raw-escape);
        // diff_stride снимает «малые разности» → SLZ жмёт в разы лучше.
        // Это и есть слепое пятно LZ-хешей — предмет теории.
        let mut p = Vec::with_capacity(4 << 20);
        let mut s: i16 = 0;
        let mut t = 77u64;
        for _ in 0..2 << 20 {
            t = t.wrapping_mul(6364136223846793005).wrapping_add(1);
            let step = ((t >> 33) % 9) as i16 - 4;
            s = s.wrapping_add(step);
            p.extend_from_slice(&s.to_le_bytes());
        }
        let (pv, packed_p) = pick_and_compress(&p, 6);
        let raw_p = encode_level(&p, 6);
        assert_eq!(pv.id, 2, "walk → diff_stride (выбран {pv:?})");
        assert!(
            packed_p.len() * 10 < raw_p.len() * 8,
            "SU25 {} должен быть < 80% SLZ {} на walk-данных",
            packed_p.len(),
            raw_p.len()
        );
    }

    #[test]
    fn su25_pack_unpack_roundtrip_all_variants() {
        let mut x = rnd(1 << 18, 31);
        x.extend_from_slice("абвгд".repeat(10000).as_bytes());
        for lv in [1usize, 6, 12] {
            let (v, packed) = su25_pack(&x, lv);
            assert!(packed.len() < x.len(), "su25 жмёт смешанные данные");
            let back = su25_unpack(&packed).unwrap();
            assert_eq!(back, x, "round-trip уровень {lv}, вариант {v:?}");
        }
        // пустой вход и 1 байт — identity и корректный заголовок
        let (_, empty) = su25_pack(&[], 6);
        assert_eq!(su25_unpack(&empty).unwrap(), Vec::<u8>::new());
        let (v1, one) = su25_pack(&[7u8], 6);
        assert_eq!(v1.id, 0);
        assert_eq!(su25_unpack(&one).unwrap(), vec![7u8]);
    }

    fn json_like(n: usize, seed: u64) -> Vec<u8> {
        // реалистичная сжимаемая часть: повторяющиеся записи с вариацией
        let mut s = seed;
        let mut v = Vec::with_capacity(n);
        let mut i = 0u32;
        while v.len() < n {
            i += 1;
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            let rec = format!(
                "{{\"ts\":{},\"user\":\"u-{:06}\",\"event\":\"page_view\",\"page\":\"/catalog/item/{}/view?ref=menu\"}}\n",
                1766500000 + i as u64 * 3,
                (s >> 33) % 50000,
                i % 12345
            );
            v.extend_from_slice(rec.as_bytes());
        }
        v.truncate(n);
        v
    }

    #[test]
    fn su26_roundtrip_edge_cases() {
        let cases: Vec<(&str, Vec<u8>, usize)> = vec![
            ("пустой", vec![], SU26_BLOCK),
            ("1 байт", vec![9u8], SU26_BLOCK),
            ("ровно блок", rnd(SU26_BLOCK, 51), SU26_BLOCK),
            ("блок+1", rnd(SU26_BLOCK + 1, 52), SU26_BLOCK),
            ("шум 192К", rnd(192 << 10, 53), SU26_BLOCK),
            ("walk 192К", json_like(96 << 10, 3), SU26_BLOCK),
            ("микс", {
                let mut v = json_like(128 << 10, 5);
                v.extend_from_slice(&rnd(64 << 10, 6));
                v
            }, SU26_BLOCK),
            ("микс, нечётный блок 17", {
                let mut v = json_like(1000, 7);
                v.extend_from_slice(&rnd(317, 8));
                v
            }, 17),
        ];
        for (name, x, bs) in cases {
            let packed = su26_pack(&x, 6, bs);
            let back = su26_unpack(&packed).unwrap();
            assert_eq!(back, x, "su26 round-trip: {name}");
        }
        // пустой вход: заголовок валиден и распаковывается
        let e = su26_pack(&[], 6, SU26_BLOCK);
        assert_eq!(su26_unpack(&e).unwrap(), Vec::<u8>::new());
        assert!(su26_unpack(b"XX26").is_err(), "чужая магия");
        let mut cut = su26_pack(&rnd(1000, 9), 6, 256);
        cut.truncate(cut.len() / 3);
        assert!(su26_unpack(&cut).is_err(), "обрезанный SU26");
    }

    #[test]
    fn su26_walk_blocks_get_transform() {
        // цельный walk с возвратной тягой (та же генерация, что в
        // bench-синтетике): блоки ratio ~50% → поток B, diff_stride(2)
        // сжимает до ~35% — SU26 должен заметно бить цельный SLZ
        let mut walk = Vec::new();
        let mut s: i16 = 0;
        let mut t = 123u64;
        for _ in 0..(192 << 10) / 2 {
            t = t.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut step = (((t >> 33) % 9) as i16) - 4;
            if s > 3000 {
                step -= (s / 3000).clamp(0, 4);
            } else if s < -3000 {
                step += (-s / 3000).clamp(0, 4);
            }
            s = s.wrapping_add(step);
            walk.extend_from_slice(&s.to_le_bytes());
        }
        let packed = su26_pack(&walk, 6, SU26_BLOCK);
        let raw = encode_level(&walk, 6);
        assert!(
            packed.len() * 100 < raw.len() * 80,
            "SU26 {} < 80% SLZ {} на walk",
            packed.len(),
            raw.len()
        );
        assert_eq!(su26_unpack(&packed).unwrap(), walk);
    }

    #[test]
    fn su26_entropy_control_rand() {
        // честный шум: карта + raw, оверхед порядка сотен байт, не процентов
        let x = rnd(256 << 10, 61);
        let packed = su26_pack(&x, 6, SU26_BLOCK);
        assert!(
            packed.len() < x.len() + 4096,
            "SU26 на шуме {} не должен превышать len+{}",
            packed.len(),
            4096
        );
        assert_eq!(su26_unpack(&packed).unwrap(), x);
    }

    #[test]
    fn su26_coarse_mix_beats_whole_slz() {
        // крупное перемешивание (пробеги ≥ блока): сегрегация даёт
        // раздельную энтропийную статистику — SU26 < цельный SLZ
        let mut x = json_like(512 << 10, 13);
        x.extend_from_slice(&rnd(224 << 10, 14));
        x.extend_from_slice(&json_like(400 << 10, 15));
        let packed = su26_pack(&x, 6, SU26_BLOCK);
        let raw = encode_level(&x, 6);
        assert!(
            packed.len() < raw.len(),
            "SU26 {} < SLZ {} на coarse-миксе",
            packed.len(),
            raw.len()
        );
        assert_eq!(su26_unpack(&packed).unwrap(), x);
    }

    #[test]
    fn su26_fine_mix_no_harm() {
        // мелкое перемешивание (пробеги 8-32К при блоке 64К): почти
        // каждый блок примесный — SU26 не должен вредить (допуск 2%)
        let mut x = Vec::new();
        let mut t = 0u64;
        for k in 0..64 {
            t = t.wrapping_mul(6364136223846793005).wrapping_add(1);
            let run = 8192 + ((t >> 33) % 24576) as usize;
            if k % 7 == 0 {
                x.extend_from_slice(&rnd(run, 100 + k));
            } else {
                x.extend_from_slice(&json_like(run, 200 + k));
            }
        }
        let packed = su26_pack(&x, 6, SU26_BLOCK);
        let raw = encode_level(&x, 6);
        assert!(
            packed.len() * 50 < raw.len() * 51,
            "SU26 {} ≤ 102% SLZ {} на fine-миксе",
            packed.len(),
            raw.len()
        );
        assert_eq!(su26_unpack(&packed).unwrap(), x);
    }

    #[test]
    fn su25_rejects_corrupt_input() {
        assert!(su25_unpack(b"XX25").is_err(), "чужая магия");
        assert!(su25_unpack(b"SU25").is_err(), "обрыв заголовка");
        let (v, mut packed) = su25_pack(&rnd(1 << 16, 3), 6);
        assert_eq!(v.id, 0);
        let cut = packed.len() / 2;
        packed.truncate(cut);
        assert!(su25_unpack(&packed).is_err(), "обрезанный поток");
    }
}
