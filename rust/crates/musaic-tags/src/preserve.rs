//! Carrying a file's metadata over to the file that replaces it.
//!
//! When new metadata does not fit the old metadata region, [`crate::flac`]
//! writes the whole file to a temporary sibling and renames it over the
//! original (crash-safe: a crash leaves either the old or the new file).
//! The replacement is a new file, so everything the filesystem keeps about
//! the old one, rather than in it, has to be copied explicitly before the
//! rename. The policy, per kind:
//!
//! | What | macOS | Linux | Windows | If it cannot be kept |
//! |---|---|---|---|---|
//! | permission bits | copied | copied | read-only flag copied | save fails, original untouched |
//! | extended attributes (Finder tags and comments, quarantine, `user.*`, POSIX ACLs stored as `system.posix_acl_*`, SMB/NAS metadata) | copied | copied | n/a | warning |
//! | alternate data streams, file attributes, NTFS extended attributes | n/a | n/a | copied (`std::fs::copy`) | save fails |
//! | owner and group | copied when allowed | copied when allowed | not covered | warning |
//! | creation ("birth") time | copied | not settable on Linux | copied | warning |
//! | modification time | set to the time of the save | same | same | - |
//! | hard links | broken: only this name gets the new file | same | same (not detected) | warning |
//!
//! Why these choices:
//!
//! - **Warning, not failure, for attributes.** Every attribute is copied
//!   onto the temporary file before the rename, so a failure is known while
//!   the original is still untouched. Failing the save would make a file
//!   with one attribute the OS will not let us write (a system-managed
//!   label, an ACL on a file someone else owns) impossible to tag at all;
//!   the tags the user asked for are written and each attribute that did not
//!   survive is named in a per-file warning, so nothing is lost silently.
//!   Permissions stay a hard failure: a rewritten file with the wrong mode
//!   can expose or lock out the file.
//! - **"Not supported" is not a loss.** A filesystem without extended
//!   attributes (some network shares, FAT) cannot list any on the original,
//!   so there is nothing to keep and nothing to report.
//! - **Modification time is not preserved**: the content changed, and the
//!   library scan detects tag edits by modification time, as it does for a
//!   save that is written in place.
//! - **Hard links are broken, never shifted in place**: rewriting the shared
//!   file in place (moving the audio) is not crash-safe and can destroy the
//!   audio of every name at once; breaking the link loses nothing (the other
//!   names keep the old, intact file) and is reported.
//!
//! Not covered: macOS ACLs and BSD file flags (no safe API in std; ACLs
//! inherited from the folder are applied to the new file by the OS anyway),
//! Windows security descriptors (explicit, non-inherited ACEs), Windows
//! hard-link detection, and the owner when the process is not allowed to
//! give a file away (the usual case for anyone but root).

use std::fs::{File, Metadata};
use std::io;

/// Copy what can be copied from `original` (open for reading, `meta` its
/// metadata taken before the rewrite) onto `replacement`, the fully written
/// temporary file. Returns one sentence per thing that could not be kept.
/// Errors only when the permission bits cannot be set (or the new file's
/// own metadata cannot be read).
pub(crate) fn carry_over(
    original: &File,
    meta: &Metadata,
    replacement: &File,
) -> io::Result<Vec<String>> {
    let mut warnings = Vec::new();
    #[cfg(unix)]
    {
        hard_links(meta, &mut warnings);
        // Owner before mode: chown clears the set-user/group-id bits.
        owner(meta, replacement, &mut warnings)?;
        // Attributes before mode: writing one needs write permission on the
        // file, which the original's mode may not grant its owner.
        copy_xattrs(original, replacement, &mut warnings);
    }
    #[cfg(not(unix))]
    let _ = original;
    replacement.set_permissions(meta.permissions())?;
    created(meta, replacement, &mut warnings);
    Ok(warnings)
}

#[cfg(unix)]
fn hard_links(meta: &Metadata, warnings: &mut Vec<String>) {
    use std::os::unix::fs::MetadataExt;
    let others = meta.nlink().saturating_sub(1);
    if others > 0 {
        let (s, show) = if others == 1 {
            ("", "shows")
        } else {
            ("s", "show")
        };
        warnings.push(format!(
            "This file had {others} other hard link{s}. The new tags did not fit, so the \
             file was rewritten as a new file under this name only; the other link{s} \
             still {show} the old tags."
        ));
    }
}

