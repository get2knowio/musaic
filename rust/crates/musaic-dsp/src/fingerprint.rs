//! Chromaprint fingerprints and fingerprint similarity.
//!
//! The fingerprinter is rusty-chromaprint 0.3.0 with the TEST2 preset
//! (fpcalc's default algorithm) fed the first [`MAX_FINGERPRINT_SECONDS`] of
//! the stream, as `pyacoustid.fingerprint_file(maxlength=120)` asks fpcalc to
//! do. It is not bit-exact with fpcalc (its resampler differs), so
//! fingerprints from the two implementations must never be compared with each
//! other: callers store [`FINGERPRINT_IMPL_RUST`] next to every
//! fingerprint this module produces.

use std::path::Path;

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use rusty_chromaprint::{Configuration, FingerprintCompressor, Fingerprinter};

use crate::decode::{DecodeError, DecodedAudio, decode_flac_limited};

/// Implementation tag of fingerprints computed by the Python app's fpcalc.
/// Mirrors `musaic_db::FINGERPRINT_IMPL_FPCALC` (kept as a copy so this crate
/// does not depend on the persistence crate; the two must stay equal).
pub const FINGERPRINT_IMPL_FPCALC: &str = "fpcalc";
/// Implementation tag of fingerprints computed by this crate.
/// Mirrors `musaic_db::FINGERPRINT_IMPL_RUST`.
pub const FINGERPRINT_IMPL_RUST: &str = "rust-chromaprint";

/// How much audio is fingerprinted: fingerprint.py passes `maxlength=120`.
pub const MAX_FINGERPRINT_SECONDS: u64 = 120;

/// Chromaprint's "normal" 3-bit value that flags a following 5-bit
/// exceptional value.
const MAX_NORMAL_VALUE: u8 = 7;

#[derive(Debug, thiserror::Error)]
pub enum FingerprintError {
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("fingerprinter rejected the stream ({channels} channels at {sample_rate} Hz)")]
    Start { sample_rate: u32, channels: u32 },
    #[error("audio too short to produce a fingerprint")]
    Empty,
    #[error("invalid compressed fingerprint: {0}")]
    Malformed(String),
}

/// One file's fingerprint, as fpcalc reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFingerprint {
    /// Stream duration in whole seconds, truncated (fpcalc prints the
    /// duration as an integer; floor(total_samples / sample_rate) matches
    /// all 94 goldens, 23 of which would differ under rounding).
    pub duration_s: u64,
    /// Raw sub-fingerprints (`fpcalc -raw`).
    pub raw: Vec<u32>,
    /// Compressed, URL-safe base64 form without padding (`fpcalc` default
    /// output, what AcoustID accepts and what the database stores).
    pub compressed: String,
}

fn config() -> Configuration {
    Configuration::preset_test2()
}

fn base64_engine() -> GeneralPurpose {
    GeneralPurpose::new(
        &alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_encode_padding(false)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    )
}

/// Fingerprint a FLAC file: decodes only the first 120 s plus the
/// STREAMINFO length.
pub fn fingerprint_file(path: &Path) -> Result<FileFingerprint, FingerprintError> {
    let probe = claxon::FlacReader::open(path).map_err(|source| DecodeError::Flac {
        path: path.to_path_buf(),
        source,
    })?;
    let rate = u64::from(probe.streaminfo().sample_rate);
    drop(probe);
    let audio = decode_flac_limited(path, Some(MAX_FINGERPRINT_SECONDS * rate))?;
    fingerprint_audio(&audio)
}

/// Fingerprint already-decoded audio (only the first 120 s are used).
pub fn fingerprint_audio(audio: &DecodedAudio) -> Result<FileFingerprint, FingerprintError> {
    let channels = audio.channels.max(1) as usize;
    let max_frames = (MAX_FINGERPRINT_SECONDS * u64::from(audio.sample_rate)) as usize;
    let frames = audio.frames().min(max_frames);
    let shift = audio.bits_per_sample as i32 - 16;
    // Same reduction to 16 bits as the parity spike: an arithmetic shift.
    // (All fixtures are 16-bit, so this branch is untested against fpcalc.)
    let pcm: Vec<i16> = audio.samples[..frames * channels]
        .iter()
        .map(|&s| {
            if shift >= 0 {
                (s >> shift) as i16
            } else {
                (s << -shift) as i16
            }
        })
        .collect();

    let cfg = config();
    let mut fp = Fingerprinter::new(&cfg);
    fp.start(audio.sample_rate, audio.channels)
        .map_err(|_| FingerprintError::Start {
            sample_rate: audio.sample_rate,
            channels: audio.channels,
        })?;
    fp.consume(&pcm);
    fp.finish();
    let raw = fp.fingerprint().to_vec();
    if raw.is_empty() {
        return Err(FingerprintError::Empty);
    }
    let compressed = compress_fingerprint(&raw);
    let duration_s = if audio.sample_rate == 0 {
        0
    } else {
        audio.total_frames / u64::from(audio.sample_rate)
    };
    Ok(FileFingerprint {
        duration_s,
        raw,
        compressed,
    })
}

