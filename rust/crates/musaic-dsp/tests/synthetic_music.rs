//! Loudness and fingerprints on synthetic, music-like audio built here
//! (tones, chords that change, a kick pattern). Nothing is read from disk;
//! every expected value follows from how the signal is built and the rule
//! stated next to it.

use std::f64::consts::PI;

use musaic_dsp::{
    ClusterTrack, DecodedAudio, LoudnessError, REPLAYGAIN_REFERENCE_LUFS,
    SIMILARITY_DUPLICATE_THRESHOLD, aggregate_album, chromaprint_similarity, cluster_duplicates,
    decompress_fingerprint, fingerprint_audio, format_gain, integrated_loudness, track_replaygain,
};

/// Tolerance on loudness in dB (the same 0.1 dB the library goldens use).
const TOL_DB: f64 = 0.1;

/// `10 * log10(2)`: the power ratio of two equal channels over one.
const DB_OF_TWO: f64 = 3.010_299_956_639_812;

fn sine(rate: u32, seconds: f64, freq: f64, amp: f64) -> Vec<f64> {
    let n = (seconds * f64::from(rate)) as usize;
    (0..n)
        .map(|i| amp * (2.0 * PI * freq * i as f64 / f64::from(rate)).sin())
        .collect()
}

/// Floats in [-1, 1] to 16-bit integer PCM.
fn to_i16(x: &[f64]) -> Vec<i32> {
    x.iter()
        .map(|v| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i32)
        .collect()
}

fn audio(rate: u32, channels: u32, interleaved: Vec<i32>) -> DecodedAudio {
    DecodedAudio {
        sample_rate: rate,
        channels,
        bits_per_sample: 16,
        total_frames: (interleaved.len() / channels as usize) as u64,
        samples: interleaved,
    }
}

// ---------------------------------------------------------------- loudness

/// BS.1770 is calibrated so that a 1 kHz sine of amplitude A in one channel
/// reads its mean square in dB: `20 log10(A) - 3.01` (A^2 / 2); the
/// K-weighting is ~0 dB at 1 kHz once the -0.691 offset is applied.
#[test]
fn a_sine_reads_its_mean_square_in_lufs() {
    for (freq, amp) in [(997.0, 0.5), (1000.0, 0.25), (997.0, 0.1)] {
        let x = sine(48000, 10.0, freq, amp);
        let lufs = integrated_loudness(&x, 1, 48000).unwrap();
        let want = 20.0 * amp.log10() - DB_OF_TWO;
        assert!(
            (lufs - want).abs() <= TOL_DB,
            "{freq} Hz at {amp}: {lufs} LUFS, want {want}"
        );
    }
}

/// Two identical channels carry twice the power of one (channel weights
/// 1.0 for L and R), so they read `10 log10(2)` dB louder.
#[test]
fn identical_stereo_channels_read_three_db_over_mono() {
    let x = sine(44100, 5.0, 997.0, 0.3);
    let mono = integrated_loudness(&x, 1, 44100).unwrap();
    let stereo: Vec<f64> = x.iter().flat_map(|v| [*v, *v]).collect();
    let both = integrated_loudness(&stereo, 2, 44100).unwrap();
    assert!((both - mono - DB_OF_TWO).abs() < 1e-9, "{mono} -> {both}");
}

/// The relative gate (-10 LU under the absolute-gated mean) drops the
/// blocks of a part 20 dB quieter than the rest when the two parts are
/// equally long: the absolute-gated mean is about 3 dB under the loud part,
/// the gate 13 dB under it, the quiet part 20 dB under it. What is left is
/// the loud part (the few blocks straddling the step weigh under 0.1 dB
/// over 20 s).
#[test]
fn a_much_quieter_passage_is_gated_out() {
    let loud = sine(48000, 20.0, 997.0, 0.5);
    let quiet = sine(48000, 20.0, 997.0, 0.05);
    let loud_only = integrated_loudness(&loud, 1, 48000).unwrap();
    let both: Vec<f64> = loud.iter().chain(&quiet).copied().collect();
    let gated = integrated_loudness(&both, 1, 48000).unwrap();
    assert!(
        (gated - loud_only).abs() <= TOL_DB,
        "gated {gated}, loud part alone {loud_only}"
    );
}

