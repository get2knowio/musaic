//! FLAC metadata model and writer that lays blocks out exactly as mutagen's
//! `FLAC.save()` does.
//!
//! Only parsed equality is required, but reproducing mutagen's layout is
//! cheap and makes a tag-only write byte-identical to the Python app's, which the
//! golden tests check. The rules copied from mutagen 1.48:
//!
//! - every block is parsed and re-serialised on save: STREAMINFO keeps its
//!   first 34 bytes, SEEKTABLE whole 18-byte seek points, VORBIS_COMMENT and
//!   PICTURE their parsed fields; CUESHEET, APPLICATION and unknown blocks
//!   are kept verbatim;
//! - all PADDING blocks are dropped and ONE padding block is appended last,
//!   sized by mutagen's default `PaddingInfo` strategy;
//! - a VORBIS_COMMENT block added to a file without one, and a new PICTURE,
//!   are appended after the existing blocks;
//! - an ID3v2 tag in front of `fLaC` is kept (`save(deleteid3=False)`).
//!
//! Audio frames are never parsed or rewritten; they are copied byte for byte.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Largest body a metadata block header can describe (24-bit length).
pub const MAX_BLOCK_SIZE: usize = (1 << 24) - 1;

/// Vendor string written into a VORBIS_COMMENT block this build creates
/// (our own name, not mutagen's). Only files that
/// had no comment block get it; an existing block keeps its vendor string.
pub const NEW_BLOCK_VENDOR: &str = concat!("Musaic ", env!("CARGO_PKG_VERSION"));

const STREAMINFO: u8 = 0;
const PADDING: u8 = 1;
const SEEKTABLE: u8 = 3;
const VORBIS_COMMENT: u8 = 4;
const CUESHEET: u8 = 5;
const PICTURE: u8 = 6;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// A FLAC PICTURE block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// ID3v2 APIC picture type (3 = front cover).
    pub kind: u32,
    pub mime: String,
    pub desc: String,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub colors: u32,
    pub data: Vec<u8>,
}

impl Picture {
    fn parse(b: &[u8]) -> io::Result<Picture> {
        let mut r = Cursor { b, pos: 0 };
        let kind = r.u32_be()?;
        let n = r.u32_be()? as usize;
        let mime = String::from_utf8_lossy(r.take(n)?).into_owned();
        let n = r.u32_be()? as usize;
        let desc = String::from_utf8_lossy(r.take(n)?).into_owned();
        let (width, height, depth, colors) = (r.u32_be()?, r.u32_be()?, r.u32_be()?, r.u32_be()?);
        let n = r.u32_be()? as usize;
        let data = r.take(n)?.to_vec();
        Ok(Picture {
            kind,
            mime,
            desc,
            width,
            height,
            depth,
            colors,
            data,
        })
    }

    fn write(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.mime.len() + self.desc.len() + self.data.len());
        out.extend_from_slice(&self.kind.to_be_bytes());
        out.extend_from_slice(&(self.mime.len() as u32).to_be_bytes());
        out.extend_from_slice(self.mime.as_bytes());
        out.extend_from_slice(&(self.desc.len() as u32).to_be_bytes());
        out.extend_from_slice(self.desc.as_bytes());
        for v in [
            self.width,
            self.height,
            self.depth,
            self.colors,
            self.data.len() as u32,
        ] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&self.data);
        out
    }
}

/// A Vorbis comment block as mutagen's `VCFLACDict` holds it: the vendor
/// string and the comments in file order, keys with their on-disk casing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VorbisComment {
    pub vendor: String,
    pub comments: Vec<(String, String)>,
}

/// mutagen `is_valid_key`: non-empty, every char in 0x20..=0x7D except '='.
fn is_valid_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| (' '..='}').contains(&c) && c != '=')
}

impl VorbisComment {
    fn new() -> VorbisComment {
        VorbisComment {
            vendor: NEW_BLOCK_VENDOR.to_string(),
            comments: Vec::new(),
        }
    }

