//! Shared harness for the golden tests: loads the Python app's DSP goldens,
//! measures every fixture FLAC once (in parallel) and checks the results
//! against them.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use musaic_dsp::{
    ClusterTrack, DecodedAudio, FileFingerprint, TrackReplayGain, aggregate_album,
    chromaprint_similarity, cluster_duplicates, compress_fingerprint, decode_flac,
    decompress_fingerprint, fingerprint_audio, raw_similarity, track_replaygain,
};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

/// Public: the DSP goldens of the synthetic library (rust/testdata).
pub const SYNTHETIC_GOLDENS: &str = "captures/dsp-synthetic/dsp/goldens.json";
/// Private: the DSP goldens of the real-audio set.
pub const REAL_GOLDENS: &str = "captures/dsp-real/dsp/goldens.json";
/// Private: the local copy of the real-audio set (never committed).
pub const REAL_LIBRARY: &str = "real-library/library";
pub const SYNTHETIC_MANIFEST: &str = "synthetic-library/library.manifest.json";

/// Similarity floor against the fpcalc fingerprint.
pub const MIN_SIMILARITY_TO_FPCALC: f64 = 0.95;
/// Tolerance on track (and album) gain / LUFS.
pub const GAIN_TOLERANCE_DB: f64 = 0.1;

/// `dev <= 0.1 dB`; false for NaN, so a NaN deviation fails.
pub fn within_tolerance(dev: f64) -> bool {
    dev <= GAIN_TOLERANCE_DB
}

/// A public JSON file (rust/testdata).
pub fn load_json(rel: &str) -> Value {
    musaic_testdata::public_json(rel)
}

/// A private JSON file; the calling test is gated on it (Tier 2).
pub fn load_private_json(rel: &str) -> Value {
    musaic_testdata::read_json(musaic_testdata::private_required(rel))
}

/// The generated synthetic library; panics with the generator command when
/// it is missing.
pub fn synthetic_library_dir() -> PathBuf {
    musaic_testdata::synthetic_library()
}

/// The local copy of the real-audio set (private; the calling test is gated
/// on it).
pub fn real_library_dir() -> PathBuf {
    musaic_testdata::private_required(REAL_LIBRARY)
}

pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// fpcalc `-raw` integers; the goldens may store them as signed ints.
pub fn golden_raw(file: &Value) -> Vec<u32> {
    file["fingerprint"]["raw"]
        .as_array()
        .expect("fingerprint.raw")
        .iter()
        .map(|v| match v.as_u64() {
            Some(u) => u32::try_from(u).expect("raw value fits u32"),
            None => v.as_i64().expect("raw integer") as i32 as u32,
        })
        .collect()
}

pub fn golden_str<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_str()
        .unwrap_or_else(|| panic!("golden {path:?} is not a string: {cur}"))
}

pub fn golden_f64(v: &Value, path: &[&str]) -> f64 {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_f64()
        .unwrap_or_else(|| panic!("golden {path:?} is not a number: {cur}"))
}

/// Parse "+0.23 dB" / "-2.43 dB".
pub fn parse_gain(s: &str) -> f64 {
    s.strip_suffix(" dB")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("bad gain string {s:?}"))
}

/// Everything the Rust build computes for one file.
pub struct Measured {
    pub rel: String,
    pub fingerprint: Result<FileFingerprint, String>,
    pub replaygain: Result<TrackReplayGain, String>,
    /// Md5 of the decoded interleaved PCM in FLAC's byte layout,
    /// and of its float64 form as soundfile returns it.
    pub pcm_md5: Result<(String, String), String>,
}

/// Decode each golden file once and compute fingerprint + ReplayGain.
pub fn measure_library(library: &Path, goldens: &Value) -> Vec<Measured> {
    let rels: Vec<String> = goldens["files"]
        .as_object()
        .expect("goldens.files")
        .keys()
        .cloned()
        .collect();
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Measured>> = Mutex::new(Vec::with_capacity(rels.len()));
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(rel) = rels.get(i) else { break };
                    let path = library.join(rel);
                    let m = match decode_flac(&path) {
                        Ok(audio) => Measured {
                            rel: rel.clone(),
                            fingerprint: fingerprint_audio(&audio).map_err(|e| e.to_string()),
                            replaygain: track_replaygain(&audio).map_err(|e| e.to_string()),
                            pcm_md5: Ok((pcm_md5_hex(&audio), pcm_float64_md5_hex(&audio))),
                        },
                        Err(e) => Measured {
                            rel: rel.clone(),
                            fingerprint: Err(e.to_string()),
                            replaygain: Err(e.to_string()),
                            pcm_md5: Err(e.to_string()),
                        },
                    };
                    out.lock().unwrap().push(m);
                }
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by(|a, b| a.rel.cmp(&b.rel));
    v
}

