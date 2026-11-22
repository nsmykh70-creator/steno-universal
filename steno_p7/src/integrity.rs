//! STENO-P8 Stage 20: integrity envelope around the experimental SP8 frame.
//! CRC64 detects accidental corruption; it is not a cryptographic authenticator.
use crate::joint;

const MAGIC: &[u8; 4] = b"S20\0";
const VERSION: u8 = 1;
const HEADER_LEN: usize = 4 + 1 + 8 + 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityError { InvalidHeader, UnsupportedVersion, Truncated, LengthMismatch, ChecksumMismatch, OutputLimit, InnerCodec }

impl std::fmt::Display for IntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") }
}
impl std::error::Error for IntegrityError {}

/// CRC-64/ECMA-182. Detects accidental corruption, not malicious modification.
pub fn crc64(data: &[u8]) -> u64 {
    const POLY: u64 = 0x42F0_E1EB_A9EA_3693;
    let mut crc = 0u64;
    for &byte in data {
        crc ^= (byte as u64) << 56;
        for _ in 0..8 { crc = if crc & (1 << 63) != 0 { (crc << 1) ^ POLY } else { crc << 1 }; }
    }
    crc
}

/// Produces a checksummed S20 envelope containing the reversible SP8 frame.
pub fn encode(input: &[u8]) -> (Vec<u8>, usize) {
    let (inner, _) = joint::encode(input);
    let mut out = Vec::with_capacity(HEADER_LEN + inner.len());
    out.extend_from_slice(MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&(input.len() as u64).to_be_bytes());
    out.extend_from_slice(&crc64(input).to_be_bytes());
    out.extend_from_slice(&inner);
    let size = out.len();
    (out, size)
}

/// Verifies envelope, bounds, decoded length, and checksum before returning bytes.
pub fn decode(frame: &[u8], max_output: usize) -> Result<Vec<u8>, IntegrityError> {
    if frame.len() < HEADER_LEN { return Err(IntegrityError::Truncated); }
    if frame.get(..4) != Some(MAGIC.as_slice()) { return Err(IntegrityError::InvalidHeader); }
    if frame[4] != VERSION { return Err(IntegrityError::UnsupportedVersion); }
    let expected_u64 = u64::from_be_bytes(frame[5..13].try_into().map_err(|_| IntegrityError::Truncated)?);
    let expected = usize::try_from(expected_u64).map_err(|_| IntegrityError::OutputLimit)?;
    if expected > max_output { return Err(IntegrityError::OutputLimit); }
    let checksum = u64::from_be_bytes(frame[13..21].try_into().map_err(|_| IntegrityError::Truncated)?);
    let payload = &frame[HEADER_LEN..];
    let decoded = joint::decode(payload, max_output).map_err(|_| IntegrityError::InnerCodec)?;
    if decoded.len() != expected { return Err(IntegrityError::LengthMismatch); }
    if crc64(&decoded) != checksum { return Err(IntegrityError::ChecksumMismatch); }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn crc64_ecma_check_vector() { assert_eq!(crc64(b"123456789"), 0x6C40_DF5F_0B49_7347); }
    #[test] fn envelope_roundtrip_empty_and_binary() {
        for input in [&b""[..], &[0, 1, 127, 128, 255][..], "Русский ∫x² 日本語".as_bytes()] {
            let (frame, _) = encode(input);
            assert_eq!(decode(&frame, 1_000_000).unwrap(), input);
        }
    }
    #[test] fn detects_header_payload_corruption_and_truncation() {
        let (frame, _) = encode(b"repeat repeat repeat");
        let mut bad = frame.clone(); bad[13] ^= 1;
        assert_eq!(decode(&bad, 1000), Err(IntegrityError::ChecksumMismatch));
        assert_eq!(decode(&frame[..frame.len()-1], 1000), Err(IntegrityError::InnerCodec));
        assert_eq!(decode(&frame[..8], 1000), Err(IntegrityError::Truncated));
    }
    #[test] fn enforces_output_limit_before_inner_decode() {
        let (frame, _) = encode(b"large output");
        assert_eq!(decode(&frame, 2), Err(IntegrityError::OutputLimit));
    }
}
