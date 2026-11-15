//! A logging facade for the crates below musaic-api.
//!
//! The lower crates (musaic-scan, musaic-resolve, musaic-net, musaic-tags,
//! musaic-fsops) cannot depend on musaic-api, whose `logging` module owns
//! the server log (masking, rotation, `server.log`). Where the Python app logs a
//! warning or an error from the corresponding Python module, the Rust crate
//! calls [`warning`] / [`error`] here with the Python app's logger name as the
//! target (`musaic.scanner`, `musaic.mb_lookup`, ...). The host installs the
//! sink once ([`set_sink`]; musaic-api routes it into its server log, which
//! masks secret values). Without a sink, lines go to stderr.
//!
//! Tests capture what the current thread logs with [`capture`], which takes
//! precedence over the process sink, so parallel tests do not see each
//! other's lines.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

/// The levels the lower crates use. `Debug` is for per-item detail behind a
/// summary line; the host's log drops it below its threshold, and without a
/// sink it is not printed at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Warning,
    Error,
}

impl Level {
    /// Python's level name.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
        }
    }
}

/// Where lines go: `(level, target, message)`.
pub type Sink = Arc<dyn Fn(Level, &str, &str) + Send + Sync>;

/// One captured line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub level: Level,
    pub target: String,
    pub message: String,
}

static SINK: OnceLock<Sink> = OnceLock::new();

thread_local! {
    static CAPTURE: RefCell<Option<Vec<Record>>> = const { RefCell::new(None) };
}

/// Install the process-wide sink. The first call wins; returns whether this
/// one did.
pub fn set_sink(sink: Sink) -> bool {
    SINK.set(sink).is_ok()
}

/// Whether a process-wide sink is installed.
pub fn has_sink() -> bool {
    SINK.get().is_some()
}

/// Log one line.
pub fn log(level: Level, target: &str, message: &str) {
    let captured = CAPTURE.with(|c| {
        let mut c = c.borrow_mut();
        match c.as_mut() {
            Some(v) => {
                v.push(Record {
                    level,
                    target: target.to_string(),
                    message: message.to_string(),
                });
                true
            }
            None => false,
        }
    });
    if captured {
        return;
    }
    match SINK.get() {
        Some(s) => s(level, target, message),
        None if level == Level::Debug => {}
        None => eprintln!("[{}] {target}: {message}", level.as_str()),
    }
}

/// `logger.debug(...)`.
pub fn debug(target: &str, message: &str) {
    log(Level::Debug, target, message);
}

/// `logger.warning(...)`.
pub fn warning(target: &str, message: &str) {
    log(Level::Warning, target, message);
}

/// `logger.error(...)` / `logger.exception(...)`.
pub fn error(target: &str, message: &str) {
    log(Level::Error, target, message);
}

/// Run `f` and return what it logged on this thread (lines logged on other
/// threads go to the process sink as usual). Nested captures are not
/// supported: the inner one takes the lines.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<Record>) {
    let prev = CAPTURE.with(|c| c.borrow_mut().replace(Vec::new()));
    struct Restore(Option<Option<Vec<Record>>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(prev) = self.0.take() {
                CAPTURE.with(|c| *c.borrow_mut() = prev);
            }
        }
    }
    let mut restore = Restore(Some(prev));
    let out = f();
    let lines = CAPTURE.with(|c| c.borrow_mut().take()).unwrap_or_default();
    if let Some(prev) = restore.0.take() {
        CAPTURE.with(|c| *c.borrow_mut() = prev);
    }
    (out, lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_takes_this_threads_lines() {
        let ((), lines) = capture(|| {
            warning("musaic.scanner", "a");
            error("musaic.tags", "b");
        });
        assert_eq!(
            lines,
            vec![
                Record {
                    level: Level::Warning,
                    target: "musaic.scanner".into(),
                    message: "a".into()
                },
                Record {
                    level: Level::Error,
                    target: "musaic.tags".into(),
                    message: "b".into()
                },
            ]
        );
        // Restored: a second capture starts empty.
        let ((), again) = capture(|| {});
        assert!(again.is_empty());
    }

    #[test]
    fn capture_survives_a_panic() {
        let r = std::panic::catch_unwind(|| capture(|| panic!("x")));
        assert!(r.is_err());
        let ((), lines) = capture(|| warning("t", "m"));
        assert_eq!(lines.len(), 1);
    }
}
