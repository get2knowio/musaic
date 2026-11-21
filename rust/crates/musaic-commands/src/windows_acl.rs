//! The descriptor's owner-only ACL on Windows (contracts/descriptor.md).
//!
//! The workspace forbids `unsafe` and no maintained crate offers safe wrappers
//! for Windows security descriptors (research R13), so the ACL is set and read
//! with the system's own tools: `whoami.exe` for the current user's SID and
//! `icacls.exe` to replace the DACL and to save it as SDDL, which is the same
//! in every display language. Both run from `%SystemRoot%\System32`, without
//! a shell, with their arguments as a vector and without a console window.
//!
//! The DACL check itself is plain text processing and is compiled (and
//! unit-tested) on every platform.

/// LocalSystem, which can read any file anyway; tools that manage files
/// (backup, endpoint protection) sometimes add an ACE for it.
const SYSTEM_SID: &str = "S-1-5-18";

/// Accept a DACL, given in SDDL (`D:<flags>(ace)(ace)...`), only when it does
/// not inherit from the folder and every ACE that grants anything names the
/// current user or SYSTEM. Deny ACEs take nothing away from this and are
/// allowed. The `Err` is the reason, worded for `DESCRIPTOR_INVALID`.
pub fn check_dacl(sddl: &str, user_sid: &str) -> Result<(), String> {
    let other = || Err("its permissions allow other users".to_string());
    let Some(rest) = sddl.trim().strip_prefix("D:") else {
        return Err("its access control list cannot be read".into());
    };
    let flags_end = rest.find('(').unwrap_or(rest.len());
    let (flags, mut aces) = rest.split_at(flags_end);
    // A DACL with no ACE may be followed directly by a SACL.
    let flags = flags.split("S:").next().unwrap_or_default();
    if flags.contains("NO_ACCESS_CONTROL") {
        return other();
    }
    if !flags.contains('P') {
        return Err("it inherits permissions from its folder".into());
    }
    while let Some(body) = aces.strip_prefix('(') {
        let Some(end) = body.find(')') else {
            return Err("its access control list cannot be read".into());
        };
        let fields: Vec<&str> = body[..end].split(';').collect();
        if fields.len() < 6 {
            return Err("its access control list cannot be read".into());
        }
        let is_deny = matches!(fields[0], "D" | "OD" | "XD");
        if !is_deny && !trustee_is_allowed(fields[5], user_sid) {
            return other();
        }
        aces = &body[end + 1..];
    }
    if !(aces.is_empty() || aces.starts_with("S:")) {
        return Err("its access control list cannot be read".into());
    }
    Ok(())
}

fn trustee_is_allowed(trustee: &str, user_sid: &str) -> bool {
    trustee.eq_ignore_ascii_case(user_sid)
        || trustee == "SY"
        || trustee == SYSTEM_SID
        // SDDL names the built-in Administrator account (RID 500) "LA" rather
        // than by its SID.
        || (trustee == "LA" && user_sid.ends_with("-500"))
}

/// The SID string `whoami /user /fo csv /nh` prints: its last quoted field,
/// shaped `S-1-<digits and dashes>`.
pub fn parse_whoami_sid(out: &str) -> Option<String> {
    let line = out.lines().map(str::trim).find(|l| !l.is_empty())?;
    let sid = line.rsplit(',').next()?.trim().trim_matches('"');
    let tail = sid.strip_prefix("S-1-")?;
    let well_formed = !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit() || b == b'-');
    well_formed.then(|| sid.to_string())
}

/// The DACL line of an `icacls /save` file (UTF-16LE; the line before it
/// names the file). Plain UTF-8 is accepted too, in case a Windows release
/// writes that instead.
pub fn parse_saved_dacl(bytes: &[u8]) -> Option<String> {
    let dacl_line = |text: &str| {
        text.lines()
            .map(|l| l.trim_matches(|c: char| c == '\u{feff}' || c == '\0' || c.is_whitespace()))
            .find(|l| l.starts_with("D:"))
            .map(str::to_string)
    };
    let (pairs, rest) = bytes.as_chunks::<2>();
    let utf16 = rest
        .is_empty()
        .then(|| {
            pairs
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect::<Vec<u16>>()
        })
        .and_then(|units| String::from_utf16(&units).ok())
        .and_then(|t| dacl_line(&t));
    utf16.or_else(|| dacl_line(std::str::from_utf8(bytes).ok()?))
}