/// Raw sub-fingerprints to the compressed base64 string (TEST2 algorithm id).
pub fn compress_fingerprint(raw: &[u32]) -> String {
    let cfg = config();
    base64_engine().encode(FingerprintCompressor::from(&cfg).compress(raw))
}

/// A decompressed fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecompressedFingerprint {
    /// Algorithm id from the header byte (1 = TEST2).
    pub algorithm: u8,
    pub raw: Vec<u32>,
}

/// Unpack a little-endian bit stream of `N`-bit values.
fn unpack_intn<const N: usize>(bytes: &[u8]) -> Vec<u8> {
    let count = bytes.len() * 8 / N;
    let mask = (1u16 << N) - 1;
    (0..count)
        .map(|i| {
            let bit = i * N;
            let byte = bit / 8;
            let lo = u16::from(bytes[byte]);
            let hi = u16::from(bytes.get(byte + 1).copied().unwrap_or(0));
            (((lo | (hi << 8)) >> (bit % 8)) & mask) as u8
        })
        .collect()
}

const fn packed_len(values: usize, n: usize) -> usize {
    (values * n).div_ceil(8)
}

/// Compressed base64 string back to raw sub-fingerprints: a port of
/// Chromaprint's `FingerprintDecompressor` (rusty-chromaprint 0.3.0 ships a
/// compressor only).
pub fn decompress_fingerprint(encoded: &str) -> Result<DecompressedFingerprint, FingerprintError> {
    let input = base64_engine()
        .decode(encoded.as_bytes())
        .map_err(|e| FingerprintError::Malformed(format!("base64: {e}")))?;
    if input.len() < 4 {
        return Err(FingerprintError::Malformed(
            "shorter than the header".into(),
        ));
    }
    let algorithm = input[0];
    let num_values =
        (usize::from(input[1]) << 16) | (usize::from(input[2]) << 8) | usize::from(input[3]);
    let mut offset = 4;
    let mut bits = unpack_intn::<3>(&input[offset..]);

    let mut found = 0usize;
    let mut exceptional = 0usize;
    let mut end = 0usize;
    if num_values > 0 {
        for (i, &b) in bits.iter().enumerate() {
            if b == 0 {
                found += 1;
                if found == num_values {
                    end = i + 1;
                    break;
                }
            } else if b == MAX_NORMAL_VALUE {
                exceptional += 1;
            }
        }
    }
    if found != num_values {
        return Err(FingerprintError::Malformed(format!(
            "header promises {num_values} values, found {found}"
        )));
    }
    bits.truncate(end);
    offset += packed_len(bits.len(), 3);
    if input.len() < offset + packed_len(exceptional, 5) {
        return Err(FingerprintError::Malformed(
            "exceptional bits truncated".into(),
        ));
    }
    let mut values: Vec<u32> = bits.iter().map(|&b| u32::from(b)).collect();
    if exceptional > 0 {
        let extra = unpack_intn::<5>(&input[offset..]);
        let mut j = 0;
        for v in values.iter_mut() {
            if *v == u32::from(MAX_NORMAL_VALUE) {
                *v += u32::from(extra[j]);
                j += 1;
            }
        }
    }

    let mut raw = Vec::with_capacity(num_values);
    let (mut last_bit, mut value) = (0u32, 0u32);
    for v in values {
        if v == 0 {
            let item = match raw.last() {
                Some(prev) => value ^ prev,
                None => value,
            };
            raw.push(item);
            value = 0;
            last_bit = 0;
            continue;
        }
        let bit = v + last_bit;
        if bit > 32 {
            return Err(FingerprintError::Malformed(format!(
                "bit index {bit} out of range"
            )));
        }
        last_bit = bit;
        value |= 1 << (bit - 1);
    }
    Ok(DecompressedFingerprint { algorithm, raw })
}

/// Bitwise similarity of two raw fingerprints over their common prefix:
/// `1 - popcount(a ^ b) / (32 * min_len)`, 0.0 when either is empty.
pub fn raw_similarity(a: &[u32], b: &[u32]) -> f64 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let differing: u64 = a[..n]
        .iter()
        .zip(&b[..n])
        .map(|(x, y)| u64::from((x ^ y).count_ones()))
        .sum();
    1.0 - differing as f64 / (n as f64 * 32.0)
}

/// Port of `musaic.fingerprint.chromaprint_similarity`: 0.0 for an
/// empty string, 1.0 for byte-identical strings, otherwise [`raw_similarity`]
/// of the decoded fingerprints. A string that does not decode counts as
/// empty, as the Python app's `_decode_fingerprint` returns `[]` when
/// `chromaprint_decode_fingerprint` fails.
pub fn chromaprint_similarity(fp_a: &str, fp_b: &str) -> f64 {
    if fp_a.is_empty() || fp_b.is_empty() {
        return 0.0;
    }
    if fp_a == fp_b {
        return 1.0;
    }
    let decode = |s: &str| decompress_fingerprint(s).map(|d| d.raw).unwrap_or_default();
    raw_similarity(&decode(fp_a), &decode(fp_b))
}
