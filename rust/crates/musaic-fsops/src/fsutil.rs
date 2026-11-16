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
use std::path::Path;

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