/// The compressed base64 form round-trips exactly for every golden
/// (compress(raw) == fpcalc string and decompress(fpcalc string) == raw).
pub fn check_compression_round_trip(goldens: &Value) -> usize {
    let mut n = 0;
    for (rel, f) in goldens["files"].as_object().unwrap() {
        assert_eq!(golden_str(f, &["fingerprint", "status"]), "ok", "{rel}");
        let raw = golden_raw(f);
        let s = golden_str(f, &["fingerprint", "fingerprint"]);
        assert_eq!(
            compress_fingerprint(&raw),
            s,
            "compress(raw) != fpcalc string: {rel}"
        );
        let d = decompress_fingerprint(s).unwrap_or_else(|e| panic!("{rel}: {e}"));
        assert_eq!(d.algorithm, 1, "{rel}: algorithm id (TEST2)");
        assert_eq!(d.raw, raw, "decompress(fpcalc string) != raw: {rel}");
        n += 1;
    }
    n
}

/// Fingerprint gate on every file: same length as fpcalc, similarity >= 0.95,
/// same integer duration, and the Rust compressed form round-trips.
/// Returns (worst similarity, file).
pub fn check_fingerprints(label: &str, goldens: &Value, measured: &[Measured]) -> (f64, String) {
    let files = goldens["files"].as_object().unwrap();
    assert_eq!(measured.len(), files.len());
    let mut worst = (f64::INFINITY, String::new());
    let mut failures = Vec::new();
    let mut exact = 0;
    for m in measured {
        let g = &files[&m.rel];
        let fp = match &m.fingerprint {
            Ok(fp) => fp,
            Err(e) => {
                failures.push(format!("{}: fingerprint failed: {e}", m.rel));
                continue;
            }
        };
        let gold = golden_raw(g);
        let gold_duration = golden_f64(g, &["fingerprint", "duration"]);
        let sim = raw_similarity(&fp.raw, &gold);
        let sim_str = chromaprint_similarity(
            &fp.compressed,
            golden_str(g, &["fingerprint", "fingerprint"]),
        );
        if sim < worst.0 {
            worst = (sim, m.rel.clone());
        }
        exact += usize::from(fp.raw == gold);
        if fp.raw.len() != gold.len() {
            failures.push(format!(
                "{}: length {} != fpcalc {}",
                m.rel,
                fp.raw.len(),
                gold.len()
            ));
        }
        if sim < MIN_SIMILARITY_TO_FPCALC {
            failures.push(format!(
                "{}: similarity {sim:.4} < {MIN_SIMILARITY_TO_FPCALC}",
                m.rel
            ));
        }
        if (sim - sim_str).abs() > 0.0 {
            failures.push(format!(
                "{}: string similarity {sim_str} != raw {sim}",
                m.rel
            ));
        }
        if fp.duration_s as f64 != gold_duration {
            failures.push(format!(
                "{}: duration {} != fpcalc {gold_duration}",
                m.rel, fp.duration_s
            ));
        }
        match decompress_fingerprint(&fp.compressed) {
            Ok(d) if d.raw == fp.raw => {}
            other => failures.push(format!(
                "{}: own compressed form does not round-trip: {other:?}",
                m.rel
            )),
        }
    }
    println!(
        "[fingerprint {label}] {} files, {exact} bit-exact with fpcalc, worst similarity {:.6} ({}), {} failures",
        measured.len(),
        worst.0,
        worst.1,
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "fingerprint {label} gate failed:\n{}",
        failures.join("\n")
    );
    worst
}

