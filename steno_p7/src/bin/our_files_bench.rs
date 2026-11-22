//! PS-ветка: бенч STENO-P8-S20 (integrity::encode = SP8 + CRC-64 конверт)
//! на РЕАЛЬНЫХ файлах пользователя — по методике Stage20 (полный размер
//! архива, раздельные тайминги encode/decode, побайтовая верификация,
//! сравнение с xz/zstd/gzip на тех же байтах).
//!
//! Запуск: cargo run --release --bin our_files_bench -- [reps] файл1 файл2 ...
use steno_p7::stage20::{benchmark, benchmark_external};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("использование: our_files_bench [reps] <файл1> <файл2> ...");
        std::process::exit(2);
    }
    let (reps, paths): (usize, &[String]) = match args[1].parse::<usize>() {
        Ok(n) => (n.max(1), &args[2..]),
        Err(_) => (1, &args[1..]),
    };
    println!("dataset,codec,input_bytes,archive_bytes,ratio_percent,encode_mib_s,decode_mib_s,roundtrip");
    for path in paths {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => { eprintln!("SKIP: {path}: {e}"); continue; }
        };
        let name = std::path::Path::new(path)
            .file_name().and_then(|s| s.to_str()).unwrap_or(path).to_string();
        // STENO-P8-S20 (SP8-словарь + CRC-64 конверт)
        let r = benchmark(name.clone(), &data, reps);
        println!("{},{},{},{},{:.3},{:.3},{:.3},{}", name, "STENO-P8-S20",
            r.input_bytes, r.archive_bytes, r.ratio_percent(), r.encode_mib_s(), r.decode_mib_s(), r.verified);
        // STENO-SP8-кадр + xz -9e (синергия трансформа с LZMA2): кадр
        // кодируем в памяти, xz прогоняем их benchmark_external (без
        // дедлоков пайпов), архив = кадр + xz(кадр)
        let t0 = Instant::now();
        let (frame, _stats) = steno_p7::joint::encode(&data);
        let enc_s = t0.elapsed().as_secs_f64();
        if let Some(x) = benchmark_external("xz", &["-9e", "-T1", "-c"], &["-d", "-c"], &frame, 1) {
            // контейнер = сжатый кадр целиком (методика Stage20: полный размер
            // архивного файла, без сложения с несжатым кадром)
            let total = x.archive_bytes;
            println!("{},{},{},{},{:.3},{:.3},{:.3},{}", name, "SP8+xz-9e",
                data.len(), total, total as f64 * 100.0 / data.len().max(1) as f64,
                data.len() as f64 / 1048576.0 / enc_s.max(1e-9),
                data.len() as f64 / 1048576.0 / x.decode.as_secs_f64().max(1e-9), x.verified);
        }
        // конкуренты на тех же байтах (методика Stage20)
        for (program, enc, dec) in [
            ("zstd", vec!["-q", "-c"], vec!["-q", "-d", "-c"]),
            ("xz", vec!["-c"], vec!["-d", "-c"]),
            ("gzip", vec!["-c"], vec!["-d", "-c"]),
        ] {
            match benchmark_external(program, &enc, &dec, &data, reps) {
                Some(x) => {
                    let enc_speed = if x.encode.is_zero() { 0.0 } else { data.len() as f64 / 1048576.0 / x.encode.as_secs_f64() };
                    let dec_speed = if x.decode.is_zero() { 0.0 } else { data.len() as f64 / 1048576.0 / x.decode.as_secs_f64() };
                    println!("{},{},{},{},{:.3},{:.3},{:.3},{}", name, program,
                        data.len(), x.archive_bytes, x.archive_bytes as f64 * 100.0 / data.len().max(1) as f64,
                        enc_speed, dec_speed, x.verified)
                }
                None => eprintln!("SKIP: {program} недоступен для {name}"),
            }
        }
    }
}
