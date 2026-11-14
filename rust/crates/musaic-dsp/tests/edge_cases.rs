//! Edge and malformed cases, against tests/data/edge_goldens.json, which
//! tests/data/gen_edge_goldens.py produced by running the Python app's own
//! pyloudnorm 0.2.0 and audio_analysis helpers on LCG-generated PCM.

mod common;

use common::*;
use musaic_dsp::{
    FingerprintError, LoudnessError, aggregate_album, chromaprint_similarity, compress_fingerprint,
    decompress_fingerprint, fingerprint_file, format_gain, format_peak, integrated_loudness,
    sample_peak, track_replaygain_file,
};
use serde_json::Value;

fn edge() -> Value {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/edge_goldens.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Same 31-bit LCG as gen_edge_goldens.py.
fn lcg_pcm(seed: u64, channels: usize, segments: &[Value]) -> Vec<i64> {
    let mut state = seed;
    let mut out = Vec::new();
    for seg in segments {
        let amp = seg["amp"].as_i64().unwrap();
        for _ in 0..seg["frames"].as_u64().unwrap() as usize * channels {
            state = (state * 1_103_515_245 + 12_345) & 0x7FFF_FFFF;
            out.push(if amp == 0 {
                0
            } else {
                ((state >> 8) as i64) % (2 * amp + 1) - amp
            });
        }
    }
    out
}

fn num(v: &Value) -> f64 {
    match v {
        Value::String(s) => s.parse().unwrap(),
        _ => v.as_f64().unwrap(),
    }
}

/// Pyloudnorm's results and errors on
/// mono, >5 channels, too short, exactly one block, silence (-inf, written
/// as "+inf dB"), near silence below the absolute gate, odd rates/lengths.
#[test]
fn loudness_edge_cases_match_pyloudnorm() {
    let e = edge();
    let mut worst = 0.0f64;
    for sig in e["signals"].as_array().unwrap() {
        let name = sig["name"].as_str().unwrap();
        let channels = sig["channels"].as_u64().unwrap() as usize;
        let pcm = lcg_pcm(
            sig["seed"].as_u64().unwrap(),
            channels,
            sig["segments"].as_array().unwrap(),
        );
        assert_eq!(
            pcm.len() as u64,
            sig["check"]["len"].as_u64().unwrap(),
            "{name}"
        );
        assert_eq!(
            pcm.iter().sum::<i64>(),
            sig["check"]["sum"].as_i64().unwrap(),
            "{name}"
        );
        assert_eq!(
            pcm.iter().map(|x| x * x).sum::<i64>(),
            sig["check"]["sum_sq"].as_i64().unwrap(),
            "{name}"
        );
        let data: Vec<f64> = pcm.iter().map(|&x| x as f64 / 32768.0).collect();
        let rate = sig["rate"].as_u64().unwrap() as u32;
        let result = &sig["result"];
        match integrated_loudness(&data, channels, rate) {
            Ok(lufs) => {
                assert!(
                    result.get("error").is_none(),
                    "{name}: the Python app raised {}, Rust gave {lufs}",
                    result["error"]
                );
                let want = num(&result["lufs"]);
                if want.is_finite() {
                    let dev = (lufs - want).abs();
                    worst = worst.max(dev);
                    assert!(dev <= GAIN_TOLERANCE_DB, "{name}: {lufs} vs {want}");
                } else {
                    assert_eq!(lufs, want, "{name}");
                }
                let peak = sample_peak(&data);
                assert_eq!(peak, result["peak"].as_f64().unwrap(), "{name}");
                assert_eq!(
                    format_gain(-18.0, lufs),
                    result["track_gain"].as_str().unwrap(),
                    "{name}"
                );
                assert_eq!(
                    format_peak(peak),
                    result["track_peak"].as_str().unwrap(),
                    "{name}"
                );
            }
            Err(err) => {
                let want = result["error"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}: Rust error {err}, Python app {result}"));
                assert_eq!(format!("ValueError: {err}"), want, "{name}");
                assert!(matches!(
                    err,
                    LoudnessError::TooShort | LoudnessError::TooManyChannels
                ));
            }
        }
    }
    println!("[loudness edge] max |LUFS dev| on finite edge signals {worst:.3e} dB");
}

/// `_aggregate_album` on non-finite loudness, zero
/// durations and empty input, and its formatted strings.
#[test]
fn album_aggregate_matches_python_formula() {
    let e = edge();
    for case in e["albums"].as_array().unwrap() {
        let tracks: Vec<(f64, f64, f64)> = case["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (num(&t[0]), num(&t[1]), num(&t[2])))
            .collect();
        let got = aggregate_album(&tracks);
        match (&got, case["result"].is_null()) {
            (None, true) => {}
            (Some(a), false) => {
                let r = &case["result"];
                assert!(
                    (a.lufs - r["lufs"].as_f64().unwrap()).abs() <= 1e-9,
                    "{case}: {a:?}"
                );
                assert_eq!(a.peak, r["peak"].as_f64().unwrap(), "{case}");
                assert_eq!(a.album_gain, r["album_gain"].as_str().unwrap(), "{case}");
                assert_eq!(a.album_peak, r["album_peak"].as_str().unwrap(), "{case}");
            }
            _ => panic!("{case}: Rust {got:?}"),
        }
    }
}

/// "%+.2f dB" / "%.6f" formatting, including round-half-even ties,
/// negative zero, +inf and nan; and the reference tag.
#[test]
fn tag_strings_match_python_formatting() {
    let e = edge();
    for c in e["format_gain"].as_array().unwrap() {
        assert_eq!(
            format_gain(-18.0, num(&c["measured"])),
            c["formatted"].as_str().unwrap(),
            "{c}"
        );
    }
    for c in e["format_peak"].as_array().unwrap() {
        assert_eq!(
            format_peak(num(&c["peak"])),
            c["formatted"].as_str().unwrap(),
            "{c}"
        );
    }
    assert_eq!(
        musaic_dsp::reference_loudness_tag(),
        e["reference_loudness"].as_str().unwrap()
    );
}

/// Corrupt compressed strings are rejected instead of
/// decoding to garbage; the Python app's similarity semantics for
/// empty, identical and undecodable strings.
#[test]
fn malformed_fingerprint_strings() {
    let g = load_json(SYNTHETIC_GOLDENS);
    let f = g["files"].as_object().unwrap().values().next().unwrap();
    let good = golden_str(f, &["fingerprint", "fingerprint"]);
    let raw = golden_raw(f);

    assert!(matches!(
        decompress_fingerprint(""),
        Err(FingerprintError::Malformed(_))
    ));
    assert!(matches!(
        decompress_fingerprint("AQA"),
        Err(FingerprintError::Malformed(_))
    ));
    assert!(matches!(
        decompress_fingerprint("not base64!"),
        Err(FingerprintError::Malformed(_))
    ));
    let truncated = &good[..good.len() / 2];
    assert!(matches!(
        decompress_fingerprint(truncated),
        Err(FingerprintError::Malformed(_))
    ));
    // Header claiming far more values than the body holds. (A small excess
    // can be absorbed by zero padding at the end of the stream; Chromaprint's
    // decompressor reads the same padding, so that is not checked here.)
    let mut bytes = base64_decode(good);
    let n =
        (usize::from(bytes[1]) << 16 | usize::from(bytes[2]) << 8 | usize::from(bytes[3])) + 10_000;
    bytes[1..4].copy_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    assert!(decompress_fingerprint(&base64_encode(&bytes)).is_err());
    // Empty fingerprint compresses to a bare header and back.
    assert_eq!(
        decompress_fingerprint(&compress_fingerprint(&[]))
            .unwrap()
            .raw,
        Vec::<u32>::new()
    );

    assert_eq!(chromaprint_similarity("", good), 0.0);
    assert_eq!(chromaprint_similarity(good, ""), 0.0);
    assert_eq!(chromaprint_similarity(good, good), 1.0);
    assert_eq!(chromaprint_similarity("garbage", "garbage"), 1.0); // byte-equality fast path
    assert_eq!(chromaprint_similarity(good, "garbage"), 0.0);
    assert_eq!(chromaprint_similarity(good, truncated), 0.0);
    // Prefix semantics: similarity is over min(len) items.
    let prefix = compress_fingerprint(&raw[..raw.len() / 2]);
    assert_eq!(chromaprint_similarity(good, &prefix), 1.0);
}

fn base64_decode(s: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .unwrap()
}

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

/// An AppleDouble `._` stub and a missing
/// file are errors, not fingerprints or measurements.
#[test]
fn undecodable_files_are_errors() {
    let lib = synthetic_library_dir();
    let stub = lib.join("Mono Harbor - Dotfiles [FLAC]/._01 - Ghost.flac");
    assert!(
        stub.is_file(),
        "{} missing from the fixture library",
        stub.display()
    );
    assert!(fingerprint_file(&stub).is_err());
    assert!(track_replaygain_file(&stub).is_err());
    let missing = lib.join("no such album/none.flac");
    assert!(fingerprint_file(&missing).is_err());
    assert!(track_replaygain_file(&missing).is_err());
}

/// Harness check: the test-only MD5 against digests computed with
/// macOS `md5 -s` (RFC 1321 appendix inputs).
#[test]
fn md5_helper_matches_reference_digests() {
    let cases = [
        ("", "d41d8cd98f00b204e9800998ecf8427e"),
        ("a", "0cc175b9c0f1b6a831c399e269772661"),
        ("abc", "900150983cd24fb0d6963f7d28e17f72"),
        ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
        (
            "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            "57edf4a22be3c955ac49da2e2107b67a",
        ),
    ];
    for (input, want) in cases {
        assert_eq!(md5::hex(&md5::digest(input.as_bytes())), want, "{input:?}");
        // Fed one byte at a time (crossing block boundaries) gives the same.
        let mut h = md5::Md5::new();
        for b in input.as_bytes() {
            h.update(std::slice::from_ref(b));
        }
        assert_eq!(md5::hex(&h.finish()), want, "{input:?} byte-wise");
    }
}

/// A copy of a fixture FLAC with one byte of frame data
/// flipped never decodes to PCM whose md5 passes as the recorded
/// STREAMINFO MD5: the decoder rejects the damaged frame.
#[test]
fn corrupted_frame_does_not_pass_md5() {
    let goldens = load_json(SYNTHETIC_GOLDENS);
    let lib = synthetic_library_dir();
    let rel = "Aurora Fields - First Light [FLAC]/01 - Dawn Chorus.flac";
    let want = golden_str(&goldens["files"][rel], &["stream", "md5_signature"]);
    let original = std::fs::read(lib.join(rel)).unwrap();
    let audio = musaic_dsp::decode_flac(&lib.join(rel)).unwrap();
    assert_eq!(pcm_md5_hex(&audio), want, "intact copy must match first");

    let dir = std::env::temp_dir().join(format!("musaic-dsp-md5-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("corrupt.flac");
    let mut bytes = original.clone();
    // Well past the metadata blocks (a few KB at most) and inside audio frames.
    let at = bytes.len() / 2;
    bytes[at] ^= 0x10;
    std::fs::write(&path, &bytes).unwrap();
    let result = musaic_dsp::decode_flac(&path);
    std::fs::remove_dir_all(&dir).unwrap();
    match result {
        Err(e) => println!("[decode malformed] decode error: {e}"),
        Ok(a) => {
            let got = pcm_md5_hex(&a);
            println!("[decode malformed] decoded; md5 {got} vs {want}");
            assert_ne!(got, want, "corrupted copy decoded to matching PCM");
        }
    }
}
