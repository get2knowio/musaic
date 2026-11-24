//! Open a file with the program the system registers for it, and learn
//! whether one did.
//!
//! On macOS the opener plugin only starts `/usr/bin/open` and returns at once,
//! so it reports success even when no application handles the file type.
//! [`open_and_wait`] runs the helper itself (an argument array, no shell, no
//! inherited output) and reads its exit status: `open` exits non-zero when
//! LaunchServices finds no application for the file. The program is a
//! parameter so tests can use a stand-in and never open a real file.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// macOS's helper that hands a file to LaunchServices.
pub const MACOS_OPEN: &str = "/usr/bin/open";

/// How long the helper may take to answer. `open` returns once
/// LaunchServices has handed the file over, normally well under a second;
/// starting a large application can take a few.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// Fixed sentences: the helper's own output can name the file, and the
/// answer to the page never carries the generated file's path.
pub const COULD_NOT_START: &str = "the system could not open the extension file";
pub const NOT_HANDLED: &str = "no application on this computer opened the extension file";

/// How a run of the helper ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// The helper exited 0: an application took the file.
    Handled,
    /// The helper was still running at the deadline. Treated as handled by
    /// the caller: a slow application launch is not a failure, and removing
    /// the file under it would break an install that is under way.
    StillRunning,
}

/// Run `program path` and wait at most `timeout` for its exit status.
/// `Err` holds one of the fixed sentences above, never the helper's output.
pub fn open_and_wait(program: &Path, path: &Path, timeout: Duration) -> Result<Opened, String> {
    // The path is the operation's own absolute temporary file, so it cannot be
    // read as an option; refuse anything else rather than pass it on.
    if !path.is_absolute() {
        return Err(COULD_NOT_START.to_string());
    }
    let mut child = Command::new(program)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| COULD_NOT_START.to_string())?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(Opened::Handled),
            Ok(Some(_)) => return Err(NOT_HANDLED.to_string()),
            Ok(None) if Instant::now() >= deadline => {
                // Reap it whenever it ends; never kill an open in progress.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(Opened::StillRunning);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Err(COULD_NOT_START.to_string());
            }
        }
    }
}