/// No block passes the absolute gate: -inf LUFS, and the gain against the
/// -18 LUFS reference is "+inf dB".
#[test]
fn silence_is_minus_infinity_and_an_infinite_gain() {
    let rg = track_replaygain(&audio(44100, 2, vec![0; 44100 * 2 * 2])).unwrap();
    assert_eq!(rg.lufs, f64::NEG_INFINITY);
    assert_eq!(rg.track_gain, "+inf dB");
    assert_eq!(rg.track_peak, "0.000000");
}

/// Shorter than one 0.4 s block: refused.
#[test]
fn shorter_than_one_block_is_refused() {
    let x = to_i16(&sine(44100, 0.3, 997.0, 0.5));
    assert!(matches!(
        track_replaygain(&audio(44100, 1, x)),
        Err(LoudnessError::TooShort)
    ));
}

/// The peak is the largest |sample| over 2^15: one sample of -16384 in a
/// quieter tone is 0.5 exactly. The gain is `-18 - LUFS` as "%+.2f dB";
/// the duration is frames over rate.
#[test]
fn track_values_and_tag_strings() {
    let mut x = to_i16(&sine(44100, 3.0, 997.0, 0.4));
    x[44100] = -16384;
    let rg = track_replaygain(&audio(44100, 1, x)).unwrap();
    assert_eq!(rg.peak, 0.5);
    assert_eq!(rg.track_peak, "0.500000");
    assert_eq!(rg.duration_s, 3.0);
    let want = 20.0 * 0.4f64.log10() - DB_OF_TWO;
    assert!((rg.lufs - want).abs() <= TOL_DB, "{} vs {want}", rg.lufs);
    assert_eq!(
        rg.track_gain,
        format!("{:+.2} dB", REPLAYGAIN_REFERENCE_LUFS - rg.lufs)
    );
}

/// The album value is the duration-weighted mean energy of the tracks with
/// finite loudness: two equally long tracks at L and L-10 give
/// `10 log10((10^(L/10) + 10^((L-10)/10)) / 2)` = L + 10 log10(0.55). A
/// silent (-inf) track is left out of the mean but its peak still counts
/// in the album peak (the largest of all).
#[test]
fn album_aggregate_is_the_energy_mean_of_finite_tracks() {
    let l = -12.0;
    let album = aggregate_album(&[
        (180.0, l, 0.5),
        (180.0, l - 10.0, 0.25),
        (60.0, f64::NEG_INFINITY, 0.9),
    ])
    .unwrap();
    let want = 10.0 * ((10f64.powf(l / 10.0) + 10f64.powf((l - 10.0) / 10.0)) / 2.0).log10();
    assert!((album.lufs - want).abs() < 1e-9, "{} vs {want}", album.lufs);
    assert!((want - (l + 10.0 * 0.55f64.log10())).abs() < 1e-12);
    assert_eq!(album.peak, 0.9);
    assert_eq!(album.album_peak, "0.900000");
    assert_eq!(
        album.album_gain,
        format_gain(REPLAYGAIN_REFERENCE_LUFS, want)
    );
    // Only silent tracks: no album value.
    assert!(aggregate_album(&[(60.0, f64::NEG_INFINITY, 0.0)]).is_none());
}

// ------------------------------------------------------------ fingerprints

/// Equal-tempered frequency of a MIDI note.
fn note(midi: f64) -> f64 {
    440.0 * 2f64.powf((midi - 69.0) / 12.0)
}

/// A piece: chords (MIDI notes) changing every `chord_s` seconds, with
/// three harmonics per note, and a kick on every beat at `bpm`.
struct Piece {
    chords: &'static [[f64; 3]],
    chord_s: f64,
    bpm: f64,
}

impl Piece {
    fn sample(&self, t: f64) -> f64 {
        let chord = &self.chords[(t / self.chord_s) as usize % self.chords.len()];
        let in_chord = t % self.chord_s;
        // Each chord swells in over 50 ms so a change is an onset, not a click.
        let swell = (in_chord / 0.05).min(1.0);
        let mut v = 0.0;
        for &m in chord {
            let f = note(m);
            v += 0.08 * (2.0 * PI * f * t).sin()
                + 0.04 * (2.0 * PI * 2.0 * f * t).sin()
                + 0.02 * (2.0 * PI * 3.0 * f * t).sin();
        }
        let beat = 60.0 / self.bpm;
        let since = t % beat;
        let kick = (-since / 0.03).exp()
            * ((2.0 * PI * 55.0 * since).sin() + 0.5 * (2.0 * PI * 110.0 * since).sin());
        swell * v + 0.3 * kick
    }