    /// `VComment.load(errors='replace', framing=False)`.
    fn parse(b: &[u8]) -> io::Result<VorbisComment> {
        let mut r = Cursor { b, pos: 0 };
        let n = r.u32_le()? as usize;
        let vendor = String::from_utf8_lossy(r.take(n)?).into_owned();
        let count = r.u32_le()?;
        let mut comments = Vec::new();
        for i in 0..count {
            let n = r.u32_le()? as usize;
            let s = String::from_utf8_lossy(r.take(n)?).into_owned();
            let (key, value) = match s.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (format!("unknown{i}"), s),
            };
            // tag.encode('ascii', 'replace'): each non-ASCII char becomes '?'.
            let key: String = key
                .chars()
                .map(|c| if c.is_ascii() { c } else { '?' })
                .collect();
            if is_valid_key(&key) {
                comments.push((key, value));
            }
        }
        Ok(VorbisComment { vendor, comments })
    }

    fn write(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(self.vendor.len() as u32).to_le_bytes());
        out.extend_from_slice(self.vendor.as_bytes());
        out.extend_from_slice(&(self.comments.len() as u32).to_le_bytes());
        for (k, v) in &self.comments {
            out.extend_from_slice(&((k.len() + 1 + v.len()) as u32).to_le_bytes());
            out.extend_from_slice(k.as_bytes());
            out.push(b'=');
            out.extend_from_slice(v.as_bytes());
        }
        out
    }

    /// First value stored under `key`, case-insensitively (`tags.get(key)[0]`).
    pub fn first(&self, key: &str) -> Option<&str> {
        self.comments
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// Every value stored under `key`, case-insensitively, in file order.
    pub fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.comments
            .iter()
            .filter(move |(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// `del tags[key]`: delete every entry whose key matches
    /// case-insensitively (every value of a multi-valued tag, every spelling
    /// of the key). Returns whether anything was removed.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.comments.len();
        self.comments.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        self.comments.len() != before
    }

    /// `tags[key] = value`: delete every entry whose key matches
    /// case-insensitively, then append `(key, value)` at the end with the
    /// caller's spelling of the key.
    pub fn set(&mut self, key: &str, value: &str) {
        assert!(is_valid_key(key), "invalid Vorbis comment key {key:?}");
        self.comments.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
        self.comments.push((key.to_string(), value.to_string()));
    }

    /// Append one more `(key, value)` entry, keeping every entry already
    /// there (a multi-valued tag gets another value).
    pub fn append(&mut self, key: &str, value: &str) {
        assert!(is_valid_key(key), "invalid Vorbis comment key {key:?}");
        self.comments.push((key.to_string(), value.to_string()));
    }

    /// Replace the value of `key` where the comment already is: the first
    /// entry whose key matches case-insensitively keeps its place and its
    /// spelling of the key and gets `value`; every other entry of the key
    /// is deleted. Returns `false`, changing nothing, when the key is
    /// absent.
    pub fn replace(&mut self, key: &str, value: &str) -> bool {
        let Some(at) = self
            .comments
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(key))
        else {
            return false;
        };
        self.comments[at].1 = value.to_string();
        let mut i = 0;
        self.comments.retain(|(k, _)| {
            let keep = i == at || !k.eq_ignore_ascii_case(key);
            i += 1;
            keep
        });
        true
    }
}

#[derive(Debug, Clone)]
enum Block {
    StreamInfo(Vec<u8>),
    Padding,
    SeekTable(Vec<u8>),
    Vorbis(VorbisComment),
    Picture(Picture),
    Raw { code: u8, data: Vec<u8> },
}

impl Block {
    fn code(&self) -> u8 {
        match self {
            Block::StreamInfo(_) => STREAMINFO,
            Block::Padding => PADDING,
            Block::SeekTable(_) => SEEKTABLE,
            Block::Vorbis(_) => VORBIS_COMMENT,
            Block::Picture(_) => PICTURE,
            Block::Raw { code, .. } => *code,
        }
    }
}

/// How [`FlacFile::save`] wrote the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveMode {
    /// The metadata region kept its size and was overwritten in place (what
    /// mutagen does in this case too).
    InPlace,
    /// The metadata region changed size: the whole file was written to a
    /// temporary sibling, fsynced and renamed over the original.
    Replaced,
}

/// What [`FlacFile::save`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    pub mode: SaveMode,
    /// One sentence per thing the filesystem kept about the old file that
    /// the rewritten file could not keep (an extended attribute, the owner,
    /// a hard link; see [`crate::preserve`]). Always empty for
    /// [`SaveMode::InPlace`]. Also logged as WARNING on `musaic.tags`.
    pub warnings: Vec<String>,
}

