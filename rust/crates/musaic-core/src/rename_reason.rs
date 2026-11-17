//! Why a rename did not happen, in words a user can act on.
//!
//! The rename code keeps the raw error text (an operating-system message, a
//! quoted name) for the log and for the fields that already carry it. What the
//! user is shown is built from a [`Reason`]: a stable code a client can branch
//! on, plus a cause and an advice sentence. Nothing here repeats the system's
//! own wording, so it reads the same on every platform.

use std::io;

/// The class of a rename failure or refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    PermissionDenied,
    /// Another program has the file open, or it is locked.
    InUse,
    DiskFull,
    NameTooLong,
    ReadOnly,
    /// The file or folder is no longer where it was.
    NotFound,
    /// The name is empty, holds a path separator, or the volume refuses it.
    NameNotAllowed,
    AlreadyExists,
    /// The volume took the rename but kept the old spelling (no case kept).
    NotKept,
    /// This file was not attempted or was undone because another failed.
    BatchStopped,
    /// Two files would get the same new name.
    DuplicateTarget,
    NoLibrary,
    AlbumNotFound,
    AlbumMissing,
    FolderMissing,
    /// The database update failed and the folder was put back.
    LibraryUpdateFailed,
    /// The rename happened but the result could not be read back to check.
    Unverified,
    Other,
}

/// `ENAMETOOLONG`.
#[cfg(target_os = "macos")]
const ENAMETOOLONG: i32 = 63;
#[cfg(not(target_os = "macos"))]
const ENAMETOOLONG: i32 = 36;

impl Reason {
    /// The stable machine-readable code.
    pub fn code(self) -> &'static str {
        match self {
            Reason::PermissionDenied => "permission_denied",
            Reason::InUse => "in_use",
            Reason::DiskFull => "disk_full",
            Reason::NameTooLong => "name_too_long",
            Reason::ReadOnly => "read_only",
            Reason::NotFound => "not_found",
            Reason::NameNotAllowed => "name_not_allowed",
            Reason::AlreadyExists => "already_exists",
            Reason::NotKept => "case_not_kept",
            Reason::BatchStopped => "batch_stopped",
            Reason::DuplicateTarget => "duplicate_target",
            Reason::NoLibrary => "no_library",
            Reason::AlbumNotFound => "album_not_found",
            Reason::AlbumMissing => "album_missing",
            Reason::FolderMissing => "folder_missing",
            Reason::LibraryUpdateFailed => "library_update_failed",
            Reason::Unverified => "unverified",
            Reason::Other => "other",
        }
    }

    /// Classify an I/O error from a rename. Only the kind and the OS error
    /// number are read; the text is never used.
    pub fn from_io(e: &io::Error) -> Reason {
        use io::ErrorKind as K;
        // The kinds for "name too long" and "invalid name" are not stable
        // in std, so those two are told by OS error number.
        if let Some(n) = e.raw_os_error() {
            if cfg!(windows) {
                match n {
                    32 | 33 => return Reason::InUse,   // sharing / lock violation
                    19 => return Reason::ReadOnly,     // write protected
                    206 => return Reason::NameTooLong, // filename exceeds range
                    123 | 161 => return Reason::NameNotAllowed, // invalid name / path
                    _ => {}
                }
            } else if n == ENAMETOOLONG {
                return Reason::NameTooLong;
            }
        }
        match e.kind() {
            K::PermissionDenied => Reason::PermissionDenied,
            K::ResourceBusy | K::ExecutableFileBusy => Reason::InUse,
            K::StorageFull | K::QuotaExceeded => Reason::DiskFull,
            K::ReadOnlyFilesystem => Reason::ReadOnly,
            K::NotFound => Reason::NotFound,
            K::AlreadyExists | K::DirectoryNotEmpty => Reason::AlreadyExists,
            _ => Reason::Other,
        }
    }

    /// Completes "... because ...": what went wrong.
    pub fn cause(self) -> &'static str {
        match self {
            Reason::PermissionDenied => "Musaic isn't allowed to change it",
            Reason::InUse => "another program is using it",
            Reason::DiskFull => "the disk is full",
            Reason::NameTooLong => "the new name is too long for this volume",
            Reason::ReadOnly => "the volume is read-only",
            Reason::NotFound => "it can no longer be found",
            Reason::NameNotAllowed => "the volume doesn't allow that name",
            Reason::AlreadyExists => "a file with that name already exists",
            Reason::NotKept => "the volume doesn't keep upper and lower case in names",
            Reason::BatchStopped => "renaming another file in the same step failed first",
            Reason::DuplicateTarget => "another file would get the same name",
            Reason::NoLibrary => "no music folder is set up",
            Reason::AlbumNotFound => "the album is no longer in the library",
            Reason::AlbumMissing => "the album's folder is missing from disk",
            Reason::FolderMissing => "the album's folder can't be found on disk",
            Reason::LibraryUpdateFailed => "Musaic couldn't update its library records",
            Reason::Unverified => "the result could not be checked",
            Reason::Other => "of an unexpected problem",
        }
    }

    /// What the user can do about it.
    pub fn advice(self) -> &'static str {
        match self {
            Reason::PermissionDenied => {
                "Check that you can change files in this folder, then try again."
            }
            Reason::InUse => {
                "Close any program that has it open (a music player or a sync tool, for example), then try again."
            }
            Reason::DiskFull => "Free some space on it, then try again.",
            Reason::NameTooLong => {
                "Use a shorter name (for example in your filename template), then try again."
            }
            Reason::ReadOnly => "Make the volume writable, then try again.",
            Reason::NotFound => "Rescan the library, then check again.",
            Reason::NameNotAllowed => {
                "Remove characters the volume doesn't accept (for example in your filename template), then try again."
            }
            Reason::AlreadyExists => "Rename or move that file, then try again.",
            Reason::NotKept => "Choose a name that differs by more than upper and lower case.",
            Reason::BatchStopped => "Fix that file, then try again.",
            Reason::DuplicateTarget => "Change the template so every file gets its own name.",
            Reason::NoLibrary => "Choose a music folder in Settings, then try again.",
            Reason::AlbumNotFound | Reason::AlbumMissing | Reason::FolderMissing => {
                "Rescan the library, then check again."
            }
            Reason::LibraryUpdateFailed => "Try again.",
            Reason::Unverified => "Rescan the library to be sure.",
            Reason::Other => "Try again. If it keeps happening, the details are in Musaic's log.",
        }
    }
}

