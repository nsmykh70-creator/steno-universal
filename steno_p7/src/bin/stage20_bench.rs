use steno_p7::stage20::{benchmark, benchmark_external, sample_corpora};

fn main() {
    println!("dataset,codec,input_bytes,archive_bytes,ratio_percent,encode_mib_s,decode_mib_s,roundtrip");
    let reps = std::env::args().nth(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(3).max(1);
    let mut all_ok = true;
    for (name, data) in sample_corpora() {
        let r = benchmark(name.clone(), &data, reps);
        all_ok &= r.verified;
        println!("{},{},{},{},{:.3},{:.3},{:.3},{}", name, "STENO-P8-S20", r.input_bytes, r.archive_bytes, r.ratio_percent(), r.encode_mib_s(), r.decode_mib_s(), r.verified);
        let competitors: [(&str, &[&str], &[&str]); 3] = [
            ("zstd", &["-q", "-c"], &["-q", "-d", "-c"]),
            ("xz", &["-c"], &["-d", "-c"]),
            ("gzip", &["-c"], &["-d", "-c"]),
        ];
        for (program, enc, dec) in competitors {
            if let Some(x) = benchmark_external(program, enc, dec, &data, reps) {
                all_ok &= x.verified;
                let ratio = if data.is_empty() { 0.0 } else { x.archive_bytes as f64 * 100.0 / data.len() as f64 };
                let enc_speed = if x.encode.is_zero() { 0.0 } else { data.len() as f64 / 1_048_576.0 / x.encode.as_secs_f64() };
                let dec_speed = if x.decode.is_zero() { 0.0 } else { data.len() as f64 / 1_048_576.0 / x.decode.as_secs_f64() };
                println!("{},{},{},{},{:.3},{:.3},{:.3},{}", name, program, data.len(), x.archive_bytes, ratio, enc_speed, dec_speed, x.verified);
            } else {
                eprintln!("SKIP: {program} unavailable or failed for {name}");
            }
        }
    }
    if !all_ok { eprintln!("ERROR: at least one roundtrip failed"); std::process::exit(2); }
}
