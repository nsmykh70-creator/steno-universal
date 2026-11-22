//! Reproducible in-process benchmark primitives for STENO-P8.
use crate::integrity;
use std::time::{Duration, Instant};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::thread;

#[derive(Debug, Clone)]
pub struct BenchResult {
    pub name: String,
    pub input_bytes: usize,
    pub archive_bytes: usize,
    pub encode: Duration,
    pub decode: Duration,
    pub verified: bool,
}
impl BenchResult {
    pub fn ratio_percent(&self) -> f64 { if self.input_bytes == 0 { 0.0 } else { self.archive_bytes as f64 * 100.0 / self.input_bytes as f64 } }
    pub fn encode_mib_s(&self) -> f64 { throughput(self.input_bytes, self.encode) }
    pub fn decode_mib_s(&self) -> f64 { throughput(self.input_bytes, self.decode) }
}
fn throughput(bytes: usize, elapsed: Duration) -> f64 {
    if elapsed.is_zero() { return 0.0; }
    bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64()
}

/// Measures encode and decode independently and refuses to mark a failed roundtrip successful.
pub fn benchmark(name: impl Into<String>, input: &[u8], repetitions: usize) -> BenchResult {
    let reps = repetitions.max(1);
    let mut archive = Vec::new();
    let start = Instant::now();
    for _ in 0..reps { archive = integrity::encode(input).0; }
    let encode = start.elapsed() / reps as u32;
    let start = Instant::now();
    let mut decoded = Vec::new();
    for _ in 0..reps { decoded = integrity::decode(&archive, input.len()).unwrap_or_default(); }
    let decode = start.elapsed() / reps as u32;
    BenchResult { name: name.into(), input_bytes: input.len(), archive_bytes: archive.len(), encode, decode, verified: decoded == input }
}



#[derive(Debug, Clone)]
pub struct ExternalBenchResult {
    pub codec: String,
    pub archive_bytes: usize,
    pub encode: Duration,
    pub decode: Duration,
    pub verified: bool,
}

/// Runs a command-line compressor/decompressor through stdin/stdout. Output is
/// drained concurrently to avoid pipe deadlocks on larger samples.
fn run_filter(program: &str, args: &[&str], input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || { let mut out = Vec::new(); stdout.read_to_end(&mut out).map(|_| out) });
    let write_result = child.stdin.take()?.write_all(input);
    let status = child.wait().ok()?;
    let output = reader.join().ok()?.ok()?;
    if write_result.is_err() || !status.success() { None } else { Some(output) }
}

/// Compare a command-line codec using the exact same bytes. `None` means the
/// executable is missing or the operation failed; it is never reported as zero.
pub fn benchmark_external(program: &str, encode_args: &[&str], decode_args: &[&str], input: &[u8], repetitions: usize) -> Option<ExternalBenchResult> {
    let reps = repetitions.max(1);
    let start = Instant::now();
    let mut archive = Vec::new();
    for _ in 0..reps { archive = run_filter(program, encode_args, input)?; }
    let encode = start.elapsed() / reps as u32;
    let start = Instant::now();
    let mut decoded = Vec::new();
    for _ in 0..reps { decoded = run_filter(program, decode_args, &archive)?; }
    let decode = start.elapsed() / reps as u32;
    Some(ExternalBenchResult { codec: program.into(), archive_bytes: archive.len(), encode, decode, verified: decoded == input })
}

/// Deterministic representative corpora; synthetic data is explicitly labelled as such.
pub fn sample_corpora() -> Vec<(String, Vec<u8>)> {
    let text = ("the quick brown fox jumps over the lazy dog. русский текст и данные. 日本語の文章。\n").repeat(512);
    let math = ("∫_0^∞ e^{-x²} dx = √π/2; a²+b²=c²; Σ(i=1..n) i = n(n+1)/2\n").repeat(512);
    let code = ("fn calculate(x: i64) -> i64 { let square = x * x; if square > 10 { return square; } square + 1 }\n").repeat(512);
    let binary: Vec<u8> = (0..65_536).map(|i| ((i * 73 + (i >> 3) * 19) & 255) as u8).collect();
    vec![("synthetic-text".into(), text.into_bytes()), ("synthetic-math".into(), math.into_bytes()), ("synthetic-code".into(), code.into_bytes()), ("synthetic-binary".into(), binary)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn benchmark_requires_exact_roundtrip() {
        let r = benchmark("tiny", b"abcabcabc", 2);
        assert!(r.verified); assert!(r.archive_bytes > 0); assert!(r.encode_mib_s() >= 0.0);
    }
    #[test] fn corpus_is_deterministic() {
        let a = sample_corpora(); let b = sample_corpora();
        assert_eq!(a, b); assert_eq!(a.len(), 4);
    }
}
