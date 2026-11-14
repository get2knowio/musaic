//! DSP layer of the Rust build of Musaic.
//!
//! * [`fingerprint`]: Chromaprint fingerprints of FLAC files, the
//!   compressed base64 form and its decompression, and the Python app's
//!   `chromaprint_similarity`.
//! * [`duplicates`]: the duplicate-tracks clustering rule.
//! * [`loudness`]: BS.1770 integrated loudness as pyloudnorm 0.2.0 computes it,
//!   sample peak, and the ReplayGain track/album values and tag strings.
//! * [`decode`]: FLAC decoding shared by the above.
//!
//! BPM is in the musaic-bpm crate.

pub mod decode;
pub mod duplicates;
pub mod fingerprint;
pub mod loudness;

pub use decode::{DecodeError, DecodedAudio, decode_flac};
pub use duplicates::{
    ClusterTrack, MAX_DURATION_DELTA_SEC, SIMILARITY_DUPLICATE_THRESHOLD, cluster_duplicates,
};
pub use fingerprint::{
    FINGERPRINT_IMPL_FPCALC, FINGERPRINT_IMPL_RUST, FileFingerprint, FingerprintError,
    MAX_FINGERPRINT_SECONDS, chromaprint_similarity, compress_fingerprint, decompress_fingerprint,
    fingerprint_audio, fingerprint_file, raw_similarity,
};
pub use loudness::{
    AlbumReplayGain, LoudnessError, REPLAYGAIN_REFERENCE_LUFS, TrackReplayGain, aggregate_album,
    format_gain, format_peak, integrated_loudness, reference_loudness_tag, sample_peak,
    track_replaygain, track_replaygain_file,
};