#[derive(Default, Debug)]
pub struct RgReport {
    pub max_lufs_dev: f64,
    pub max_gain_dev: f64,
    pub gain_string_mismatches: usize,
    pub max_peak_dev: f64,
    pub peak_string_mismatches: usize,
    pub worst_file: String,
}

/// Per track, LUFS and gain within 0.1 dB of the Python app, peak
/// string exact to six decimals, reference string "-18.00 LUFS".
pub fn check_track_replaygain(label: &str, goldens: &Value, measured: &[Measured]) -> RgReport {
    let files = goldens["files"].as_object().unwrap();
    let mut r = RgReport::default();
    let mut failures = Vec::new();
    assert_eq!(musaic_dsp::reference_loudness_tag(), "-18.00 LUFS");
    for m in measured {
        let g = &files[&m.rel]["replaygain"];
        assert_eq!(golden_str(g, &["status"]), "ok", "{}", m.rel);
        assert_eq!(
            golden_f64(g, &["reference_lufs"]),
            musaic_dsp::REPLAYGAIN_REFERENCE_LUFS
        );
        let rg = match &m.replaygain {
            Ok(rg) => rg,
            Err(e) => {
                failures.push(format!("{}: replaygain failed: {e}", m.rel));
                continue;
            }
        };
        let lufs_dev = (rg.lufs - golden_f64(g, &["lufs"])).abs();
        let gain_dev =
            (parse_gain(&rg.track_gain) - parse_gain(golden_str(g, &["track_gain"]))).abs();
        let peak_dev = (rg.peak - golden_f64(g, &["peak"])).abs();
        if lufs_dev > r.max_lufs_dev {
            r.max_lufs_dev = lufs_dev;
            r.worst_file = m.rel.clone();
        }
        r.max_gain_dev = r.max_gain_dev.max(gain_dev);
        r.max_peak_dev = r.max_peak_dev.max(peak_dev);
        if rg.track_gain != golden_str(g, &["track_gain"]) {
            r.gain_string_mismatches += 1;
        }
        if rg.track_peak != golden_str(g, &["track_peak"]) {
            r.peak_string_mismatches += 1;
            failures.push(format!(
                "{}: peak {} != {}",
                m.rel,
                rg.track_peak,
                golden_str(g, &["track_peak"])
            ));
        }
        if !within_tolerance(lufs_dev) || !within_tolerance(gain_dev) {
            failures.push(format!(
                "{}: lufs dev {lufs_dev}, gain dev {gain_dev}",
                m.rel
            ));
        }
    }
    println!(
        "[loudness {label}] {} tracks: max |LUFS dev| {:.3e} dB ({}), max |gain dev| {:.3e} dB, \
         gain strings differing {}, max |peak dev| {:.3e}, peak strings differing {}",
        measured.len(),
        r.max_lufs_dev,
        r.worst_file,
        r.max_gain_dev,
        r.gain_string_mismatches,
        r.max_peak_dev,
        r.peak_string_mismatches
    );
    assert!(
        failures.is_empty(),
        "loudness {label} failed:\n{}",
        failures.join("\n")
    );
    r
}

