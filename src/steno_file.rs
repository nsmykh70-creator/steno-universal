//! Утилита для реальных файлов: кодирование в кадры STENO-UV (статический и
//! обучаемый словари), проверка roundtrip и обратное декодирование.
//!
//! Использование:
//!   steno_file encode <вход> <каталог_вывода>  — кадры + восстановленные файлы
//!   steno_file decode <кадр> <выход>           — кадр → исходные байты
//!   steno_file cmz <поток> <каталог>           — CM-сжатие потока (файловый формат с длиной)
//!   steno_file cmz-decode <файл.cmz> <выход>   — CM-распаковка потока
//!   steno_file pack <hdr> <ctrl> <data> <кадр> — сборка кадра из частей архива
//!   steno_file unpack <каталог> <stem> <выход> — полная распаковка архива пайплайна

use std::fs;
use std::time::Instant;

use steno_uv::codec;
use steno_uv::dict::Dictionary;
use steno_uv::lpaq_ctrl;
use steno_uv::train::{self, TrainConfig};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("encode") if args.len() >= 4 => encode_cmd(&args[2], &args[3], &args[4..]),
        Some("decode") if args.len() >= 4 => decode_cmd(&args[2], &args[3]),
        Some("dumpstreams") if args.len() >= 4 => dumpstreams_cmd(&args[2], &args[3]),
        Some("frec") if args.len() >= 4 => frec_cmd(&args[2], &args[3], &args[4..]),
        Some("cmz") if args.len() >= 4 => cmz_cmd(&args[2], &args[3]),
        Some("cmz-decode") if args.len() >= 4 => cmz_decode_cmd(&args[2], &args[3]),
        Some("pack") if args.len() >= 6 => pack_cmd(&args[2], &args[3], &args[4], &args[5]),
        Some("unpack") if args.len() >= 5 => unpack_cmd(&args[2], &args[3], &args[4]),
        _ => {
            eprintln!("использование: steno_file encode <вход> <каталог> [--cap N] [--min-count M] [--chars C] [--syll K] | decode <кадр> <выход> | dumpstreams <кадр> <каталог> | frec <ctrl> <каталог> [--order0] | cmz <поток> <каталог> | cmz-decode <файл.cmz> <выход> | pack <hdr> <ctrl> <data> <кадр> | unpack <каталог> <stem> <выход>");
            std::process::exit(2);
        }
    }
}

fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string())
}

/// Флаги тюнинга обучения (по умолчанию — TrainConfig::default).
fn parse_flags(args: &[String]) -> TrainConfig {
    let mut cfg = TrainConfig::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cap" => {
                cfg.candidate_cap = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(cfg.candidate_cap);
                i += 2;
            }
            "--min-count" => {
                cfg.min_count = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(cfg.min_count);
                i += 2;
            }
            "--chars" => {
                cfg.max_train_chars = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(cfg.max_train_chars);
                i += 2;
            }
            "--syll" => {
                cfg.syllables = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(cfg.syllables);
                i += 2;
            }
            _ => i += 1,
        }
    }
    cfg
}

fn encode_cmd(input: &str, outdir: &str, flags: &[String]) {
    let cfg = parse_flags(flags);
    let raw = fs::read(input).expect("чтение входного файла");
    let utf8_ok = std::str::from_utf8(&raw).is_ok();
    let base = Dictionary::static_only();

    let t = Instant::now();
    let (frame_static, st_static) =
        codec::encode(&raw, &base).expect("кодирование статическим словарём");
    let enc_s = t.elapsed();
    let t = Instant::now();
    let dec_static = codec::decode(&frame_static).expect("декодер статического кадра");
    let dec_s = t.elapsed();
    assert_eq!(dec_static, raw, "roundtrip статического кадра не сошёлся");

    // train(): при невалидном UTF-8 возвращает базовый словарь без изменений
    let t = Instant::now();
    let trained = train::train(&raw, &base, &cfg).expect("обучение");
    let train_d = t.elapsed();
    let t = Instant::now();
    let (frame_trained, st_trained) =
        codec::encode(&raw, &trained).expect("кодирование обученным словарём");
    let enc_t = t.elapsed();
    // кадр на диск ДО проверки декодером: падение разбирать по файлу
    fs::create_dir_all(outdir).expect("создание каталога вывода");
    let stem = file_stem(input);
    fs::write(format!("{outdir}/{stem}.stuv_trained"), &frame_trained)
        .expect("запись кадра обуч");
    let t = Instant::now();
    let dec_trained = codec::decode(&frame_trained).expect("декодер обученного кадра");
    let dec_t = t.elapsed();
    assert_eq!(dec_trained, raw, "roundtrip обученного кадра не сошёлся");

    fs::create_dir_all(outdir).expect("создание каталога вывода");
    let stem = file_stem(input);
    fs::write(format!("{outdir}/{stem}.stuv_static"), &frame_static).expect("запись кадра стат");
    fs::write(format!("{outdir}/{stem}.stuv_trained"), &frame_trained)
        .expect("запись кадра обуч");
    fs::write(format!("{outdir}/{stem}.restored_static"), &dec_static)
        .expect("запись восстановления стат");
    fs::write(format!("{outdir}/{stem}.restored_trained"), &dec_trained)
        .expect("запись восстановления обуч");

    let tc = trained.trained_counts();
    println!(
        "RESULT\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        stem,
        raw.len(),
        frame_static.len(),
        frame_trained.len(),
        utf8_ok,
        enc_s.as_secs_f64() * 1000.0,
        dec_s.as_secs_f64() * 1000.0,
        train_d.as_secs_f64() * 1000.0,
        enc_t.as_secs_f64() * 1000.0,
        dec_t.as_secs_f64() * 1000.0,
        tc[0], tc[1], tc[2], tc[3], tc[4],
        st_static.payload, st_static.dict_bytes,
        st_trained.payload, st_trained.dict_bytes,
    );
    eprintln!("encode {stem}: OK (static {} Б, trained {} Б)", frame_static.len(), frame_trained.len());
}