#[cfg(windows)]
mod os {
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    /// A program in `%SystemRoot%\System32`, by absolute path, so a
    /// look-alike earlier on `PATH` or in the working directory is never run.
    fn system32(exe: &str) -> io::Result<PathBuf> {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| io::Error::other("SystemRoot is not set to an absolute path"))?;
        Ok(root.join("System32").join(exe))
    }

    fn run(exe: &str, args: &[&OsStr]) -> io::Result<Vec<u8>> {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the desktop app is a GUI process, and a console
        // tool it starts would otherwise flash a window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = Command::new(system32(exe)?)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        if !out.status.success() {
            return Err(io::Error::other(format!("{exe} failed ({})", out.status)));
        }
        Ok(out.stdout)
    }

    /// The current user's SID, e.g. `S-1-5-21-…-1001`. It is the same in an
    /// elevated process, whose files are owned by Administrators instead, so
    /// the file owner is not used.
    pub fn current_user_sid() -> io::Result<String> {
        let out = run(
            "whoami.exe",
            &[
                OsStr::new("/user"),
                OsStr::new("/fo"),
                OsStr::new("csv"),
                OsStr::new("/nh"),
            ],
        )?;
        super::parse_whoami_sid(&String::from_utf8_lossy(&out))
            .ok_or_else(|| io::Error::other("whoami did not print a SID"))
    }

    /// The file's DACL as SDDL, through `icacls <path> /save <tmp>`.
    pub fn dacl_sddl(path: &Path) -> io::Result<String> {
        let mut name = [0u8; 8];
        getrandom::fill(&mut name).map_err(|e| io::Error::other(e.to_string()))?;
        let saved = std::env::temp_dir().join(format!(
            ".musaic-acl-{}.txt",
            crate::auth::hex_encode(&name)
        ));
        let result = run(
            "icacls.exe",
            &[
                path.as_os_str(),
                OsStr::new("/save"),
                saved.as_os_str(),
                OsStr::new("/Q"),
            ],
        )
        .and_then(|_| std::fs::read(&saved));
        let _ = std::fs::remove_file(&saved);
        super::parse_saved_dacl(&result?).ok_or_else(|| io::Error::other("icacls saved no DACL"))
    }

    /// Replace the file's DACL with one ACE, full control for the current
    /// user, and stop it inheriting from its folder; then read it back and
    /// check it as the reader will.
    pub fn restrict_to_current_user(path: &Path) -> io::Result<()> {
        let sid = current_user_sid()?;
        let mut grant = OsString::from("*");
        grant.push(&sid);
        grant.push(":F");
        run(
            "icacls.exe",
            &[
                path.as_os_str(),
                OsStr::new("/inheritance:r"),
                OsStr::new("/grant:r"),
                &grant,
                OsStr::new("/Q"),
            ],
        )?;
        super::check_dacl(&dacl_sddl(path)?, &sid).map_err(io::Error::other)
    }

    /// The reader's check: fails closed when the user or the ACL cannot be
    /// learned.
    pub fn check_file(path: &Path) -> Result<(), String> {
        let sid =
            current_user_sid().map_err(|_| "cannot tell which user is reading it".to_string())?;
        let sddl =
            dacl_sddl(path).map_err(|_| "its access control list cannot be read".to_string())?;
        super::check_dacl(&sddl, &sid)
    }
}

#[cfg(windows)]
pub use os::{check_file, current_user_sid, dacl_sddl, restrict_to_current_user};

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "S-1-5-21-1004336348-1177238915-682003330-1001";

    #[test]
    fn an_owner_only_protected_dacl_is_accepted() {
        assert_eq!(check_dacl(&format!("D:P(A;;FA;;;{ME})"), ME), Ok(()));
        assert_eq!(
            check_dacl(&format!("D:PAI(A;;FA;;;{ME})(A;;FA;;;SY)"), ME),
            Ok(())
        );
        assert_eq!(check_dacl("D:P(A;;FA;;;S-1-5-18)", ME), Ok(()));
        // A deny ACE for anyone takes nothing away.
        assert_eq!(
            check_dacl(&format!("D:P(D;;FA;;;WD)(A;;FA;;;{ME})"), ME),
            Ok(())
        );
        // Nobody at all, and a SACL after the DACL.
        assert_eq!(check_dacl("D:P", ME), Ok(()));
        assert_eq!(check_dacl(&format!("D:P(A;;FA;;;{ME})S:AI"), ME), Ok(()));
        assert_eq!(check_dacl("D:PS:AI", ME), Ok(()));
        assert_eq!(check_dacl("D:P(A;;FA;;;LA)", "S-1-5-21-1-2-3-500"), Ok(()));
    }

    #[test]
    fn other_trustees_are_refused() {
        for t in ["WD", "BU", "AU", "BA", "S-1-1-0", "CO", "OW", "LA"] {
            let why = check_dacl(&format!("D:P(A;;FA;;;{ME})(A;;FR;;;{t})"), ME).unwrap_err();
            assert!(why.contains("allow other users"), "{t}: {why}");
        }
        let another = "S-1-5-21-1004336348-1177238915-682003330-1002";
        assert!(check_dacl(&format!("D:P(A;;FA;;;{another})"), ME).is_err());
        assert!(check_dacl(&format!("D:P(XA;;FA;;;WD)(A;;FA;;;{ME})"), ME).is_err());
        assert!(check_dacl("D:NO_ACCESS_CONTROL", ME).is_err());
    }

    #[test]
    fn an_inherited_dacl_is_refused() {
        let why = check_dacl(&format!("D:AI(A;ID;FA;;;{ME})"), ME).unwrap_err();
        assert!(why.contains("inherits"));
        assert!(check_dacl(&format!("D:(A;;FA;;;{ME})"), ME).is_err());
    }

    #[test]
    fn malformed_sddl_is_refused() {
        for s in [
            "",
            "O:BA",
            &format!("D:P(A;;FA;;;{ME}"),
            "D:P(A;;FA)",
            &format!("D:P(A;;FA;;;{ME})junk"),
        ] {
            assert!(check_dacl(s, ME).is_err(), "{s}");
        }
    }

    #[test]
    fn whoami_csv_gives_the_sid() {
        assert_eq!(
            parse_whoami_sid(&format!("\"desktop-1\\paul\",\"{ME}\"\r\n")),
            Some(ME.to_string())
        );
        assert_eq!(parse_whoami_sid("\"a\\b\",\"not a sid\""), None);
        assert_eq!(parse_whoami_sid(""), None);
    }

    #[test]
    fn a_saved_acl_file_gives_the_dacl_line() {
        let text = format!("agent-instance.json\r\nD:P(A;;FA;;;{ME})\r\n");
        let bytes: Vec<u8> = std::iter::once(0xfeffu16)
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(parse_saved_dacl(&bytes), Some(format!("D:P(A;;FA;;;{ME})")));
        assert_eq!(parse_saved_dacl(b"odd"), None);
        assert_eq!(
            parse_saved_dacl(text.as_bytes()),
            Some(format!("D:P(A;;FA;;;{ME})"))
        );
    }
}