/// Album aggregate from the Rust per-track values against the
/// Python app's album goldens; also feeds the Python app's own per-track values
/// through the ported aggregate to pin the formula independently of the
/// loudness meter.
pub fn check_album_replaygain(label: &str, goldens: &Value, measured: &[Measured]) -> (f64, f64) {
    let files = goldens["files"].as_object().unwrap();
    let mut by_album: BTreeMap<String, Vec<&Measured>> = BTreeMap::new();
    for m in measured {
        let album = golden_str(&files[&m.rel], &["album"]).to_string();
        by_album.entry(album).or_default().push(m);
    }
    let albums = goldens["albums"].as_object().unwrap();
    assert_eq!(by_album.len(), albums.len());
    let (mut max_dev, mut max_formula_dev) = (0.0f64, 0.0f64);
    let mut failures = Vec::new();
    for (album, tracks) in &by_album {
        let g = &albums[album];
        assert_eq!(g["complete"], Value::Bool(true), "{album}");
        let grg = &g["replaygain"];
        assert_eq!(golden_str(grg, &["status"]), "ok", "{album}");
        // Rust measurements, in the Python app's sorted-filename order.
        let ours: Vec<(f64, f64, f64)> = tracks
            .iter()
            .filter_map(|m| m.replaygain.as_ref().ok())
            .map(|rg| (rg.duration_s, rg.lufs, rg.peak))
            .collect();
        // The Python app's per-track values (duration = mutagen stream length).
        let python: Vec<(f64, f64, f64)> = tracks
            .iter()
            .map(|m| {
                let f = &files[&m.rel];
                (
                    golden_f64(f, &["stream", "length"]),
                    golden_f64(f, &["replaygain", "lufs"]),
                    golden_f64(f, &["replaygain", "peak"]),
                )
            })
            .collect();
        let gold_lufs = golden_f64(grg, &["lufs"]);
        let Some(a) = aggregate_album(&ours) else {
            failures.push(format!("{album}: no aggregate"));
            continue;
        };
        let f = aggregate_album(&python).expect("python aggregate");
        let dev = (a.lufs - gold_lufs).abs();
        let gain_dev =
            (parse_gain(&a.album_gain) - parse_gain(golden_str(grg, &["album_gain"]))).abs();
        max_dev = max_dev.max(dev).max(gain_dev);
        max_formula_dev = max_formula_dev.max((f.lufs - gold_lufs).abs());
        if !within_tolerance(dev) || !within_tolerance(gain_dev) {
            failures.push(format!("{album}: lufs dev {dev}, gain dev {gain_dev}"));
        }
        if a.album_peak != golden_str(grg, &["album_peak"]) || a.peak != golden_f64(grg, &["peak"])
        {
            failures.push(format!(
                "{album}: peak {} != {}",
                a.album_peak,
                golden_str(grg, &["album_peak"])
            ));
        }
        if f.album_gain != golden_str(grg, &["album_gain"]) {
            failures.push(format!(
                "{album}: formula on the Python app's tracks gives {} != {}",
                f.album_gain,
                golden_str(grg, &["album_gain"])
            ));
        }
    }
    println!(
        "[album gain {label}] {} albums: max |album LUFS/gain dev| {max_dev:.3e} dB (Rust tracks), \
         {max_formula_dev:.3e} dB (the Python app's tracks through the Rust formula)",
        by_album.len()
    );
    assert!(
        failures.is_empty(),
        "album gain {label} failed:\n{}",
        failures.join("\n")
    );
    (max_dev, max_formula_dev)
}

/// Run the duplicate-tracks rule per album over (filename, duration,
/// fingerprint) triples; returns album -> clusters of filenames.
pub fn clusters_by_album(
    goldens: &Value,
    track_of: impl Fn(&str) -> (f64, String),
) -> BTreeMap<String, BTreeSet<Vec<String>>> {
    let files = goldens["files"].as_object().unwrap();
    let mut by_album: BTreeMap<String, Vec<(String, f64, String)>> = BTreeMap::new();
    for (rel, f) in files {
        let album = golden_str(f, &["album"]).to_string();
        let filename = rel.rsplit('/').next().unwrap().to_string();
        let (d, fp) = track_of(rel);
        by_album.entry(album).or_default().push((filename, d, fp));
    }
    let mut out = BTreeMap::new();
    for (album, mut rows) in by_album {
        rows.sort_by(|a, b| a.0.cmp(&b.0)); // ORDER BY filename
        let tracks: Vec<ClusterTrack<'_>> = rows
            .iter()
            .map(|(f, d, fp)| ClusterTrack {
                filename: f,
                duration_s: *d,
                fingerprint: fp,
            })
            .collect();
        let clusters: BTreeSet<Vec<String>> = cluster_duplicates(&tracks)
            .into_iter()
            .map(|c| c.into_iter().map(|i| rows[i].0.clone()).collect())
            .collect();
        out.insert(album, clusters);
    }
    out
}

/// Clusters from the Python app's fpcalc fingerprints and durations.
pub fn golden_clusters(goldens: &Value) -> BTreeMap<String, BTreeSet<Vec<String>>> {
    let files = goldens["files"].as_object().unwrap();
    clusters_by_album(goldens, |rel| {
        let f = &files[rel];
        (
            golden_f64(f, &["fingerprint", "duration"]),
            golden_str(f, &["fingerprint", "fingerprint"]).to_string(),
        )
    })
}

