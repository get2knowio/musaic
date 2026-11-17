//! Moves that never replace an existing file.
//!
//! `std::fs::rename` (like Python's `os.rename` / `shutil.move`) silently
//! replaces an existing target on Unix. Both helpers here create the target
//! name with an operation that fails if it exists (a hard link, or a file
//! opened with `create_new`), and only then drop the source name, so the
//! file always has at least one name and nothing else is overwritten, even
//! if another process creates the target in between.

use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

/// How a name compares on a case- and normalisation-insensitive volume.
pub(crate) fn fold(name: &str) -> String {
    name.nfc().collect::<String>().to_lowercase()
}

/// Whether `dir` lists an entry spelled `name`: the same characters in the
/// same case. Names are compared after NFC normalisation because a volume
/// that stores names decomposed (HFS+) lists an NFC name in its NFD form;
/// a normalisation-only difference is never planned as a rename.
///
/// This reads the directory instead of asking whether the path exists: on
/// a case-insensitive volume `foo.flac` "exists" when the entry is
/// `Foo.flac`, so only the listing says which spelling is on disk.
///
/// `Err` when the directory cannot be read (or an entry cannot be read
/// while listing it): the caller cannot tell, and must not guess, which
/// spelling is there.
pub(crate) fn listed(dir: &Path, name: &str) -> io::Result<bool> {
    for e in fs::read_dir(dir)? {
        if lists_as(&e?.file_name(), name) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether a listed entry `n` is `name` as [`listed`] decides it: the same
/// characters in the same case, after NFC normalisation.
pub(crate) fn lists_as(n: &std::ffi::OsStr, name: &str) -> bool {
    let n = n.to_string_lossy();
    n == name || n.nfc().eq(name.nfc())
}

/// The longest name most volumes accept (255 bytes on ext4 / APFS, 255
/// UTF-16 units on NTFS; a UTF-8 byte count is never smaller).
const NAME_MAX: usize = 255;

/// The `n`th temporary name for an entry called `name`, in the same
/// directory: `.<name>.renaming.tmp`, then `.<name>.renaming.<n>.tmp`; when
/// that would be longer than a volume accepts (a name already near the
/// limit), the short `.musaic-<n>.tmp`.
pub(crate) fn temp_name(name: &str, n: usize) -> String {
    let long = if n == 1 {
        format!(".{name}.renaming.tmp")
    } else {
        format!(".{name}.renaming.{n}.tmp")
    };
    if long.len() <= NAME_MAX {
        long
    } else {
        format!(".musaic-{n}.tmp")
    }
}

/// A name next to `path` that nothing uses yet ([`temp_name`]).
fn free_temp_name(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    for n in 1..1000 {
        let tmp = path.with_file_name(temp_name(&name, n));
        if fs::symlink_metadata(&tmp).is_err() {
            return Ok(tmp);
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "no free temporary name",
    ))
}

/// Rename a directory within its parent without replacing anything.
///
/// When `src` and `dst` are the same directory (a case- or
/// normalisation-only change of spelling on an insensitive volume, decided
/// by same-file, never by comparing strings) the move goes through a
/// temporary name in the same parent, so the new spelling is written by a
/// real rename on every volume. If the second step fails the directory is
/// moved back to `src`; an error that also could not do that names the
/// temporary name the directory is left under.
///
/// Otherwise an existing `dst` is refused (`fs::rename` would replace an
/// empty directory on Unix).
pub(crate) fn rename_dir(src: &Path, dst: &Path) -> io::Result<()> {
    if !same_file::is_same_file(src, dst).unwrap_or(false) {
        if fs::symlink_metadata(dst).is_ok() {
            return Err(io::Error::new(ErrorKind::AlreadyExists, "target exists"));
        }
        return fs::rename(src, dst);
    }
    let tmp = free_temp_name(src)?;
    fs::rename(src, &tmp)?;
    let second = if fs::symlink_metadata(dst).is_ok() {
        // Something took the name while the directory was away.
        Err(io::Error::new(ErrorKind::AlreadyExists, "target exists"))
    } else {
        fs::rename(&tmp, dst)
    };
    if let Err(e) = second {
        if let Err(back) = fs::rename(&tmp, src) {
            musaic_log::error(
                crate::plan::TARGET,
                &format!(
                    "rename: {} could not be renamed to {} ({e}) nor put back ({back}); \
                     it is left at {}",
                    src.display(),
                    dst.display(),
                    tmp.display()
                ),
            );
            let left = tmp
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            return Err(io::Error::new(
                e.kind(),
                format!("{e}; the folder is left as {left}: {back}"),
            ));
        }
        return Err(e);
    }
    Ok(())
}

fn link_then_unlink(src: &Path, dst: &Path) -> io::Result<()> {
    fs::hard_link(src, dst)?;
    if let Err(e) = fs::remove_file(src) {
        // Undo: dst is a second name of the same inode, so this loses nothing.
        let _ = fs::remove_file(dst);
        return Err(e);
    }
    Ok(())
}

/// Rename a regular file within one volume without replacing `dst`.
///
/// Where the filesystem has no hard links (FAT/exFAT, some SMB shares) this
/// falls back to "fail if `dst` exists, else rename": safe against
/// everything except a process creating `dst` in the instant between the
/// check and the rename.
pub(crate) fn rename_noclobber(src: &Path, dst: &Path) -> io::Result<()> {
    match link_then_unlink(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            if fs::symlink_metadata(dst).is_ok() {
                return Err(io::Error::new(ErrorKind::AlreadyExists, "target exists"));
            }
            fs::rename(src, dst)
        }
    }
}

/// Move a regular file, possibly to another volume, without replacing `dst`:
/// hard link + unlink, or (no hard links / other volume) [`copy_then_remove`].
pub(crate) fn move_noclobber(src: &Path, dst: &Path) -> io::Result<()> {
    match link_then_unlink(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(e),
        Err(_) => copy_then_remove(src, dst),
    }
}

/// The cross-volume half of [`move_noclobber`]: copy into a file created with
/// `create_new`, carrying over the permissions and the access and
/// modification times, then remove the source. On any failure the copy is
/// removed and the source stays where it was, so a failed move never leaves
/// two copies or none.
pub(crate) fn copy_then_remove(src: &Path, dst: &Path) -> io::Result<()> {
    let mut out = OpenOptions::new().write(true).create_new(true).open(dst)?;
    let copied = (|| {
        let mut input = fs::File::open(src)?;
        io::copy(&mut input, &mut out)?;
        let meta = input.metadata()?;
        // Best effort, as for permissions: a target filesystem that cannot
        // store a time is not a reason to keep the file out of the trash.
        let mut times = fs::FileTimes::new();
        if let Ok(t) = meta.modified() {
            times = times.set_modified(t);
        }
        if let Ok(t) = meta.accessed() {
            times = times.set_accessed(t);
        }
        let _ = out.set_times(times);
        out.sync_all()?;
        let _ = fs::set_permissions(dst, meta.permissions());
        Ok::<(), io::Error>(())
    })();
    drop(out);
    if let Err(e) = copied {
        let _ = fs::remove_file(dst);
        return Err(e);
    }
    if let Err(e) = fs::remove_file(src) {
        // The source is still there: drop the copy rather than keep two.
        let _ = fs::remove_file(dst);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name near the limit falls back to the short temporary name;
    /// every other name keeps the descriptive one.
    #[test]
    fn temp_names_stay_within_the_name_limit() {
        assert_eq!(temp_name("a.flac", 1), ".a.flac.renaming.tmp");
        assert_eq!(temp_name("a.flac", 3), ".a.flac.renaming.3.tmp");
        let long = "x".repeat(250);
        assert_eq!(temp_name(&long, 1), ".musaic-1.tmp");
        assert_eq!(temp_name(&long, 2), ".musaic-2.tmp");
        let edge = "y".repeat(NAME_MAX - ".renaming.tmp".len() - 1);
        assert_eq!(temp_name(&edge, 1).len(), NAME_MAX);
        assert!(temp_name(&edge, 2).starts_with(".musaic-"));
    }
    use std::time::{Duration, SystemTime};

    fn old_time() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_500_000_000)
    }

    /// The cross-volume fallback keeps the bytes and the modification time
    /// and leaves no source behind.
    #[test]
    fn copy_keeps_contents_and_modification_time() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst) = (tmp.path().join("a.flac"), tmp.path().join("b.flac"));
        fs::write(&src, b"audio").unwrap();
        fs::File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_modified(old_time())
            .unwrap();
        copy_then_remove(&src, &dst).unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"audio");
        assert_eq!(fs::metadata(&dst).unwrap().modified().unwrap(), old_time());
    }

    /// An existing target is never replaced, and the source stays.
    #[test]
    fn copy_never_replaces_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst) = (tmp.path().join("a.flac"), tmp.path().join("b.flac"));
        fs::write(&src, b"new").unwrap();
        fs::write(&dst, b"old").unwrap();
        let e = copy_then_remove(&src, &dst).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&src).unwrap(), b"new");
        assert_eq!(fs::read(&dst).unwrap(), b"old");
    }

    /// When the source cannot be removed the copy is dropped: one file, in
    /// its old place.
    #[cfg(unix)]
    #[test]
    fn source_that_cannot_be_removed_leaves_no_copy() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let from = tmp.path().join("from");
        fs::create_dir(&from).unwrap();
        let src = from.join("a.flac");
        let dst = tmp.path().join("b.flac");
        fs::write(&src, b"audio").unwrap();
        fs::set_permissions(&from, fs::Permissions::from_mode(0o555)).unwrap();
        let r = copy_then_remove(&src, &dst);
        fs::set_permissions(&from, fs::Permissions::from_mode(0o755)).unwrap();
        if r.is_ok() {
            panic!("removed a file from a read-only dir (running as root?)");
        }
        assert_eq!(fs::read(&src).unwrap(), b"audio");
        assert!(!dst.exists());
    }
}