fn decode_cmd(frame_path: &str, output: &str) {
    let frame = fs::read(frame_path).expect("чтение кадра");
    let out = codec::decode(&frame).expect("декодирование кадра");
    fs::write(output, &out).expect("запись выхода");
    eprintln!("decode {}: OK ({} Б)", frame_path, out.len());
    println!("DECODED\t{}\t{}", output, out.len());
}

/// Разделение готового кадра на управляющий поток и поток данных с проверкой
/// обратной сборки и декодирования. Обучение не требуется — кадр уже собран.
fn dumpstreams_cmd(frame_path: &str, outdir: &str) {
    let frame = fs::read(frame_path).expect("чтение кадра");
    let split = codec::split_streams(&frame).expect("разделение потоков");
    let rebuilt = codec::recompose_frame(&frame, &split).expect("обратная сборка");
    assert_eq!(rebuilt, frame, "реконструированный кадр не совпал с исходным");
    let raw = codec::decode(&rebuilt).expect("декодер реконструированного кадра");

    fs::create_dir_all(outdir).expect("создание каталога вывода");
    let stem = file_stem(frame_path);
    let stem = match stem.rsplit_once('.') {
        Some((base, tag)) if tag == "stuv_static" || tag == "stuv_trained" => base.to_string(),
        _ => stem,
    };
    fs::write(format!("{outdir}/{stem}.ctrl"), &split.ctrl).expect("запись ctrl");
    fs::write(format!("{outdir}/{stem}.data"), &split.data).expect("запись data");
    fs::write(format!("{outdir}/{stem}.recomposed"), &rebuilt).expect("запись recomposed");
    println!(
        "RESULT2\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        stem,
        raw.len(),
        frame.len(),
        split.header_len,
        split.ctrl.len(),
        split.data.len(),
        split.ascii_runs,
        split.ascii_bytes,
        split.run_tokens,
        split.run_bytes,
        split.raw_tokens,
        split.token_tokens,
        split.ctrl.len() + split.data.len(),
        split.header_len + split.ctrl.len() + split.data.len(),
    );
    eprintln!(
        "dumpstreams {stem}: ctrl {} Б + data {} Б (кадр {} Б, заголовок {} Б)",
        split.ctrl.len(),
        split.data.len(),
        frame.len(),
        split.header_len
    );
}

/// Частотное перекодирование ctrl-потока: ранги токенов + арифметический
/// кодер порядка 1 (или 0 с --order0). Кодер сам проверяет roundtrip.
fn frec_cmd(ctrl_path: &str, outdir: &str, flags: &[String]) {
    let order0 = flags.iter().any(|f| f == "--order0");
    let ctrl = fs::read(ctrl_path).expect("чтение ctrl");
    let (parts, events, alphabet) =
        steno_uv::frec::frec_encode(&ctrl, order0).expect("frec кодирование");
    fs::create_dir_all(outdir).expect("создание каталога вывода");
    let stem = file_stem(ctrl_path);
    let stem = match stem.rsplit_once('.') {
        Some((base, tag)) if tag == "ctrl" => base.to_string(),
        _ => stem,
    };
    fs::write(format!("{outdir}/{stem}.ftab"), &parts.ftab).expect("запись ftab");
    fs::write(format!("{outdir}/{stem}.fmain"), &parts.fmain).expect("запись fmain");
    fs::write(format!("{outdir}/{stem}.fesc"), &parts.fesc).expect("запись fesc");
    let total = parts.ftab.len() + parts.fmain.len() + parts.fesc.len();
    println!(
        "RESULT3\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        stem,
        ctrl.len(),
        parts.ftab.len(),
        parts.fmain.len(),
        parts.fesc.len(),
        total,
        events,
        alphabet,
        if order0 { 0 } else { 1 },
    );
    eprintln!(
        "frec {stem}: {} Б → ftab {} + fmain {} + fesc {} = {} Б ({} событий, алфавит {})",
        ctrl.len(),
        parts.ftab.len(),
        parts.fmain.len(),
        parts.fesc.len(),
        total,
        events,
        alphabet
    );
}