    /// `seconds` of mono 16-bit audio at `rate`, scaled by `gain`.
    fn render(&self, rate: u32, seconds: f64, gain: f64) -> DecodedAudio {
        let n = (seconds * f64::from(rate)) as usize;
        let x: Vec<f64> = (0..n)
            .map(|i| gain * self.sample(i as f64 / f64::from(rate)))
            .collect();
        audio(rate, 1, to_i16(&x))
    }
}

/// C - Am - F - G in the middle register, 2 s each, 120 BPM.
const FIRST: Piece = Piece {
    chords: &[
        [60.0, 64.0, 67.0],
        [57.0, 60.0, 64.0],
        [53.0, 57.0, 60.0],
        [55.0, 59.0, 62.0],
    ],
    chord_s: 2.0,
    bpm: 120.0,
};

/// Another piece: E - C#m - A - D#dim, 1.5 s each, 96 BPM.
const SECOND: Piece = Piece {
    chords: &[
        [64.0, 68.0, 71.0],
        [61.0, 64.0, 68.0],
        [57.0, 61.0, 64.0],
        [63.0, 66.0, 69.0],
    ],
    chord_s: 1.5,
    bpm: 96.0,
};

/// 25 s: a whole number of seconds at 44.1 kHz, so the duration is exact.
const SECONDS: f64 = 25.0;

#[test]
fn a_fingerprint_is_deterministic_and_round_trips() {
    let a = FIRST.render(44100, SECONDS, 1.0);
    let fp = fingerprint_audio(&a).unwrap();
    // Same input, same fingerprint.
    assert_eq!(fingerprint_audio(&a).unwrap(), fp);
    // Duration: whole seconds, frames over rate, truncated.
    assert_eq!(fp.duration_s, 25);
    let odd = FIRST.render(44100, 25.9, 1.0);
    assert_eq!(fingerprint_audio(&odd).unwrap().duration_s, 25);
    // The stored (compressed) form decodes to the raw sub-fingerprints,
    // with the TEST2 algorithm id 1.
    let back = decompress_fingerprint(&fp.compressed).unwrap();
    assert_eq!(back.algorithm, 1);
    assert_eq!(back.raw, fp.raw);
    assert!(!fp.raw.is_empty());
}

/// The same music 1 dB quieter is the same recording to the fingerprint
/// (chroma features do not depend on level), so it reaches the duplicate
/// threshold and clusters with the original; another piece (other chords,
/// other tempo) of the same length does not.
#[test]
fn a_quieter_copy_is_a_duplicate_and_another_piece_is_not() {
    let gain = 10f64.powf(-1.0 / 20.0);
    let original = fingerprint_audio(&FIRST.render(44100, SECONDS, 1.0)).unwrap();
    let quieter = fingerprint_audio(&FIRST.render(44100, SECONDS, gain)).unwrap();
    let other = fingerprint_audio(&SECOND.render(44100, SECONDS, 1.0)).unwrap();

    let same = chromaprint_similarity(&original.compressed, &quieter.compressed);
    assert!(
        same >= SIMILARITY_DUPLICATE_THRESHOLD,
        "a 1 dB quieter copy: similarity {same}"
    );
    let different = chromaprint_similarity(&original.compressed, &other.compressed);
    assert!(
        different < SIMILARITY_DUPLICATE_THRESHOLD,
        "another piece: similarity {different}"
    );

    let tracks = [
        ClusterTrack {
            filename: "01 First.flac",
            duration_s: original.duration_s as f64,
            fingerprint: &original.compressed,
        },
        ClusterTrack {
            filename: "02 Second.flac",
            duration_s: other.duration_s as f64,
            fingerprint: &other.compressed,
        },
        ClusterTrack {
            filename: "03 First (quieter copy).flac",
            duration_s: quieter.duration_s as f64,
            fingerprint: &quieter.compressed,
        },
    ];
    assert_eq!(cluster_duplicates(&tracks), vec![vec![0, 2]]);
}
