//! Album covers (`scanner.get_embedded_cover`, `thumbs.py`): the largest
//! embedded FLAC picture, cover thumbnails (`<config>/thumbs/<album
//! id>.jpg`, long edge 384 px, JPEG quality 82, as the Python app made them), their ETag, and
//! the cover route's choice of what to serve: the disk cover, else the
//! embedded one, else nothing (the host answers 404, [`NO_COVER`]).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::Json;

/// The cover route's 404 detail.
pub const NO_COVER: &str = "No cover art for this album";

/// The largest PICTURE block of a FLAC (first one on ties), with its MIME
/// (`image/jpeg` when empty).
pub fn embedded_cover(flac: &Path) -> Option<(Vec<u8>, String)> {
    let mut f = std::io::BufReader::new(std::fs::File::open(flac).ok()?);
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    if &magic != b"fLaC" {
        return None;
    }
    let mut best: Option<(Vec<u8>, String)> = None;
    loop {
        let mut h = [0u8; 4];
        f.read_exact(&mut h).ok()?;
        let last = h[0] & 0x80 != 0;
        let kind = h[0] & 0x7f;
        let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
        let mut block = vec![0u8; len];
        f.read_exact(&mut block).ok()?;
        if kind == 6
            && let Some((data, mime)) = parse_picture(&block)
            && best.as_ref().is_none_or(|(b, _)| data.len() > b.len())
        {
            best = Some((data, mime));
        }
        if last {
            break;
        }
    }
    best
}

fn parse_picture(b: &[u8]) -> Option<(Vec<u8>, String)> {
    let mut i = 0usize;
    let u32_at = |i: &mut usize| -> Option<usize> {
        let v = b.get(*i..*i + 4)?;
        *i += 4;
        Some(u32::from_be_bytes([v[0], v[1], v[2], v[3]]) as usize)
    };
    let _ptype = u32_at(&mut i)?;
    let mlen = u32_at(&mut i)?;
    let mime = String::from_utf8_lossy(b.get(i..i + mlen)?).into_owned();
    i += mlen;
    let dlen = u32_at(&mut i)?;
    i += dlen;
    for _ in 0..4 {
        u32_at(&mut i)?;
    }
    let len = u32_at(&mut i)?;
    let data = b.get(i..i + len)?.to_vec();
    let mime = if mime.is_empty() {
        "image/jpeg".into()
    } else {
        mime
    };
    Some((data, mime))
}

/// `thumbs.get_source_image(album)`: the disk cover, else the embedded one.
fn source_image(album_path: &str, album: &Json) -> Option<Vec<u8>> {
    let cover_file = album.get("cover_file").and_then(Json::as_str);
    if let Some(p) = musaic_fsops::cover::disk_cover_path(album_path, cover_file) {
        match std::fs::read(&p) {
            Ok(b) => return Some(b),
            // thumbs.py:69; `e` is io::Error's text, not Python's OSError.
            Err(e) => musaic_log::warning(
                crate::thumbs::THUMBS_LOG,
                &format!("failed to read on-disk cover {}: {e}", p.display()),
            ),
        }
    }
    if album.get("has_embedded_cover") == Some(&Json::Bool(true)) {
        let sample = album.get("sample_track").and_then(Json::as_str)?;
        if !sample.is_empty() {
            return embedded_cover(Path::new(sample)).map(|(d, _)| d);
        }
    }
    None
}

/// `thumbs.ensure_thumb(album, force=force)`: generate the thumbnail when
/// missing (or always with `force`, as the scan does for fresh albums).
/// Pixels follow Pillow's ([`crate::thumbs`]); the write is a temp file +
/// rename, as in the Python app.
pub fn ensure_thumb(
    dir: &Path,
    album_id: &str,
    album_path: &str,
    album: &Json,
    force: bool,
) -> Option<PathBuf> {
    if album_id.is_empty() {
        return None;
    }
    let path = dir.join(format!("{album_id}.jpg"));
    if path.is_file() && !force {
        return Some(path);
    }
    let src = source_image(album_path, album)?;
    let out = crate::thumbs::make_thumbnail(&src)?;
    std::fs::create_dir_all(dir).ok()?;
    // The Python app's temp name was `<id>.jpg.tmp`, shared by every writer: a
    // scan (force) and a cover request for the same album could each write
    // it, and the slower rename then failed, so the cover route answered
    // 404 for an album that has a cover. Each writer now has its own temp
    // file; either rename leaves a complete thumbnail.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{album_id}.jpg.{}-{n}.tmp", std::process::id()));
    if std::fs::write(&tmp, &out).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(path)
}

/// `thumbs.delete_thumb(album_id)`: remove `<dir>/<album id>.jpg`; whether a
/// file was deleted. A missing file is not an error; any other failure is
/// logged as the Python app logged it (thumbs.py:142, WARNING on `musaic.thumbs`;
/// `e` is io::Error's text) and answers false.
pub fn delete_thumb(dir: &Path, album_id: &str) -> bool {
    match std::fs::remove_file(dir.join(format!("{album_id}.jpg"))) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            musaic_log::warning(
                crate::thumbs::THUMBS_LOG,
                &format!("delete_thumb({album_id}) failed: {e}"),
            );
            false
        }
    }
}

/// `thumbs.thumb_etag`: `"<album id>-<int mtime>"`.
pub fn thumb_etag(thumb: &Path, album_id: &str) -> Option<String> {
    let m = std::fs::metadata(thumb).ok()?.modified().ok()?;
    let secs = m.duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some(format!("\"{album_id}-{secs}\""))
}

/// What `GET /api/albums/{id}/cover?size=full` serves.
pub enum FullCover {
    /// The cover file next to the tracks (`FileResponse`).
    Disk(PathBuf),
    /// The largest embedded picture of the sample track, with its MIME.
    Embedded(Vec<u8>, String),
}

/// The full-size cover: the disk cover, else (when the album has one) the
/// sample track's embedded picture, else `None` (404, [`NO_COVER`]).
pub fn full_cover(album_path: &str, album: &Json) -> Option<FullCover> {
    let cover_file = album.get("cover_file").and_then(Json::as_str);
    if let Some(disk) = musaic_fsops::cover::disk_cover_path(album_path, cover_file) {
        return Some(FullCover::Disk(disk));
    }
    if album.get("has_embedded_cover") == Some(&Json::Bool(true)) {
        let sample = album
            .get("sample_track")
            .and_then(Json::as_str)
            .unwrap_or("");
        if let Some((data, mime)) = embedded_cover(Path::new(sample)) {
            return Some(FullCover::Embedded(data, mime));
        }
    }
    None
}

/// The thumbnail `GET /api/albums/{id}/cover` serves: the existing file,
/// else one generated now (`ensure_thumb(album)`), else `None` (404,
/// [`NO_COVER`]).
pub fn thumb_path(
    thumbs_dir: &Path,
    album_id: &str,
    album_path: &str,
    album: &Json,
) -> Option<PathBuf> {
    let thumb = thumbs_dir.join(format!("{album_id}.jpg"));
    if !thumb.is_file() && ensure_thumb(thumbs_dir, album_id, album_path, album, false).is_none() {
        return None;
    }
    Some(thumb)
}