/// CM-кодирование ctrl-потока (lpaq/zpaq-класс): контексты порядков 0–6 +
/// match-модель + логистический миксер + APM; roundtrip проверяется внутри.
fn cmz_cmd(ctrl_path: &str, outdir: &str) {
    let ctrl = fs::read(ctrl_path).expect("чтение ctrl");
    let t0 = Instant::now();
    // файловый формат: uvarint(длина) + CM-поток — длина нужна декодеру
    let cmz = lpaq_ctrl::cmz_encode_file(&ctrl).expect("cmz кодирование");
    let dt = t0.elapsed();
    fs::create_dir_all(outdir).expect("создание каталога вывода");
    // имя входа целиком (677647.ctrl → 677647.ctrl.cmz), иначе оба потока
    // пишут в один файл: file_stem съедает тег потока
    let name = std::path::Path::new(ctrl_path)
        .file_name()
        .and_then(|s| s.to_str())
        .expect("имя файла потока")
        .to_string();
    fs::write(format!("{outdir}/{name}.cmz"), &cmz).expect("запись cmz");
    println!("RESULT4\t{}\t{}\t{}\t{}", name, ctrl.len(), cmz.len(), dt.as_secs_f64());
    eprintln!(
        "cmz {name}: {} Б → {} Б ({:.2}×, {:.1} c)",
        ctrl.len(),
        cmz.len(),
        ctrl.len() as f64 / cmz.len() as f64,
        dt.as_secs_f64()
    );
}

/// CM-распаковка файлового формата cmz (uvarint-длина + CM-поток).
fn cmz_decode_cmd(cmz_path: &str, output: &str) {
    let data = fs::read(cmz_path).expect("чтение cmz");
    let t0 = Instant::now();
    let out = lpaq_ctrl::cmz_decode_file(&data).expect("cmz декодирование");
    let dt = t0.elapsed();
    fs::write(output, &out).expect("запись распакованного потока");
    println!(
        "RESULT5\t{}\t{}\t{}\t{}",
        cmz_path,
        data.len(),
        out.len(),
        dt.as_secs_f64()
    );
    eprintln!(
        "cmz-decode {}: {} Б → {} Б ({:.1} c)",
        cmz_path,
        data.len(),
        out.len(),
        dt.as_secs_f64()
    );
}

/// Сборка кадра из частей архива (заголовок + ctrl + data) с контролем декодером.
fn pack_cmd(hdr_path: &str, ctrl_path: &str, data_path: &str, output: &str) {
    let hdr = fs::read(hdr_path).expect("чтение заголовка");
    let ctrl = fs::read(ctrl_path).expect("чтение ctrl");
    let data = fs::read(data_path).expect("чтение data");
    let t0 = Instant::now();
    let frame = codec::pack_frame(&hdr, &ctrl, &data).expect("сборка кадра");
    codec::decode(&frame).expect("контрольное декодирование собранного кадра");
    let dt = t0.elapsed();
    fs::write(output, &frame).expect("запись кадра");
    println!(
        "RESULT6\t{}\t{}\t{}\t{}",
        output,
        frame.len(),
        dt.as_secs_f64(),
        hdr.len() + ctrl.len() + data.len()
    );
    eprintln!(
        "pack: {} + {} + {} Б → кадр {} Б ({:.1} c)",
        hdr.len(),
        ctrl.len(),
        data.len(),
        frame.len(),
        dt.as_secs_f64()
    );
}

/// Полная распаковка архива пайплайна: <каталог>/<stem>.hdr + .ctrl.cmz + .data.cmz.
fn unpack_cmd(dir: &str, stem: &str, output: &str) {
    let t_all = Instant::now();
    let hdr = fs::read(format!("{dir}/{stem}.hdr")).expect("чтение заголовка архива");
    let ctrl_c = fs::read(format!("{dir}/{stem}.ctrl.cmz")).expect("чтение ctrl.cmz");
    let data_c = fs::read(format!("{dir}/{stem}.data.cmz")).expect("чтение data.cmz");
    let ctrl = lpaq_ctrl::cmz_decode_file(&ctrl_c).expect("CM-декодирование ctrl");
    let data = lpaq_ctrl::cmz_decode_file(&data_c).expect("CM-декодирование data");
    let frame = codec::pack_frame(&hdr, &ctrl, &data).expect("сборка кадра");
    let raw = codec::decode(&frame).expect("декодирование кадра");
    fs::write(output, &raw).expect("запись исходного файла");
    println!(
        "RESULT7\t{}\t{}\t{}\t{}",
        output,
        raw.len(),
        hdr.len() + ctrl_c.len() + data_c.len(),
        t_all.elapsed().as_secs_f64()
    );
    eprintln!(
        "unpack {stem}: {} Б из архива {} Б ({:.1} c)",
        raw.len(),
        hdr.len() + ctrl_c.len() + data_c.len(),
        t_all.elapsed().as_secs_f64()
    );
}
