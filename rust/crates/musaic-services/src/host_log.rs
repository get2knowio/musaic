//! The host's app log as the services see it: lines with a level and the
//! Python app's logger name as the target. The host's implementation decides
//! where they go (musaic-api's `AppLog` masks every secret value, then hands
//! the line to its sink). Unlike [`musaic_log`]'s process-wide facade this
//! is per host instance and has an INFO level.

/// A per-host line sink.
pub trait HostLog: Send + Sync {
    /// An INFO line.
    fn info(&self, target: &str, msg: &str);
    /// A WARNING line.
    fn warn(&self, target: &str, msg: &str);
    /// An ERROR line.
    fn error(&self, target: &str, msg: &str);
}
