use std::{
    env, fs,
    io::{self, Read, Write},
    process::{self, Command, Stdio},
    thread,
    time::Instant,
};
use steno_p7::StenoP7;

#[derive(Debug)]
struct BenchRow {
    stage: String,
    codec: String,
    output_bytes: Option<usize>,
    ratio_percent: Option<f64>,
    elapsed_ms: Option<f64>,
    status: &'static str,
}

fn usage() {
    eprintln!(
        "STENO-P7 v2\nUsage:\n  steno-p7 compress <input.txt> <output.sp7>\n  steno-p7 decompress <input.sp7> <output.txt>\n  steno-p7 roundtrip <input.txt>\n  steno-p7 bench <input.txt> [report.csv]"
    );
}

fn run_filter(
    program: &str,
    args: &[&str],
    input: &[u8],
) -> Result<Option<(usize, f64)>, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Box::new(error)),
    };

    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "compressor stdin unavailable"))?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "compressor stdout unavailable"))?;

    // Read stdout and write stdin concurrently: a compressor can otherwise block
    // when its output pipe fills before the input writer has finished.
    let input_copy = input.to_vec();
    let writer = thread::spawn(move || child_stdin.write_all(&input_copy));
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        child_stdout.read_to_end(&mut output).map(|_| output)
    });

    let status = child.wait()?;
    writer
        .join()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "compressor input thread panicked"))??;
    let output = reader
        .join()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "compressor output thread panicked"))??;

    if !status.success() {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::Other,
            format!("{program} exited with status {status}"),
        )));
    }
    Ok(Some((output.len(), started.elapsed().as_secs_f64() * 1000.0)))
}

fn push_external_row(
    rows: &mut Vec<BenchRow>,
    original_size: usize,
    stage: &str,
    codec_name: &str,
    program: &str,
    args: &[&str],
    input: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    match run_filter(program, args, input)? {
        Some((size, elapsed_ms)) => rows.push(BenchRow {
            stage: stage.to_owned(),
            codec: codec_name.to_owned(),
            output_bytes: Some(size),
            ratio_percent: Some(size as f64 * 100.0 / original_size.max(1) as f64),
            elapsed_ms: Some(elapsed_ms),
            status: "OK",
        }),
        None => rows.push(BenchRow {
            stage: stage.to_owned(),
            codec: codec_name.to_owned(),
            output_bytes: None,
            ratio_percent: None,
            elapsed_ms: None,
            status: "NOT_INSTALLED",
        }),
    }
    Ok(())
}

fn csv_report(original_size: usize, rows: &[BenchRow]) -> String {
    let mut csv = String::from(
        "input_bytes,stage,codec,output_bytes,ratio_percent,elapsed_ms,status\n",
    );
    for row in rows {
        let size = row.output_bytes.map(|v| v.to_string()).unwrap_or_default();
        let ratio = row
            .ratio_percent
            .map(|v| format!("{v:.4}"))
            .unwrap_or_default();
        let elapsed = row
            .elapsed_ms
            .map(|v| format!("{v:.3}"))
            .unwrap_or_default();
        csv.push_str(&format!(
            "{original_size},{},{},{size},{ratio},{elapsed},{}\n",
            row.stage, row.codec, row.status
        ));
    }
    csv
}

fn print_rows(original_size: usize, rows: &[BenchRow]) {
    println!(
        "{:<22} {:<12} {:>14} {:>12} {:>12}  {}",
        "STAGE", "CODEC", "OUTPUT BYTES", "RATIO", "TIME ms", "STATUS"
    );
    for row in rows {
        let size = row
            .output_bytes
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_owned());
        let ratio = row
            .ratio_percent
            .map(|v| format!("{v:.2}%"))
            .unwrap_or_else(|| "-".to_owned());
        let elapsed = row
            .elapsed_ms
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "{:<22} {:<12} {:>14} {:>12} {:>12}  {}",
            row.stage, row.codec, size, ratio, elapsed, row.status
        );
    }
    println!("Original input: {original_size} bytes");
}

fn run_benchmark(
    input_path: &str,
    report_path: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let input = fs::read(input_path)?;
    let text = String::from_utf8(input.clone())?;
    let original_size = input.len();
    let codec = StenoP7::new();
    let mut rows = Vec::new();

    rows.push(BenchRow {
        stage: "RAW".to_owned(),
        codec: "none".to_owned(),
        output_bytes: Some(original_size),
        ratio_percent: Some(100.0),
        elapsed_ms: Some(0.0),
        status: "OK",
    });

    let started = Instant::now();
    let steno_archive = codec.try_compress(&text)?;
    let steno_ms = started.elapsed().as_secs_f64() * 1000.0;
    if codec.decompress(&steno_archive)? != text {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            "STENO round-trip mismatch; benchmark aborted",
        )));
    }
    // Required baseline sequence: RAW through each reference compressor, then
    // STENO output through the same compressors to measure transform synergy.
    let compressors: [(&str, &str, &[&str]); 3] = [
        ("Zstd", "zstd", &["-q", "-c", "-19"]),
        ("LZMA2", "xz", &["--format=raw", "--lzma2=preset=9", "-c"]),
        ("GZIP", "gzip", &["-c", "-9"]),
    ];
    for &(name, program, args) in compressors.iter() {
        push_external_row(
            &mut rows,
            original_size,
            &format!("RAW -> {name}"),
            name,
            program,
            args,
            &input,
        )?;
    }
    for &(name, program, args) in compressors.iter() {
        push_external_row(
            &mut rows,
            original_size,
            &format!("STENO -> {name}"),
            name,
            program,
            args,
            &steno_archive,
        )?;
    }

    // Seventh comparison from the experiment plan: the STENO transform alone.
    rows.push(BenchRow {
        stage: "RAW -> STENO".to_owned(),
        codec: "STENO-P7".to_owned(),
        output_bytes: Some(steno_archive.len()),
        ratio_percent: Some(steno_archive.len() as f64 * 100.0 / original_size.max(1) as f64),
        elapsed_ms: Some(steno_ms),
        status: "OK",
    });

    print_rows(original_size, &rows);
    if let Some(path) = report_path {
        fs::write(path, csv_report(original_size, &rows))?;
        println!("CSV report written: {path}");
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
        process::exit(2)
    }
    let codec = StenoP7::new();
    match args[1].as_str() {
        "compress" if args.len() == 4 => {
            let input = fs::read(&args[2])?;
            let text = String::from_utf8(input)?;
            let archive = codec.try_compress(&text)?;
            fs::write(&args[3], &archive)?;
            println!(
                "{} -> {} bytes ({:.2}%)",
                text.len(),
                archive.len(),
                archive.len() as f64 * 100.0 / text.len().max(1) as f64
            );
        }
        "decompress" if args.len() == 4 => {
            let data = fs::read(&args[2])?;
            let text = codec.decompress(&data)?;
            fs::write(&args[3], text)?;
        }
        "roundtrip" if args.len() == 3 => {
            let input = fs::read_to_string(&args[2])?;
            let archive = codec.try_compress(&input)?;
            let output = codec.decompress(&archive)?;
            if input != output {
                return Err("roundtrip mismatch".into());
            }
            println!("roundtrip OK: {} -> {} bytes", input.len(), archive.len());
        }
        "bench" if args.len() == 3 || args.len() == 4 => {
            run_benchmark(&args[2], args.get(3).map(String::as_str))?;
        }
        _ => {
            usage();
            process::exit(2)
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        process::exit(1)
    }
}