/// Clusters from fingerprints and durations the Rust build computed (never
/// mixed with fpcalc ones).
pub fn computed_clusters(
    goldens: &Value,
    measured: &[Measured],
) -> BTreeMap<String, BTreeSet<Vec<String>>> {
    let by_rel: BTreeMap<&str, &FileFingerprint> = measured
        .iter()
        .map(|m| (m.rel.as_str(), m.fingerprint.as_ref().expect("fingerprint")))
        .collect();
    clusters_by_album(goldens, |rel| {
        let fp = by_rel[rel];
        (fp.duration_s as f64, fp.compressed.clone())
    })
}

/// Expected clusters from the committed fixture manifest's `dup_of` links.
pub fn manifest_clusters(manifest_rel: &str) -> BTreeMap<String, BTreeSet<Vec<String>>> {
    let manifest = load_json(manifest_rel);
    let mut out = BTreeMap::new();
    for album in manifest["albums"].as_array().expect("manifest.albums") {
        let mut groups: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for t in album["tracks"].as_array().expect("tracks") {
            if let Some(of) = t["dup_of"].as_str() {
                let g = groups.entry(of.to_string()).or_default();
                g.insert(of.to_string());
                g.insert(t["filename"].as_str().unwrap().to_string());
            }
        }
        let clusters = groups
            .into_values()
            .map(|g| g.into_iter().collect())
            .collect();
        out.insert(nfc(album["dir_name"].as_str().unwrap()), clusters);
    }
    out
}

pub fn nfc_keys<V>(m: BTreeMap<String, V>) -> BTreeMap<String, V> {
    m.into_iter().map(|(k, v)| (nfc(&k), v)).collect()
}

/// Md5 of decoded PCM laid out the way the FLAC format defines the
/// STREAMINFO MD5 (interleaved, little-endian, signed, ceil(bps/8) bytes per
/// sample).
pub fn pcm_md5_hex(audio: &DecodedAudio) -> String {
    let width = audio.bits_per_sample.div_ceil(8) as usize;
    let mut h = md5::Md5::new();
    for s in &audio.samples {
        h.update(&s.to_le_bytes()[..width]);
    }
    md5::hex(&h.finish())
}

/// Md5 of the float64 array the Python app hands pyloudnorm
/// (`soundfile.read(path)`: libsndfile scales integer PCM by 1/2^(bps-1)),
/// little-endian, interleaved.
pub fn pcm_float64_md5_hex(audio: &DecodedAudio) -> String {
    let scale = f64::from(1u32 << (audio.bits_per_sample - 1));
    let mut h = md5::Md5::new();
    for s in &audio.samples {
        h.update(&(f64::from(*s) / scale).to_le_bytes());
    }
    md5::hex(&h.finish())
}

/// Public half: the synthetic library; private half: `libraries.real`.
pub const PCM_MD5_GOLDEN: &str = "dsp/pcm-md5.json";

/// Every file's decoded-PCM md5 equals the STREAMINFO MD5 the
/// Python app recorded (`stream.md5_signature`) and the md5 of the integer PCM
/// the Python app's decoder (soundfile) yields; the float64 form equals the
/// array the Python app measures loudness on. `library` names the section of
/// testdata/golden/dsp/pcm-md5.json. Returns the number of files compared.
pub fn check_pcm_md5(library: &str, goldens: &Value, measured: &[Measured]) -> usize {
    let files = goldens["files"].as_object().unwrap();
    let pcm = musaic_testdata::golden(PCM_MD5_GOLDEN);
    let pcm = pcm["libraries"][library]["files"]
        .as_object()
        .unwrap_or_else(|| panic!("no {library} section in {PCM_MD5_GOLDEN}"));
    assert_eq!(measured.len(), files.len());
    assert_eq!(
        pcm.len(),
        files.len(),
        "{PCM_MD5_GOLDEN} {library} file count"
    );
    let mut failures = Vec::new();
    for m in measured {
        let want = golden_str(&files[&m.rel], &["stream", "md5_signature"]);
        assert_ne!(
            want,
            format!("{:032x}", 0),
            "{}: no STREAMINFO MD5 recorded",
            m.rel
        );
        let p = &pcm[&m.rel];
        let want_int = golden_str(p, &["pcm_int_md5"]);
        let want_f64 = golden_str(p, &["pcm_float64_md5"]);
        match &m.pcm_md5 {
            Ok((int, f64s)) => {
                if int != want {
                    failures.push(format!("{}: pcm md5 {int} != md5_signature {want}", m.rel));
                }
                if int != want_int {
                    failures.push(format!("{}: pcm md5 {int} != soundfile {want_int}", m.rel));
                }
                if f64s != want_f64 {
                    failures.push(format!(
                        "{}: float64 md5 {f64s} != soundfile {want_f64}",
                        m.rel
                    ));
                }
            }
            Err(e) => failures.push(format!("{}: decode failed: {e}", m.rel)),
        }
    }
    assert!(failures.is_empty(), "[decode {library}] {failures:#?}");
    println!(
        "[decode {library}] {} files: decoded PCM md5 == md5_signature == soundfile's; float64 form == soundfile's",
        measured.len()
    );
    measured.len()
}

