//! Minimal FLAC metadata reader: STREAMINFO, Vorbis comments, picture count.
//!
//! Hand-rolled so that tag order and key casing are exactly what is in the
//! file (the Python app reads "the first value" of a key, and mutagen folds key
//! case while keeping file order). Only metadata blocks are read; audio
//! frames are never touched.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    pub total_samples: u64,
    pub md5: [u8; 16],
}

impl StreamInfo {
    /// Seconds, as mutagen computes `info.length`.
    pub fn length(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.total_samples as f64 / self.sample_rate as f64
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlacMeta {
    pub info: StreamInfo,
    /// `None` when the file has no VORBIS_COMMENT block. Keys are lower-cased;
    /// entries keep file order.
    pub tags: Option<Vec<(String, String)>>,
    pub picture_count: usize,
}

impl FlacMeta {
    /// Every value stored under `key` (case-insensitive), in file order.
    pub fn values<'a>(&'a self, key: &str) -> impl Iterator<Item = &'a str> + 'a {
        let key = key.to_lowercase();
        self.tags
            .iter()
            .flatten()
            .filter(move |(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Disc number of a file from its `DISCNUMBER` values ("2", "2/3"), else
/// from a disc-prefixed `TRACKNUMBER` ("2-05", "2.05"), as `parse_tracknum`
/// reads those. One definition for the track-gaps check and the tag writer.
pub fn file_disc<'a>(
    discnumber: impl Iterator<Item = &'a str>,
    tracknumber: impl Iterator<Item = &'a str>,
) -> Option<i64> {
    use musaic_core::pycompat::{int, strip};
    let from_tag = discnumber
        .map(strip)
        .find(|v| !v.is_empty())
        .and_then(|v| int(v.split('/').next().unwrap_or("")));
    if from_tag.is_some() {
        return from_tag;
    }
    let raw = tracknumber.map(strip).find(|v| !v.is_empty())?;
    let head = strip(raw.split('/').next().unwrap_or(""));
    ['-', '.'].into_iter().find_map(|sep| {
        let (disc, track) = head.rsplit_once(sep)?;
        let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
        let (disc, track) = (strip(disc), strip(track));
        (digits(disc) && digits(track)).then(|| int(disc)).flatten()
    })
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub fn read(path: &Path) -> io::Result<FlacMeta> {
    let mut r = BufReader::new(File::open(path)?);
    let mut head = [0u8; 4];
    r.read_exact(&mut head)?;
    // An ID3v2 tag may precede the stream marker; skip it like mutagen does.
    if &head[..3] == b"ID3" {
        let mut rest = [0u8; 6];
        r.read_exact(&mut rest)?;
        let size = rest[2..]
            .iter()
            .fold(0u64, |acc, b| (acc << 7) | (*b as u64 & 0x7f));
        r.seek(SeekFrom::Start(10 + size))?;
        r.read_exact(&mut head)?;
    }
    if &head != b"fLaC" {
        return Err(invalid("not a FLAC stream"));
    }
    let (mut info, mut tags, mut picture_count) = (None, None, 0usize);
    loop {
        let mut h = [0u8; 4];
        r.read_exact(&mut h)?;
        let last = h[0] & 0x80 != 0;
        let kind = h[0] & 0x7f;
        let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
        match kind {
            0 => {
                let mut b = vec![0u8; len];
                r.read_exact(&mut b)?;
                if len < 34 {
                    return Err(invalid("short STREAMINFO"));
                }
                let packed = u64::from_be_bytes(b[10..18].try_into().expect("8 bytes"));
                let mut md5 = [0u8; 16];
                md5.copy_from_slice(&b[18..34]);
                info = Some(StreamInfo {
                    sample_rate: (packed >> 44) as u32,
                    channels: ((packed >> 41) & 0x7) as u8 + 1,
                    bits_per_sample: ((packed >> 36) & 0x1f) as u8 + 1,
                    total_samples: packed & 0xf_ffff_ffff,
                    md5,
                });
            }
            4 if tags.is_none() => {
                let mut b = vec![0u8; len];
                r.read_exact(&mut b)?;
                tags = Some(parse_vorbis_comments(&b)?);
            }
            6 => {
                picture_count += 1;
                r.seek(SeekFrom::Current(len as i64))?;
            }
            _ => {
                r.seek(SeekFrom::Current(len as i64))?;
            }
        }
        if last {
            break;
        }
    }
    Ok(FlacMeta {
        info: info.ok_or_else(|| invalid("no STREAMINFO block"))?,
        tags,
        picture_count,
    })
}

fn parse_vorbis_comments(b: &[u8]) -> io::Result<Vec<(String, String)>> {
    let mut pos = 0usize;
    let mut take = |n: usize| -> io::Result<&[u8]> {
        let end = pos
            .checked_add(n)
            .filter(|e| *e <= b.len())
            .ok_or_else(|| invalid("truncated comment block"))?;
        let s = &b[pos..end];
        pos = end;
        Ok(s)
    };
    let le32 = |s: &[u8]| u32::from_le_bytes(s.try_into().expect("4 bytes")) as usize;
    let vendor_len = le32(take(4)?);
    take(vendor_len)?;
    let count = le32(take(4)?);
    let mut out = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let len = le32(take(4)?);
        let entry = String::from_utf8_lossy(take(len)?);
        if let Some((k, v)) = entry.split_once('=') {
            out.push((k.to_lowercase(), v.to_string()));
        }
    }
    Ok(out)
}
