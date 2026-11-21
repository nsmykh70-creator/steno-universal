//! Точка входа бенчмарка STENO-UV: генерация корпусов, статическое и
//! обучаемое кодирование, эталон fixed-3B, сводная таблица.
//!
//! Запуск: `cargo run --release [--quick]` (без флага — корпусы по 10 МиБ).

use std::time::Instant;

use steno_uv::codec;
use steno_uv::corpus::{self, Kind, KINDS};
use steno_uv::dict::Dictionary;
use steno_uv::train::{self, TrainConfig};

const MIB: f64 = 1024.0 * 1024.0;

fn mib(bytes: u64) -> String {
    format!("{:.2} МиБ", bytes as f64 / MIB)
}

fn ms(d: std::time::Duration) -> String {
    if d.as_secs_f64() >= 1.0 {
        format!("{:.2} с", d.as_secs_f64())
    } else {
        format!("{:.1} мс", d.as_secs_f64() * 1000.0)
    }
}

/// Сжатие внешним zstd -19 (если установлен) для сравнения STENO→кодек с RAW→кодек.
fn compress_zstd(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("zstd")
        .args(["-q", "-19"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    stdin.write_all(data).ok()?;
    drop(stdin);
    let out = child.wait_with_output().ok()?;
    if out.status.success() {
        Some(out.stdout)
    } else {
        None
    }
}

fn zstd_available() -> bool {
    std::process::Command::new("zstd")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

struct Row {
    kind: Kind,
    input: u64,
    static_frame: u64,
    trained_frame: u64,
    fixed3: u64,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let quick = args.iter().any(|a| a == "--quick");
    let target: usize = if quick { 1 * 1024 * 1024 } else { 10 * 1024 * 1024 };
    println!("STENO-UV (PS9.10): токены 1/2/3/4/5 байт, статический + обучаемый словарь");
    println!(
        "Режим: {} на корпус, детерминированные генераторы (seed = 42)",
        mib(target as u64)
    );
    let static_dict = Dictionary::static_only();
    println!(
        "Статический словарь: {} записей (sym {} / short {} / reg {} / struct {} / ext {}), отброшено дубликатов {}",
        static_dict.total_entries(),
        static_dict.lane(0).len(),
        static_dict.lane(1).len(),
        static_dict.lane(2).len(),
        static_dict.lane(3).len(),
        static_dict.lane(4).len(),
        static_dict.deduped,
    );
    let have_zstd = zstd_available();
    if !have_zstd {
        println!("zstd не найден в PATH — сравнение STENO→zstd пропускается");
    }
    println!();

    let mut rows: Vec<Row> = Vec::new();
    for kind in KINDS {
        let raw = corpus::generate(kind, target, 42);
        println!("[{:>5}] вход {} Б ({})", kind.name(), raw.len(), mib(raw.len() as u64));

        // --- статический словарь ---
        let t = Instant::now();
        let (frame, stats) =
            codec::encode(&raw, &static_dict).expect("кодирование статическим словарём");
        let enc_s = t.elapsed();
        let t = Instant::now();
        let decoded = codec::decode(&frame).expect("декодер не должен падать");
        let dec_s = t.elapsed();
        let mut ok = decoded == raw;

        // --- обучаемый слой ---
        let t = Instant::now();
        let trained =
            train::train(&raw, &static_dict, &TrainConfig::default()).expect("обучение");
        let train_d = t.elapsed();
        let t = Instant::now();
        let (frame_t, _) = codec::encode(&raw, &trained).expect("кодирование с обучением");
        let enc_t = t.elapsed();
        let t = Instant::now();
        let decoded_t = codec::decode(&frame_t).expect("декодер обученного кадра");
        let dec_t = t.elapsed();
        ok = ok && decoded_t == raw;

        // --- эталон fixed-3B ---
        let f3 = codec::encode_fixed3(&raw, &static_dict).expect("fixed3");
        let f3_decoded = codec::decode_fixed3(&f3, &static_dict).expect("fixed3 декодер");
        ok = ok && f3_decoded == raw;

        rows.push(Row {
            kind,
            input: raw.len() as u64,
            static_frame: frame.len() as u64,
            trained_frame: frame_t.len() as u64,
            fixed3: f3.len() as u64,
        });

        // --- печать блока корпуса ---
        let w = stats.by_width();
        println!(
            "  стат. кадр : {} Б ({})  ×{:.3}  enc {} dec {}",
            frame.len(),
            mib(frame.len() as u64),
            raw.len() as f64 / frame.len() as f64,
            ms(enc_s),
            ms(dec_s),
        );
        println!(
            "  обуч. кадр : {} Б ({})  ×{:.3}  enc {} dec {}  (обучение {}; Δсловарь {} Б; обуч. записей: sym {} short {} reg {} struct {} ext {})",
            frame_t.len(),
            mib(frame_t.len() as u64),
            raw.len() as f64 / frame_t.len() as f64,
            ms(enc_t),
            ms(dec_t),
            ms(train_d),
            frame_t.len() as i64 - frame.len() as i64,
            trained.lane(0).len() - static_dict.lane(0).len(),
            trained.lane(1).len() - static_dict.lane(1).len(),
            trained.lane(2).len() - static_dict.lane(2).len(),
            trained.lane(3).len() - static_dict.lane(3).len(),
            trained.lane(4).len() - static_dict.lane(4).len(),
        );
        println!(
            "  fixed-3B   : {} Б ({})  ×{:.3}  Δ к обуч. = {} Б",
            f3.len(),
            mib(f3.len() as u64),
            raw.len() as f64 / f3.len() as f64,
            f3.len() as i64 - frame_t.len() as i64,
        );
        println!(
            "  токены: 1Б={} 2Б={} 3Б={} 4Б={} 5Б={} FC(цепочки)={} ({} Б) FD(байты)={}",
            w[0], w[1], w[2], w[3], w[4], stats.run, stats.run_bytes, stats.raw_byte
        );
        if have_zstd {
            if let (Some(zraw), Some(zst), Some(ztr)) = (
                compress_zstd(&raw),
                compress_zstd(&frame),
                compress_zstd(&frame_t),
            ) {
                println!(
                    "  zstd-19    : raw→{} Б | стат→{} Б | обуч→{} Б (контейнер целиком)",
                    zraw.len(),
                    zst.len(),
                    ztr.len()
                );
            }
        }
        println!("  roundtrip  : {}", if ok { "100% OK" } else { "ОШИБКА" });
        println!();
        assert!(ok, "корпус {}: roundtrip не сошёлся", kind.name());
    }

    // --- сводная таблица ---
    println!("==== Сводка ====");
    println!(
        "{:<7} {:>10} {:>10} {:>7} {:>10} {:>7} {:>10} {:>7}",
        "корпус", "вход, МиБ", "стат, МиБ", "×", "обуч, МиБ", "×", "fixed3, МиБ", "×"
    );
    for r in &rows {
        println!(
            "{:<7} {:>10} {:>10} {:>7.3} {:>10} {:>7.3} {:>10} {:>7.3}",
            r.kind.name(),
            format!("{:.2}", r.input as f64 / MIB),
            format!("{:.2}", r.static_frame as f64 / MIB),
            r.input as f64 / r.static_frame as f64,
            format!("{:.2}", r.trained_frame as f64 / MIB),
            r.input as f64 / r.trained_frame as f64,
            format!("{:.2}", r.fixed3 as f64 / MIB),
            r.input as f64 / r.fixed3 as f64,
        );
    }
    let totals = rows.iter().fold(
        (0u64, 0u64, 0u64, 0u64),
        |(a, b, c, d), r| (a + r.input, b + r.static_frame, c + r.trained_frame, d + r.fixed3),
    );
    println!(
        "{:<7} {:>10} {:>10} {:>7.3} {:>10} {:>7.3} {:>10} {:>7.3}",
        "ИТОГО",
        format!("{:.2}", totals.0 as f64 / MIB),
        format!("{:.2}", totals.1 as f64 / MIB),
        totals.0 as f64 / totals.1 as f64,
        format!("{:.2}", totals.2 as f64 / MIB),
        totals.0 as f64 / totals.2 as f64,
        format!("{:.2}", totals.3 as f64 / MIB),
        totals.0 as f64 / totals.3 as f64,
    );
}
