//! FLAC decoding (claxon, pure Rust) into interleaved integer PCM.

use std::path::{Path, PathBuf};

/// Errors from opening or decoding a FLAC file.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("cannot decode FLAC {path}: {source}")]
    Flac {
        path: PathBuf,
        #[source]
        source: claxon::Error,
    },
    #[error("unsupported FLAC stream in {path}: {reason}")]
    Unsupported { path: PathBuf, reason: String },
}

/// Interleaved integer PCM plus the stream parameters needed downstream.
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    pub channels: u32,
    pub bits_per_sample: u32,
    /// Length of the whole stream in frames (samples per channel): the
    /// STREAMINFO total when the encoder recorded one, otherwise the number
    /// of frames actually decoded. This is what mutagen reports as
    /// `info.total_samples` and divides by the rate for `info.length`.
    pub total_frames: u64,
    /// Interleaved samples, `channels` per frame, at their native bit depth.
    /// May hold fewer than `total_frames` frames when decoding was limited.
    pub samples: Vec<i32>,
}

impl DecodedAudio {
    /// Number of frames held in `samples`.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }

    /// Stream length in seconds as mutagen computes `FLAC.info.length`.
    pub fn length_seconds(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.total_frames as f64 / f64::from(self.sample_rate)
        }
    }
}

/// Decode a whole FLAC file.
pub fn decode_flac(path: &Path) -> Result<DecodedAudio, DecodeError> {
    decode_flac_limited(path, None)
}

/// Decode at most `max_frames` frames (all of them for `None`). When the
/// STREAMINFO block carries no total length and a limit is set, the whole
/// stream is still walked so that `total_frames` is right.
pub fn decode_flac_limited(
    path: &Path,
    max_frames: Option<u64>,
) -> Result<DecodedAudio, DecodeError> {
    let flac_err = |source| DecodeError::Flac {
        path: path.to_path_buf(),
        source,
    };
    let mut reader = claxon::FlacReader::open(path).map_err(flac_err)?;
    let info = reader.streaminfo();
    if info.channels == 0 || info.sample_rate == 0 {
        return Err(DecodeError::Unsupported {
            path: path.to_path_buf(),
            reason: format!("{} channels at {} Hz", info.channels, info.sample_rate),
        });
    }
    let channels = info.channels as usize;
    let known_total = info.samples;
    let keep = match (max_frames, known_total) {
        (Some(m), Some(t)) => m.min(t),
        (Some(m), None) => m,
        (None, Some(t)) => t,
        (None, None) => u64::MAX,
    };
    let walk_all = known_total.is_none() || max_frames.is_none();
    // Pre-size only from a recorded length, capped at 2^30 frames.
    let capacity = match known_total {
        Some(_) => usize::try_from(keep.min(1 << 30)).unwrap_or(0) * channels,
        None => 0,
    };
    let mut samples: Vec<i32> = Vec::with_capacity(capacity);
    let mut decoded_frames: u64 = 0;

    let mut blocks = reader.blocks();
    let mut buffer = Vec::new();
    loop {
        if !walk_all && decoded_frames >= keep {
            break;
        }
        let block = match blocks.read_next_or_eof(buffer) {
            Ok(Some(block)) => block,
            Ok(None) => break,
            Err(e) => return Err(flac_err(e)),
        };
        let len = u64::from(block.duration());
        let take = keep.saturating_sub(decoded_frames).min(len) as usize;
        for i in 0..take {
            for ch in 0..channels {
                samples.push(block.sample(ch as u32, i as u32));
            }
        }
        decoded_frames += len;
        buffer = block.into_buffer();
    }

    Ok(DecodedAudio {
        sample_rate: info.sample_rate,
        channels: info.channels,
        bits_per_sample: info.bits_per_sample,
        total_frames: known_total.unwrap_or(decoded_frames),
        samples,
    })
}