/// A name inside a sentence, in typographic quotes (never a repr).
pub fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
}

/// `“a”, “b”, “c” and 2 more`: at most `max` names.
pub fn quoted_list<S: AsRef<str>>(names: &[S], max: usize) -> String {
    let mut shown: Vec<String> = names.iter().take(max).map(|n| quoted(n.as_ref())).collect();
    let more = names.len().saturating_sub(max);
    if more > 0 {
        return format!("{} and {more} more", shown.join(", "));
    }
    match shown.pop() {
        Some(last) if shown.is_empty() => last,
        Some(last) => format!("{} and {last}", shown.join(", ")),
        None => String::new(),
    }
}

/// `<subject> couldn't be renamed because <cause>. <advice>`
pub fn couldnt_rename(subject: &str, reason: Reason) -> String {
    format!(
        "{subject} couldn't be renamed because {}. {}",
        reason.cause(),
        reason.advice()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use io::ErrorKind as K;

    #[test]
    fn kinds_map_to_codes() {
        let code = |k: K| Reason::from_io(&io::Error::from(k)).code();
        assert_eq!(code(K::PermissionDenied), "permission_denied");
        assert_eq!(code(K::ResourceBusy), "in_use");
        assert_eq!(code(K::StorageFull), "disk_full");
        assert_eq!(code(K::QuotaExceeded), "disk_full");
        assert_eq!(code(K::ReadOnlyFilesystem), "read_only");
        assert_eq!(code(K::NotFound), "not_found");
        assert_eq!(code(K::AlreadyExists), "already_exists");
        assert_eq!(code(K::TimedOut), "other");
    }

    #[cfg(unix)]
    #[test]
    fn os_numbers_map_to_codes() {
        let code = |n: i32| Reason::from_io(&io::Error::from_raw_os_error(n)).code();
        assert_eq!(code(13), "permission_denied"); // EACCES
        assert_eq!(code(28), "disk_full"); // ENOSPC
        assert_eq!(code(30), "read_only"); // EROFS
        assert_eq!(code(ENAMETOOLONG), "name_too_long");
    }

    #[cfg(windows)]
    #[test]
    fn windows_numbers_map_to_codes() {
        let code = |n: i32| Reason::from_io(&io::Error::from_raw_os_error(n)).code();
        assert_eq!(code(32), "in_use");
        assert_eq!(code(33), "in_use");
        assert_eq!(code(19), "read_only");
        assert_eq!(code(206), "name_too_long");
        assert_eq!(code(123), "name_not_allowed");
    }

    #[test]
    fn lists_and_sentences_have_no_raw_text() {
        assert_eq!(quoted_list(&["a"], 3), "\u{201c}a\u{201d}");
        assert_eq!(
            quoted_list(&["a", "b"], 3),
            "\u{201c}a\u{201d} and \u{201c}b\u{201d}"
        );
        assert_eq!(
            quoted_list(&["a", "b", "c", "d", "e"], 3),
            "\u{201c}a\u{201d}, \u{201c}b\u{201d}, \u{201c}c\u{201d} and 2 more"
        );
        let s = couldnt_rename(&quoted("x.flac"), Reason::ReadOnly);
        assert_eq!(
            s,
            "\u{201c}x.flac\u{201d} couldn't be renamed because the volume is read-only. \
             Make the volume writable, then try again."
        );
    }
}