#[cfg(unix)]
fn owner(meta: &Metadata, out: &File, warnings: &mut Vec<String>) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, fchown};
    let now = out.metadata()?;
    if (now.uid(), now.gid()) == (meta.uid(), meta.gid()) {
        return Ok(());
    }
    // Giving a file away needs privilege; keeping the group needs only
    // membership, so try that on its own when the pair is refused.
    let err = fchown(out, Some(meta.uid()), Some(meta.gid()))
        .inspect_err(|_| {
            let _ = fchown(out, None, Some(meta.gid()));
        })
        .err();
    let now = out.metadata()?;
    let why = err.map(|e| format!(" ({e})")).unwrap_or_default();
    if now.uid() != meta.uid() {
        warnings.push(format!(
            "The file's owner (user id {}) could not be kept when it was rewritten; it is \
             now owned by user id {}{why}.",
            meta.uid(),
            now.uid()
        ));
    }
    if now.gid() != meta.gid() {
        warnings.push(format!(
            "The file's group (group id {}) could not be kept when it was rewritten; it is \
             now group id {}{why}.",
            meta.gid(),
            now.gid()
        ));
    }
    Ok(())
}

/// "This filesystem has no extended attributes": `ENOTSUP` / `EOPNOTSUPP`.
#[cfg(unix)]
pub(crate) fn is_unsupported(e: &io::Error) -> bool {
    #[cfg(target_os = "linux")]
    const CODES: &[i32] = &[95]; // EOPNOTSUPP == ENOTSUP
    #[cfg(target_vendor = "apple")]
    const CODES: &[i32] = &[45, 102]; // ENOTSUP, EOPNOTSUPP
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    const CODES: &[i32] = &[];
    e.kind() == io::ErrorKind::Unsupported || e.raw_os_error().is_some_and(|c| CODES.contains(&c))
}

/// The extended-attribute calls [`copy_xattrs`] makes, so its policy can be
/// tested against a simulated filesystem.
#[cfg(unix)]
trait Xattrs {
    fn list(&self) -> io::Result<Vec<std::ffi::OsString>>;
    fn get(&self, name: &std::ffi::OsStr) -> io::Result<Option<Vec<u8>>>;
    fn set(&self, name: &std::ffi::OsStr, value: &[u8]) -> io::Result<()>;
}

#[cfg(unix)]
impl Xattrs for File {
    fn list(&self) -> io::Result<Vec<std::ffi::OsString>> {
        xattr::FileExt::list_xattr(self).map(Iterator::collect)
    }
    fn get(&self, name: &std::ffi::OsStr) -> io::Result<Option<Vec<u8>>> {
        xattr::FileExt::get_xattr(self, name)
    }
    fn set(&self, name: &std::ffi::OsStr, value: &[u8]) -> io::Result<()> {
        xattr::FileExt::set_xattr(self, name, value)
    }
}

