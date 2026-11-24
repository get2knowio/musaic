//! Quitting while work is running asks for
//! confirmation. This module holds the decision, so the desktop shell's
//! native dialog stays a thin caller and the rule is testable without a
//! window.
//!
//! "Work running" is read from what the server already tracks:
//!
//! * the restore gate's labelled work guards ([`musaic_services::gate::WorkKind`]):
//!   scans, applies (and dry runs), analyses, check runs, quick-fix-all,
//!   and every other writing request;
//! * a database restore holding the gate;
//! * the scan service's state (a scan started by any route);
//! * the fingerprint pool's state.
//!
//! The Python app has no counterpart (its Electron shell kills the sidecar
//! after a SIGTERM grace period, whatever runs).

use musaic_services::gate::WorkKind;

/// What is running at one moment ([`crate::App::work_snapshot`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkSnapshot {
    /// Work guards held, by kind (only kinds with at least one).
    pub kinds: Vec<(WorkKind, usize)>,
    /// A database restore runs.
    pub restoring: bool,
    /// The scan service reports a scan in progress.
    pub scan_running: bool,
    /// The fingerprint pool is working through the library.
    pub pool_running: bool,
    /// Research leased to an external assistant, independent of local work guards.
    pub external_claims: usize,
}

/// The title of the confirmation dialog.
pub const QUIT_TITLE: &str = "Quit Musaic?";

impl WorkSnapshot {
    /// The things running, as short phrases, in a fixed order (no repeats).
    pub fn running(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        if self.restoring {
            out.push("a database restore");
        }
        let has = |k: WorkKind| self.kinds.iter().any(|(x, n)| *x == k && *n > 0);
        for k in WorkKind::ALL {
            let on = has(k) || (k == WorkKind::Scan && self.scan_running);
            if on {
                out.push(k.describe());
            }
        }
        if self.pool_running {
            out.push("fingerprinting the library");
        }
        out
    }

    /// Nothing running: quit without asking.
    pub fn is_idle(&self) -> bool {
        self.running().is_empty() && self.external_claims == 0
    }

    /// The confirmation message when quitting should ask first, else `None`.
    pub fn quit_prompt(&self) -> Option<String> {
        let running = self.running();
        if self.external_claims > 0 {
            let research = "An assistant has claimed research. It can send updates again when Musaic reopens. Its claim can expire while Musaic is closed.";
            return Some(if running.is_empty() {
                format!("{research} Quit anyway?")
            } else {
                format!(
                    "Quitting stops local work ({}). {research} Quit anyway?",
                    running.join(", ")
                )
            });
        }
        if running.is_empty() {
            return None;
        }
        let list = match running.as_slice() {
            [one] => (*one).to_string(),
            [init @ .., last] => format!("{} and {last}", init.join(", ")),
            [] => unreachable!(),
        };
        Some(format!(
            "Musaic is still busy with {list}. Quitting now stops it before it finishes. Quit anyway?"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Idle quits without asking.
    #[test]
    fn idle_does_not_ask() {
        let s = WorkSnapshot::default();
        assert!(s.is_idle());
        assert_eq!(s.quit_prompt(), None);
        // A kind with a zero count is not running.
        let s = WorkSnapshot {
            kinds: vec![(WorkKind::Apply, 0)],
            ..Default::default()
        };
        assert_eq!(s.quit_prompt(), None);
    }

    // Every source of running work asks, and names it.
    #[test]
    fn each_source_asks() {
        let cases: Vec<(WorkSnapshot, &str)> = vec![
            (
                WorkSnapshot {
                    restoring: true,
                    ..Default::default()
                },
                "a database restore",
            ),
            (
                WorkSnapshot {
                    scan_running: true,
                    ..Default::default()
                },
                "a library scan",
            ),
            (
                WorkSnapshot {
                    pool_running: true,
                    ..Default::default()
                },
                "fingerprinting the library",
            ),
        ];
        let mut all = cases;
        for k in WorkKind::ALL {
            all.push((
                WorkSnapshot {
                    kinds: vec![(k, 1)],
                    ..Default::default()
                },
                k.describe(),
            ));
        }
        for (s, phrase) in all {
            let p = s
                .quit_prompt()
                .unwrap_or_else(|| panic!("{s:?} should ask"));
            assert!(p.contains(phrase), "{p}");
            assert!(p.ends_with("Quit anyway?"), "{p}");
        }
    }

    // A scan counted by both the gate and the scan service is
    // named once; several kinds are listed in a fixed order.
    #[test]
    fn message_lists_each_once() {
        let s = WorkSnapshot {
            kinds: vec![(WorkKind::Scan, 1), (WorkKind::QuickFixAll, 2)],
            restoring: false,
            scan_running: true,
            pool_running: true,
            external_claims: 0,
        };
        let p = s.quit_prompt().unwrap();
        assert_eq!(
            p,
            "Musaic is still busy with a library scan, a bulk quick fix and fingerprinting the library. Quitting now stops it before it finishes. Quit anyway?"
        );
        assert!(!p.contains("AI") && !p.contains("Claude"), "{p}");
    }
}