/// A FLAC file's metadata, loaded the way mutagen loads it.
#[derive(Debug, Clone)]
pub struct FlacFile {
    path: PathBuf,
    /// Offset just after `fLaC` (mutagen's `header`): 4, or more with ID3v2.
    header: u64,
    blocks: Vec<Block>,
    /// Index of the block mutagen exposes as `.tags` (the first VORBIS_COMMENT).
    tags: Option<usize>,
    audio_offset: u64,
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.b.len());
        let end = end.ok_or_else(|| invalid("truncated metadata block"))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u32_be(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    fn u32_le(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
}

/// Offset just after `fLaC`, skipping an ID3v2 tag like mutagen's
/// `__check_header` (10-byte header + syncsafe size; no footer handling).
fn check_header<R: Read + Seek>(r: &mut R) -> io::Result<u64> {
    let mut head = [0u8; 4];
    r.read_exact(&mut head)?;
    if &head == b"fLaC" {
        return Ok(4);
    }
    if &head[..3] == b"ID3" {
        let mut rest = [0u8; 6];
        r.read_exact(&mut rest)?;
        let size = rest[2..]
            .iter()
            .fold(0u64, |acc, b| (acc << 7) | u64::from(b & 0x7f));
        let header = 14 + size;
        r.seek(SeekFrom::Start(header - 4))?;
        r.read_exact(&mut head)?;
        if &head == b"fLaC" {
            return Ok(header);
        }
    }
    Err(invalid("not a valid FLAC file"))
}

impl FlacFile {
    /// Parse the metadata blocks (`FLAC(path)`). Errors where mutagen raises.
    pub fn open(path: &Path) -> io::Result<FlacFile> {
        let mut r = BufReader::new(File::open(path)?);
        let header = check_header(&mut r)?;
        let mut blocks = Vec::new();
        let (mut tags, mut seektables, mut cuesheets, mut streaminfo) = (None, 0, 0, false);
        loop {
            let mut h = [0u8; 4];
            r.read_exact(&mut h)?;
            let code = h[0] & 0x7f;
            let last = h[0] & 0x80 != 0;
            let size = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
            let mut body = vec![0u8; size];
            r.read_exact(&mut body)?;
            let block = match code {
                STREAMINFO => {
                    if size < 34 {
                        return Err(invalid(format!("file said 34 bytes, read {size} bytes")));
                    }
                    let packed = u64::from_be_bytes(body[10..18].try_into().expect("8 bytes"));
                    if packed >> 44 == 0 {
                        return Err(invalid("A sample rate value of 0 is invalid"));
                    }
                    body.truncate(34);
                    streaminfo = true;
                    Block::StreamInfo(body)
                }
                PADDING => Block::Padding,
                SEEKTABLE => {
                    seektables += 1;
                    if seektables > 1 {
                        return Err(invalid("> 1 SeekTable block found"));
                    }
                    body.truncate(size / 18 * 18);
                    Block::SeekTable(body)
                }
                VORBIS_COMMENT => {
                    if tags.is_none() {
                        tags = Some(blocks.len());
                    }
                    Block::Vorbis(VorbisComment::parse(&body)?)
                }
                CUESHEET => {
                    cuesheets += 1;
                    if cuesheets > 1 {
                        return Err(invalid("> 1 CueSheet block found"));
                    }
                    Block::Raw { code, data: body }
                }
                PICTURE => Block::Picture(Picture::parse(&body)?),
                _ => Block::Raw { code, data: body },
            };
            blocks.push(block);
            if last {
                break;
            }
        }
        if !streaminfo {
            return Err(invalid("Stream info block not found"));
        }
        let audio_offset = r.stream_position()?;
        Ok(FlacFile {
            path: path.to_path_buf(),
            header,
            blocks,
            tags,
            audio_offset,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Byte offset of the first audio frame.
    pub fn audio_offset(&self) -> u64 {
        self.audio_offset
    }

    /// Length of the ID3v2 tag in front of `fLaC` (0 if none).
    pub fn id3_prefix_len(&self) -> u64 {
        self.header - 4
    }

    /// The Vorbis comment block mutagen exposes as `.tags`, if any.
    pub fn tags(&self) -> Option<&VorbisComment> {
        self.tags.map(|i| match &self.blocks[i] {
            Block::Vorbis(vc) => vc,
            _ => unreachable!("tags index points at a Vorbis block"),
        })
    }

    /// `.tags`, creating an empty block appended after the existing blocks
    /// when the file has none (`FileType.__setitem__` -> `add_tags()`).
    pub fn tags_mut(&mut self) -> &mut VorbisComment {
        let i = match self.tags {
            Some(i) => i,
            None => {
                self.blocks.push(Block::Vorbis(VorbisComment::new()));
                self.tags = Some(self.blocks.len() - 1);
                self.blocks.len() - 1
            }
        };
        match &mut self.blocks[i] {
            Block::Vorbis(vc) => vc,
            _ => unreachable!("tags index points at a Vorbis block"),
        }
    }

    /// Every PICTURE block in file order.
    pub fn pictures(&self) -> Vec<&Picture> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Picture(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// `clear_pictures()`: drop every PICTURE block.
    pub fn clear_pictures(&mut self) {
        self.blocks.retain(|b| b.code() != PICTURE);
        if self.tags.is_some() {
            self.tags = self.blocks.iter().position(|b| b.code() == VORBIS_COMMENT);
        }
    }

    /// `add_picture(pic)`: append a PICTURE block after the existing blocks.
    pub fn add_picture(&mut self, picture: Picture) {
        self.blocks.push(Block::Picture(picture));
    }

    /// The metadata blocks as `MetadataBlock._writeblocks` renders them for a
    /// region that currently holds `available` bytes, followed by
    /// `content_size` bytes of audio.
    fn render_blocks(&self, available: u64, content_size: u64) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        for block in &self.blocks {
            let body = match block {
                Block::Padding => continue,
                Block::StreamInfo(b) | Block::SeekTable(b) | Block::Raw { data: b, .. } => {
                    std::borrow::Cow::Borrowed(b.as_slice())
                }
                Block::Vorbis(vc) => std::borrow::Cow::Owned(vc.write()),
                Block::Picture(p) => std::borrow::Cow::Owned(p.write()),
            };
            push_block(&mut data, block.code(), &body, false)?;
        }
        // The padding block's own 4-byte header always counts.
        let blocks_size = data.len() as i64 + 4;
        let padding = default_padding(available as i64 - blocks_size, content_size as i64);
        let padding = padding.min(MAX_BLOCK_SIZE as i64) as usize;
        push_block(&mut data, PADDING, &vec![0u8; padding], true)?;
        Ok(data)
    }

    /// Write the metadata back (`audio.save()`), leaving the audio frames and
    /// any ID3v2 prefix untouched.
    ///
    /// When the rendered metadata has exactly the old region's size it is
    /// overwritten in place, as mutagen does. Otherwise mutagen shifts the
    /// audio inside the same file (not crash-safe); here the complete new
    /// file is written to `.<name>.musaic-tags.tmp` next to it, given what
    /// the filesystem keeps about the original (permissions, extended
    /// attributes, owner, creation date: [`crate::preserve`]), fsynced and
    /// renamed over the original, so a crash leaves either the old or the
    /// new file. What could not be carried over is returned as warnings in
    /// the [`SaveReport`]. A symlink is followed: the file it points to is
    /// replaced and the link kept.
    ///
    /// Either way the file itself is first opened for writing, the open the
    /// in-place path needs anyway: a file that
    /// cannot be opened for writing (read-only mode, ACL, immutable flag) is
    /// refused with that open's error (`PermissionDenied`, the same OS error
    /// as the Python app's `open(path, "rb+")` in mutagen), and nothing is
    /// written. Without it a read-only file in a writable directory would be
    /// replaced, because a rename needs only directory permission.
    ///
    /// Directory permission matters only for the replace path: a writable
    /// file in a read-only directory is still saved in place when the
    /// metadata fits, and refused (the temp file cannot be created; the
    /// original is untouched) when it does not. The Python app (mutagen shifts
    /// the audio inside the open file) saves that second case; it is a
    /// consequence of the temp-file write, not of the read-only probe.
    pub fn save(&mut self) -> io::Result<SaveReport> {
        let mut writable = OpenOptions::new().write(true).open(&self.path)?;
        let file_len = writable.metadata()?.len();
        let available = self.audio_offset - self.header;
        let content_size = file_len
            .checked_sub(self.audio_offset)
            .ok_or_else(|| invalid("file shrank since it was read"))?;
        let data = self.render_blocks(available, content_size)?;
        let report = if data.len() as u64 == available {
            let f = &mut writable;
            f.seek(SeekFrom::Start(self.header - 4))?;
            f.write_all(b"fLaC")?;
            f.write_all(&data)?;
            f.flush()?;
            SaveReport {
                mode: SaveMode::InPlace,
                warnings: Vec::new(),
            }
        } else {
            drop(writable);
            let warnings = self.replace_whole_file(&data)?;
            for w in &warnings {
                musaic_log::warning("musaic.tags", &format!("{}: {w}", self.path.display()));
            }
            SaveReport {
                mode: SaveMode::Replaced,
                warnings,
            }
        };
        // Reload the layout the file now has, so a second save behaves like
        // mutagen's (which re-reads header and audio offset from disk).
        self.audio_offset = self.header + data.len() as u64;
        self.blocks.retain(|b| b.code() != PADDING);
        self.blocks.push(Block::Padding);
        self.tags = self.blocks.iter().position(|b| b.code() == VORBIS_COMMENT);
        Ok(report)
    }

    fn replace_whole_file(&self, data: &[u8]) -> io::Result<Vec<String>> {
        // Replace the file a symlink points to, not the link: renaming over
        // the link would turn it into a regular file and leave the target
        // with the old tags.
        let target = if fs::symlink_metadata(&self.path)?.file_type().is_symlink() {
            fs::canonicalize(&self.path)?
        } else {
            self.path.clone()
        };
        let name = target
            .file_name()
            .ok_or_else(|| invalid("path has no file name"))?;
        let tmp = target.with_file_name(format!(".{}.musaic-tags.tmp", name.to_string_lossy()));
        // A temp file left by a crash may carry attributes of its own.
        let _ = fs::remove_file(&tmp);
        let result = (|| -> io::Result<Vec<String>> {
            let mut src = File::open(&target)?;
            let meta = src.metadata()?;
            let mut out = create_replacement(&target, &tmp)?;
            let mut prefix = vec![0u8; (self.header - 4) as usize];
            src.read_exact(&mut prefix)?;
            out.write_all(&prefix)?;
            out.write_all(b"fLaC")?;
            out.write_all(data)?;
            src.seek(SeekFrom::Start(self.audio_offset))?;
            io::copy(&mut src, &mut out)?;
            let warnings = crate::preserve::carry_over(&src, &meta, &out)?;
            // After the attributes, so the rename never publishes a file
            // whose data or attributes are still only in the cache.
            out.sync_all()?;
            drop(out);
            fs::rename(&tmp, &target)?;
            Ok(warnings)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

/// The empty temporary file the rewritten FLAC goes into.
#[cfg(not(windows))]
fn create_replacement(_original: &Path, tmp: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(tmp)
}

/// On Windows, a copy of the original (`CopyFileExW` carries its alternate
/// data streams, file attributes and NTFS extended attributes, which std has
/// no other way to reach), emptied. Costs one extra copy of the audio.
#[cfg(windows)]
fn create_replacement(original: &Path, tmp: &Path) -> io::Result<File> {
    fs::copy(original, tmp)?;
    let out = OpenOptions::new().write(true).open(tmp)?;
    out.set_len(0)?;
    Ok(out)
}

fn push_block(out: &mut Vec<u8>, code: u8, body: &[u8], last: bool) -> io::Result<()> {
    if body.len() > MAX_BLOCK_SIZE {
        return Err(invalid("block is too long to write"));
    }
    out.push(if last { code | 0x80 } else { code });
    out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(body);
    Ok(())
}

/// mutagen `PaddingInfo.get_default_padding()`.
fn default_padding(padding: i64, size: i64) -> i64 {
    let high = 1024 * 10 + size / 100;
    let low = 1024 + size / 1000;
    if padding >= 0 {
        if padding > high { low } else { padding }
    } else {
        low
    }
}