#[cfg(unix)]
fn copy_xattrs(original: &impl Xattrs, out: &impl Xattrs, warnings: &mut Vec<String>) {
    let names = match original.list() {
        Ok(names) => names,
        // No extended attributes on this filesystem: nothing to keep.
        Err(e) if is_unsupported(&e) => return,
        Err(e) => {
            warnings.push(format!(
                "The file's extended attributes could not be read, so none were kept when \
                 it was rewritten ({e})."
            ));
            return;
        }
    };
    for name in names {
        let shown = name.to_string_lossy();
        let value = match original.get(&name) {
            Ok(Some(v)) => v,
            Ok(None) => continue, // removed meanwhile
            Err(e) => {
                warnings.push(format!(
                    "Extended attribute {shown:?} could not be read, so it was not kept when \
                     the file was rewritten ({e})."
                ));
                continue;
            }
        };
        if let Err(e) = out.set(&name, &value) {
            // An attribute the system manages itself (an SELinux label, the
            // macOS provenance tag) may refuse the write yet already hold the
            // same value on the new file.
            if out.get(&name).ok().flatten().as_deref() == Some(value.as_slice()) {
                continue;
            }
            warnings.push(format!(
                "Extended attribute {shown:?} could not be kept when the file was rewritten \
                 ({e})."
            ));
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
fn created(meta: &Metadata, out: &File, warnings: &mut Vec<String>) {
    #[cfg(target_os = "macos")]
    use std::os::macos::fs::FileTimesExt;
    #[cfg(windows)]
    use std::os::windows::fs::FileTimesExt;
    let Ok(created) = meta.created() else {
        return; // the filesystem does not record it
    };
    if let Err(e) = out.set_times(std::fs::FileTimes::new().set_created(created)) {
        warnings.push(format!(
            "The file's creation date could not be kept when it was rewritten ({e})."
        ));
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn created(_meta: &Metadata, _out: &File, _warnings: &mut Vec<String>) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::ffi::{OsStr, OsString};

    /// A file on a simulated filesystem: its attributes, and the error each
    /// call answers with instead (`None` = works).
    #[derive(Default)]
    struct Fake {
        attrs: RefCell<BTreeMap<OsString, Vec<u8>>>,
        list_err: Option<i32>,
        set_err: Option<i32>,
    }

    impl Xattrs for Fake {
        fn list(&self) -> io::Result<Vec<OsString>> {
            match self.list_err {
                Some(c) => Err(io::Error::from_raw_os_error(c)),
                None => Ok(self.attrs.borrow().keys().cloned().collect()),
            }
        }
        fn get(&self, name: &OsStr) -> io::Result<Option<Vec<u8>>> {
            Ok(self.attrs.borrow().get(name).cloned())
        }
        fn set(&self, name: &OsStr, value: &[u8]) -> io::Result<()> {
            if let Some(c) = self.set_err {
                return Err(io::Error::from_raw_os_error(c));
            }
            self.attrs.borrow_mut().insert(name.into(), value.to_vec());
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    const ENOTSUP: i32 = 95;
    #[cfg(target_vendor = "apple")]
    const ENOTSUP: i32 = 45;
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    const ENOTSUP: i32 = 0;
    const EPERM: i32 = 1;

    fn with(attrs: &[(&str, &[u8])]) -> Fake {
        let f = Fake::default();
        for (k, v) in attrs {
            f.attrs.borrow_mut().insert((*k).into(), v.to_vec());
        }
        f
    }

    #[test]
    fn every_attribute_is_copied() {
        let src = with(&[("user.a", b"1"), ("user.b", b"\x00\xff")]);
        let dst = Fake::default();
        let mut w = Vec::new();
        copy_xattrs(&src, &dst, &mut w);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(*dst.attrs.borrow(), *src.attrs.borrow());
    }

    /// A share without extended attributes cannot list any on the original:
    /// nothing to keep, no warning, no error (the save goes on).
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    #[test]
    fn a_filesystem_without_xattrs_is_not_a_loss() {
        let src = Fake {
            list_err: Some(ENOTSUP),
            ..Fake::default()
        };
        let mut w = Vec::new();
        copy_xattrs(&src, &Fake::default(), &mut w);
        assert!(w.is_empty(), "{w:?}");
    }

    /// The original has attributes but the new file cannot take them ("not
    /// supported" on the write, as some network shares answer): the save is
    /// not failed, and each lost attribute is named.
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    #[test]
    fn an_attribute_the_new_file_cannot_take_is_a_warning() {
        let src = with(&[("user.a", b"1"), ("user.b", b"2")]);
        let dst = Fake {
            set_err: Some(ENOTSUP),
            ..Fake::default()
        };
        let mut w = Vec::new();
        copy_xattrs(&src, &dst, &mut w);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("\"user.a\""), "{w:?}");
        assert!(w[1].contains("\"user.b\""), "{w:?}");
    }

    #[test]
    fn a_refused_write_of_a_value_already_there_is_fine() {
        let src = with(&[("security.label", b"x"), ("user.a", b"1")]);
        let dst = Fake {
            set_err: Some(EPERM),
            ..with(&[("security.label", b"x")])
        };
        let mut w = Vec::new();
        copy_xattrs(&src, &dst, &mut w);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("\"user.a\""), "{w:?}");
    }

    #[test]
    fn an_unreadable_list_is_one_warning() {
        let src = Fake {
            list_err: Some(EPERM),
            ..Fake::default()
        };
        let mut w = Vec::new();
        copy_xattrs(&src, &Fake::default(), &mut w);
        assert_eq!(w.len(), 1, "{w:?}");
    }
}
