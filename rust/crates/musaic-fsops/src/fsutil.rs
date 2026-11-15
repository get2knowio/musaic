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
/// hard link + unlink, or (no hard links / other volume) copy into a file
/// created with `create_new`, then remove the source. A failed copy removes
/// its partial target and leaves the source in place.
pub(crate) fn move_noclobber(src: &Path, dst: &Path) -> io::Result<()> {
    match link_then_unlink(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            let mut out = OpenOptions::new().write(true).create_new(true).open(dst)?;
            let copied = (|| {
                let mut input = fs::File::open(src)?;
                io::copy(&mut input, &mut out)?;
                out.sync_all()?;
                if let Ok(m) = fs::metadata(src) {
                    let _ = fs::set_permissions(dst, m.permissions());
                }
                Ok::<(), io::Error>(())
            })();
            if let Err(e) = copied {
                let _ = fs::remove_file(dst);
                return Err(e);
            }
            fs::remove_file(src)
        }
    }
}