/// RFC 1321 MD5, streaming (the workspace carries no md5 crate; test-only).
pub mod md5 {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];

    pub struct Md5 {
        state: [u32; 4],
        k: [u32; 64],
        buf: Vec<u8>,
        len: u64,
    }

    impl Default for Md5 {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Md5 {
        pub fn new() -> Self {
            // K[i] = floor(abs(sin(i + 1)) * 2^32), as RFC 1321 defines it.
            let mut k = [0u32; 64];
            for (i, v) in k.iter_mut().enumerate() {
                *v = (((i + 1) as f64).sin().abs() * 4_294_967_296.0) as u32;
            }
            Md5 {
                state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
                k,
                buf: Vec::with_capacity(64),
                len: 0,
            }
        }

        pub fn update(&mut self, mut data: &[u8]) {
            self.len = self.len.wrapping_add(data.len() as u64);
            if !self.buf.is_empty() {
                let take = (64 - self.buf.len()).min(data.len());
                self.buf.extend_from_slice(&data[..take]);
                data = &data[take..];
                if self.buf.len() < 64 {
                    return;
                }
                let block: [u8; 64] = self.buf[..].try_into().unwrap();
                self.block(&block);
                self.buf.clear();
            }
            let (blocks, rest) = data.as_chunks::<64>();
            for c in blocks {
                self.block(c);
            }
            self.buf.extend_from_slice(rest);
        }

        pub fn finish(mut self) -> [u8; 16] {
            let bit_len = self.len.wrapping_mul(8);
            let mut tail = vec![0x80u8];
            while (self.len as usize + tail.len()) % 64 != 56 {
                tail.push(0);
            }
            tail.extend_from_slice(&bit_len.to_le_bytes());
            let len = self.len;
            self.update(&tail);
            debug_assert!(self.buf.is_empty(), "{len}");
            let mut out = [0u8; 16];
            for (i, w) in self.state.iter().enumerate() {
                out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
            out
        }

        fn block(&mut self, chunk: &[u8; 64]) {
            let mut m = [0u32; 16];
            for (w, b) in m.iter_mut().zip(chunk.as_chunks::<4>().0) {
                *w = u32::from_le_bytes(*b);
            }
            let [mut a, mut b, mut c, mut d] = self.state;
            for (i, (&s, &k)) in S.iter().zip(&self.k).enumerate() {
                let (f, g) = match i / 16 {
                    0 => ((b & c) | (!b & d), i),
                    1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                    2 => (b ^ c ^ d, (3 * i + 5) % 16),
                    _ => (c ^ (b | !d), (7 * i) % 16),
                };
                let f = f.wrapping_add(a).wrapping_add(k).wrapping_add(m[g]);
                a = d;
                d = c;
                c = b;
                b = b.wrapping_add(f.rotate_left(s));
            }
            for (s, v) in self.state.iter_mut().zip([a, b, c, d]) {
                *s = s.wrapping_add(v);
            }
        }
    }

    pub fn digest(data: &[u8]) -> [u8; 16] {
        let mut h = Md5::new();
        h.update(data);
        h.finish()
    }

    pub fn hex(d: &[u8; 16]) -> String {
        d.iter().map(|b| format!("{b:02x}")).collect()
    }
}
