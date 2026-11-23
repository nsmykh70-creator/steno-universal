//! Утилита для реальных файлов: кодирование в кадры STENO-UV (статический и
//! обучаемый словари), проверка roundtrip и обратное декодирование.
//!
//! Использование:
//!   steno_file encode <вход> <каталог_вывода>  — кадры + восстановленные файлы
//!   steno_file decode <кадр> <выход>           — кадр → исходные байты
//!   steno_file cmz <поток> <каталог>           — CM-сжатие потока (файловый формат с длиной)
//!   steno_file cmz-decode <файл.cmz> <выход>   — CM-распаковка потока (SU21 распознаётся)
//!   steno_file cmz-frame <кадр> <каталог>      — CM-сжатие кадра целиком
//!   steno_file cmz-frame-decode <файл> <выход> — CM-распаковка кадра (SU21 распознаётся)
//!   steno_file s21-pack <исходник> <архив> <выход.s21>    — конверт SU21 (CRC-64)
//!   steno_file s21-unpack <архив.s21> <выход> [--raw-frame] — проверка+распаковка
//!   steno_file bench-s20 <файл> [--reps N]     — бенч по методологии Stage20 (CSV)
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
        Some("cmz-frame") if args.len() >= 4 => cmz_frame_cmd(&args[2], &args[3]),
        Some("cmz-frame-decode") if args.len() >= 4 => cmz_frame_decode_cmd(&args[2], &args[3]),
        Some("pack") if args.len() >= 6 => pack_cmd(&args[2], &args[3], &args[4], &args[5]),
        Some("unpack") if args.len() >= 5 => unpack_cmd(&args[2], &args[3], &args[4]),
        Some("s21-pack") if args.len() >= 5 => s21_pack_cmd(&args[2], &args[3], &args[4]),
        Some("s21-unpack") if args.len() >= 4 => s21_unpack_cmd(&args[2], &args[3], &args[4..]),
        Some("slz") if args.len() >= 4 => slz_cmd(&args[2], &args[3]),
        Some("slz-decode") if args.len() >= 4 => slz_decode_cmd(&args[2], &args[3]),
        Some("bench-s20") if args.len() >= 3 => bench_s20_cmd(&args[2], &args[3..]),
        _ => {
            eprintln!("использование: steno_file encode <вход> <каталог> | decode <кадр> <выход> | dumpstreams <кадр> <каталог> | frec <ctrl> <каталог> [--order0] | cmz <поток> <каталог> | cmz-decode <файл.cmz> <выход> | cmz-frame <кадр> <каталог> | cmz-frame-decode <файл> <выход> | s21-pack <исходник> <архив> <выход.s21> | s21-unpack <архив.s21> <выход> [--raw-frame] | slz <вход> <выход.s21> | slz-decode <архив.s21> <выход> | bench-s20 <файл> [--reps N] | pack <hdr> <ctrl> <data> <кадр> | unpack <каталог> <stem> <выход>");
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
            "--onc" => {
                cfg.onc_parts = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(cfg.onc_parts);
                i += 2;
            }
            "--no-stems" => {
                cfg.stem_flex = false;
                i += 1;
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
    if dec_static != raw {
        let diff = dec_static
            .iter()
            .zip(raw.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(dec_static.len().min(raw.len()));
        panic!(
            "roundtrip статического кадра не сошёлся: расхождение на байте {} из {} (длина декода {})\nисходник: {:?}\nдекод:    {:?}",
            diff,
            raw.len(),
            dec_static.len(),
            &raw[diff.saturating_sub(24)..(diff + 24).min(raw.len())],
            &dec_static[diff.saturating_sub(24)..(diff + 24).min(dec_static.len())]
        );
    }

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
    if dec_trained != raw {
        let diff = dec_trained
            .iter()
            .zip(raw.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(dec_trained.len().min(raw.len()));
        panic!(
            "roundtrip обученного кадра не сошёлся: расхождение на байте {} из {} (длина декода {})\nисходник: {:?}\nдекод:    {:?}",
            diff,
            raw.len(),
            dec_trained.len(),
            &raw[diff.saturating_sub(32)..(diff + 32).min(raw.len())],
            &dec_trained[diff.saturating_sub(32)..(diff + 32).min(dec_trained.len())]
        );
    }

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
/// Если вход обёрнут в конверт SU21 (PS21) — конверт снимается, а после
/// декодирования проверяются длина и CRC-64 до выдачи результата.
fn cmz_decode_cmd(cmz_path: &str, output: &str) {
    let data = fs::read(cmz_path).expect("чтение cmz");
    let t0 = Instant::now();
    let (payload, wrapped) = match steno_uv::integrity::try_unwrap(&data) {
        Some(inner) => (inner, true),
        None => (&data[..], false),
    };
    let out = lpaq_ctrl::cmz_decode_file(payload).expect("cmz декодирование");
    if wrapped {
        steno_uv::integrity::verify(&data, &out).expect("конверт SU21: проверка целостности");
    }
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

/// CM-кодирование кадра ЦЕЛИКОМ (PS19): один поток hdr+payload позволяет
/// энтропийному кодеру находить кросс-совпадения «словарь ↔ payload»
/// (словарные записи повторяются в литеральных данных), чего нет при
/// раздельном сжатии потоков. Roundtrip проверяется внутри cmz_encode_file.
fn cmz_frame_cmd(frame_path: &str, outdir: &str) {
    let frame = fs::read(frame_path).expect("чтение кадра");
    let t0 = Instant::now();
    let cmz = lpaq_ctrl::cmz_encode_file(&frame).expect("cmz-кадр кодирование");
    let dt = t0.elapsed();
    fs::create_dir_all(outdir).expect("создание каталога вывода");
    let name = std::path::Path::new(frame_path)
        .file_name()
        .and_then(|s| s.to_str())
        .expect("имя файла кадра")
        .to_string();
    fs::write(format!("{outdir}/{name}.cmz"), &cmz).expect("запись cmz-кадра");
    println!(
        "RESULT7\t{}\t{}\t{}\t{}",
        name,
        frame.len(),
        cmz.len(),
        dt.as_secs_f64()
    );
    eprintln!(
        "cmz-frame {name}: {} Б → {} Б ({:.2}×, {:.1} c)",
        frame.len(),
        cmz.len(),
        frame.len() as f64 / cmz.len() as f64,
        dt.as_secs_f64()
    );
}

/// CM-распаковка целого кадра (обратная к cmz-frame) с контролем декодером.
/// Конверт SU21 (PS21) распознаётся автоматически: конверт снимается, кадр
/// декодируется, после чего проверяются заявленная длина и CRC-64 исходных
/// байтов до записи результата.
fn cmz_frame_decode_cmd(cmz_path: &str, output: &str) {
    let data = fs::read(cmz_path).expect("чтение cmz-кадра");
    let t0 = Instant::now();
    let (payload, wrapped) = match steno_uv::integrity::try_unwrap(&data) {
        Some(inner) => (inner, true),
        None => (&data[..], false),
    };
    let frame = lpaq_ctrl::cmz_decode_file(payload).expect("cmz-кадр декодирование");
    let text = codec::decode(&frame).expect("декодер восстановленного кадра");
    if wrapped {
        steno_uv::integrity::verify(&data, &text).expect("конверт SU21: проверка целостности");
    }
    let dt = t0.elapsed();
    fs::write(output, &text).expect("запись выхода");
    println!(
        "RESULT8\t{}\t{}\t{}\t{}",
        cmz_path,
        data.len(),
        frame.len(),
        dt.as_secs_f64()
    );
    eprintln!(
        "cmz-frame-decode {cmz_path}: {} Б → {} Б кадра → {} Б текста ({:.1} c)",
        data.len(),
        frame.len(),
        text.len(),
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

/// PS21: обёртка готового архива в конверт SU21 (методика Stage20):
/// магия + версия + исходная длина + CRC-64 исходных байтов + архив.
fn s21_pack_cmd(orig_path: &str, archive_path: &str, output: &str) {
    let orig = fs::read(orig_path).expect("чтение исходного файла");
    let archive = fs::read(archive_path).expect("чтение архива");
    let t0 = Instant::now();
    let wrapped = steno_uv::integrity::wrap(&archive, &orig);
    let dt = t0.elapsed();
    fs::write(output, &wrapped).expect("запись конверта SU21");
    println!(
        "RESULT21\t{}\t{}\t{}\t{}\t{}",
        output,
        orig.len(),
        archive.len(),
        wrapped.len(),
        dt.as_secs_f64()
    );
    eprintln!(
        "s21-pack {output}: архив {} Б + конверт {} Б = {} Б ({:.3} c)",
        archive.len(),
        steno_uv::integrity::HEADER_LEN,
        wrapped.len(),
        dt.as_secs_f64()
    );
}

/// PS21: проверка конверта SU21 и распаковка в исходные байты. Длина и
/// CRC-64 сверяются с декодированными байтами ДО записи выхода.
/// По умолчанию вложенный архив — cmz-кадр (cmz_decode_file → codec::decode);
/// флаг --raw-frame — вложен stuv-кадр напрямую (только codec::decode).
fn s21_unpack_cmd(archive_path: &str, output: &str, flags: &[String]) {
    let raw_frame = flags.iter().any(|f| f == "--raw-frame");
    let data = fs::read(archive_path).expect("чтение конверта SU21");
    let t0 = Instant::now();
    let inner = steno_uv::integrity::try_unwrap(&data)
        .expect("вход не является конвертом SU21");
    let raw = if raw_frame {
        codec::decode(inner).expect("декодер вложенного кадра")
    } else {
        let frame = lpaq_ctrl::cmz_decode_file(inner).expect("CM-декодирование вложенного кадра");
        codec::decode(&frame).expect("декодер восстановленного кадра")
    };
    steno_uv::integrity::verify(&data, &raw).expect("конверт SU21: проверка целостности");
    let dt = t0.elapsed();
    fs::write(output, &raw).expect("запись исходного файла");
    println!(
        "RESULT22\t{}\t{}\t{}\t{}",
        output,
        raw.len(),
        data.len(),
        dt.as_secs_f64()
    );
    eprintln!(
        "s21-unpack {output}: {} Б исходника из {} Б конверта, CRC-64 OK ({:.1} c)",
        raw.len(),
        data.len(),
        dt.as_secs_f64()
    );
}

/// PS21: бенч реального файла по методологии Stage20 — полный размер
/// контейнера, раздельные тайминги encode/decode, обязательная побайтовая
/// верификация, конкуренты (xz/gzip/zstd) на тех же байтах. Строки CSV:
/// dataset,codec,input_bytes,archive_bytes,ratio_percent,encode_mib_s,decode_mib_s,roundtrip.
/// Наш пайплайн: train + stuv-кадр → cmz-кадр → конверт SU21.
fn bench_s20_cmd(input: &str, flags: &[String]) {
    let reps = flags
        .iter()
        .position(|f| f == "--reps")
        .and_then(|i| flags.get(i + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1);
    let raw = fs::read(input).expect("чтение входного файла");
    let name = std::path::Path::new(input)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(input)
        .to_string();

    // --- наш пайплайн: train + stuv-кадр ---
    let t_train = Instant::now();
    let trained =
        train::train(&raw, &Dictionary::static_only(), &TrainConfig::default()).expect("обучение");
    let train_d = t_train.elapsed();
    let t_enc = Instant::now();
    let (frame, _) = codec::encode(&raw, &trained).expect("кодирование кадра");
    let enc_d = t_enc.elapsed();
    let t_dec = Instant::now();
    let restored = codec::decode(&frame).expect("декодер кадра");
    let frame_dec_d = t_dec.elapsed();
    let frame_ok = restored == raw;

    // --- CM-сжатие кадра целиком (PS19) ---
    let t_cmz = Instant::now();
    let cmz = lpaq_ctrl::cmz_encode_file(&frame).expect("cmz-кадр кодирование");
    let cmz_enc_d = t_cmz.elapsed();
    let t_cmzd = Instant::now();
    let frame_back = lpaq_ctrl::cmz_decode_file(&cmz).expect("cmz-кадр декодирование");
    let cmz_dec_d = t_cmzd.elapsed();
    let cmz_ok = frame_back == frame;

    // --- конверт SU21 (PS21) ---
    let t_wrap = Instant::now();
    let container = steno_uv::integrity::wrap(&cmz, &raw);
    let wrap_d = t_wrap.elapsed();
    let t_unwrap = Instant::now();
    let inner = steno_uv::integrity::try_unwrap(&container).expect("конверт распознаётся");
    let frame_back2 = lpaq_ctrl::cmz_decode_file(inner).expect("cmz декодирование конверта");
    let raw_back = codec::decode(&frame_back2).expect("декодер кадра из конверта");
    steno_uv::integrity::verify(&container, &raw_back).expect("проверка SU21");
    let unwrap_d = t_unwrap.elapsed();
    let s21_ok = raw_back == raw;

    let pipeline_enc = train_d + enc_d + cmz_enc_d + wrap_d;
    let pipeline_dec = unwrap_d;
    let mib = raw.len() as f64 / 1_048_576.0;
    let speed = |d: std::time::Duration| {
        if d.is_zero() {
            0.0
        } else {
            mib / d.as_secs_f64()
        }
    };
    println!("dataset,codec,input_bytes,archive_bytes,ratio_percent,encode_mib_s,decode_mib_s,roundtrip");
    let row = |codec: &str, bytes: usize, enc: f64, dec: f64, ok: bool| {
        println!(
            "{},{},{},{},{:.3},{:.3},{:.3},{}",
            name,
            codec,
            raw.len(),
            bytes,
            bytes as f64 * 100.0 / raw.len().max(1) as f64,
            enc,
            dec,
            ok
        );
    };
    row("stuv-кадр", frame.len(), speed(enc_d), speed(frame_dec_d), frame_ok);
    row(
        "PS19-cmz-кадр",
        cmz.len(),
        speed(enc_d + cmz_enc_d),
        speed(cmz_dec_d + frame_dec_d),
        cmz_ok,
    );
    row(
        "PS21-s21",
        container.len(),
        speed(pipeline_enc),
        speed(pipeline_dec),
        s21_ok,
    );

    // --- SLZ (PS22): наш быстрый кодек на тех же байтах ---
    let t_slz = Instant::now();
    let slz_stream = steno_uv::slz::encode(&raw);
    let slz_container = steno_uv::integrity::wrap(&slz_stream, &raw);
    let slz_enc_d = t_slz.elapsed();
    let t_slzd = Instant::now();
    let slz_inner = steno_uv::integrity::try_unwrap(&slz_container).expect("SU21 у slz");
    let slz_back = steno_uv::slz::decode(slz_inner, raw.len()).expect("slz декодирование");
    steno_uv::integrity::verify(&slz_container, &slz_back).expect("проверка SU21 у slz");
    let slz_dec_d = t_slzd.elapsed();
    row(
        "PS22-slz",
        slz_container.len(),
        speed(slz_enc_d),
        speed(slz_dec_d),
        slz_back == raw,
    );
    eprintln!(
        "slz: поток {} Б, контейнер {} Б (конверт {} Б), enc {:.2} c dec {:.2} c",
        slz_stream.len(),
        slz_container.len(),
        steno_uv::integrity::HEADER_LEN,
        slz_enc_d.as_secs_f64(),
        slz_dec_d.as_secs_f64()
    );
    eprintln!(
        "стадии: train {:.1} c | stuv enc {:.1} c dec {:.1} c | cmz enc {:.1} c dec {:.1} c | wrap {:.3} c unwrap {:.1} c",
        train_d.as_secs_f64(),
        enc_d.as_secs_f64(),
        frame_dec_d.as_secs_f64(),
        cmz_enc_d.as_secs_f64(),
        cmz_dec_d.as_secs_f64(),
        wrap_d.as_secs_f64(),
        unwrap_d.as_secs_f64()
    );

    // --- конкуренты на тех же байтах (методика Stage20) ---
    for _ in 0..reps.saturating_sub(1) {
        // повторы таймингов: прогон конкурентами для усреднения
        let _ = run_filter("xz", &["-c"], &raw);
        let _ = run_filter("gzip", &["-c"], &raw);
    }
    for (codec_name, binary, enc_args, dec_args) in [
        ("xz", "xz", &["-c"][..], &["-d", "-c"][..]),
        ("gzip", "gzip", &["-c"][..], &["-d", "-c"][..]),
        // zstd: явные уровни 3 (дефолт CLI) и 19 (максимальное сжатие)
        ("zstd-3", "zstd", &["-q", "-3", "-c"][..], &["-q", "-d", "-c"][..]),
        ("zstd-19", "zstd", &["-q", "-19", "-c"][..], &["-q", "-d", "-c"][..]),
    ] {
        let t0 = Instant::now();
        let compressed = match run_filter(binary, &enc_args, &raw) {
            Some(c) => c,
            None => {
                eprintln!("SKIP: {codec_name} недоступен или не сжал {name}");
                continue;
            }
        };
        let enc = t0.elapsed();
        let t0 = Instant::now();
        let verified = match run_filter(binary, &dec_args, &compressed) {
            Some(d) => d == raw,
            None => false,
        };
        let dec = t0.elapsed();
        row(codec_name, compressed.len(), speed(enc), speed(dec), verified);
    }

    // --- lz4 и snappy: чистые Rust-порты, сравнение в этом же процессе
    // (без накладных расходов subprocess, «на лету» как SLZ) ---
    {
        let t0 = Instant::now();
        let compressed = lz4_flex::compress_prepend_size(&raw);
        let enc = t0.elapsed();
        let t0 = Instant::now();
        let verified = lz4_flex::decompress_size_prepended(&compressed)
            .map(|d| d == raw)
            .unwrap_or(false);
        let dec = t0.elapsed();
        row("lz4", compressed.len(), speed(enc), speed(dec), verified);
    }
    {
        let mut se = snap::raw::Encoder::new();
        let t0 = Instant::now();
        let compressed = se.compress_vec(&raw).expect("snappy encode");
        let enc = t0.elapsed();
        let mut sd = snap::raw::Decoder::new();
        let t0 = Instant::now();
        let verified = sd
            .decompress_vec(&compressed)
            .map(|out| out == raw)
            .unwrap_or(false);
        let dec = t0.elapsed();
        row("snappy", compressed.len(), speed(enc), speed(dec), verified);
    }
}

/// Запуск внешнего кодека через пайпы без дедлоков: stdout вычитывается
/// параллельным потоком (методика Stage20), затем stdin закрывается.
fn run_filter(program: &str, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    });
    {
        let mut stdin = child.stdin.take()?;
        stdin.write_all(input).ok()?;
    }
    let status = child.wait().ok()?;
    let out = reader.join().ok()?.ok()?;
    if status.success() {
        Some(out)
    } else {
        None
    }
}

/// PS22: быстрое сжатие SLZ (LZ77+Huffman) с конвертом SU21.
fn slz_cmd(input: &str, output: &str) {
    let raw = fs::read(input).expect("чтение входного файла");
    let t0 = Instant::now();
    let stream = steno_uv::slz::encode(&raw);
    let container = steno_uv::integrity::wrap(&stream, &raw);
    let dt = t0.elapsed();
    fs::write(output, &container).expect("запись SLZ-контейнера");
    println!(
        "RESULT23\t{}\t{}\t{}\t{}\t{}",
        input,
        raw.len(),
        stream.len(),
        container.len(),
        dt.as_secs_f64()
    );
    eprintln!(
        "slz {input}: {} Б → поток {} Б + конверт {} Б = {} Б ({:.2}×, {:.2} c)",
        raw.len(),
        stream.len(),
        steno_uv::integrity::HEADER_LEN,
        container.len(),
        raw.len() as f64 / container.len().max(1) as f64,
        dt.as_secs_f64()
    );
}

/// PS22: распаковка SLZ-контейнера (SU21 распознаётся, проверка до записи).
/// Голый SLZ-поток без конверта тоже принимается.
fn slz_decode_cmd(input: &str, output: &str) {
    let data = fs::read(input).expect("чтение SLZ-контейнера");
    let t0 = Instant::now();
    let (payload, wrapped) = match steno_uv::integrity::try_unwrap(&data) {
        Some(inner) => (inner, true),
        None => (&data[..], false),
    };
    let limit = if wrapped {
        steno_uv::integrity::declared_len(&data).unwrap_or(usize::MAX)
    } else {
        usize::MAX
    };
    let raw = steno_uv::slz::decode(payload, limit).expect("slz декодирование");
    if wrapped {
        steno_uv::integrity::verify(&data, &raw).expect("конверт SU21: проверка целостности");
    }
    let dt = t0.elapsed();
    fs::write(output, &raw).expect("запись исходного файла");
    println!(
        "RESULT24\t{}\t{}\t{}\t{}",
        output,
        raw.len(),
        data.len(),
        dt.as_secs_f64()
    );
    eprintln!(
        "slz-decode {output}: {} Б исходника из {} Б ({:.2} c)",
        raw.len(),
        data.len(),
        dt.as_secs_f64()
    );
}
