//! Quitting while work is running asks first.
//!
//! [`decide`] is the rule and [`QuitGate`] the bookkeeping around it (one
//! prompt at a time; a confirmed quit is not asked again); lib.rs only
//! shows the native dialog. What counts as running is musaic-api's
//! [`WorkSnapshot`](musaic_api::WorkSnapshot) (scan, apply, analyze,
//! restore, fingerprint pool, quick-fix-all, check runs, other writes, and
//! research an assistant has claimed).
//!
//! Which quit routes reach this (macOS, tao 0.37 / Tauri 2.12; details in
//! docs/desktop.md "Quitting"): the window's close button and Window →
//! Close (Cmd-W) as [`QuitRoute::CloseButton`]; the app menu's Quit item
//! and its Cmd-Q as [`QuitRoute::QuitMenu`] (our own item,
//! [`QUIT_MENU_ID`], because the predefined one sends `terminate:`); the
//! last window gone as [`QuitRoute::LastWindow`]. Dock → Quit, the quit
//! Apple Event and logout send `terminate:` and are not seen here.

use std::sync::atomic::{AtomicBool, Ordering};

use musaic_api::WorkSnapshot;

/// The id of the app menu's own Quit item (accelerator Cmd-Q) that
/// replaces Tauri's predefined `quit` item on macOS.
pub const QUIT_MENU_ID: &str = "musaic-quit";

/// Which user action asked to quit (selftest report and log wording).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitRoute {
    /// The window's close button, Window → Close (Cmd-W), `Window::close`.
    CloseButton,
    /// The app menu's Quit item or its Cmd-Q accelerator.
    QuitMenu,
    /// The last window is gone (`RunEvent::ExitRequested` without a code).
    LastWindow,
}

impl QuitRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            QuitRoute::CloseButton => "close-button",
            QuitRoute::QuitMenu => "quit-menu",
            QuitRoute::LastWindow => "last-window",
        }
    }
}

/// `RunEvent::ExitRequested`'s `code`: `None` when the last window closed
/// (a user action), `Some` from `AppHandle::exit` (ours: a signal, a
/// confirmed prompt, the Quit item after it decided).
pub fn exit_request_source(code: Option<i32>) -> QuitSource {
    match code {
        None => QuitSource::User,
        Some(_) => QuitSource::Programmatic,
    }
}

/// Where a quit request came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitSource {
    /// The user: closing the window, Cmd-Q / the app menu's Quit.
    User,
    /// The shell itself (`AppHandle::exit`): after a confirmed prompt, a
    /// signal (SIGTERM / SIGINT / SIGHUP / Ctrl-C) or a failed boot. Never
    /// asks: a signal has no one to answer, and a confirmed quit was asked.
    Programmatic,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuitDecision {
    Proceed,
    /// Hold the quit and ask with this message; quit only on "yes".
    Ask(String),
}

/// The decision for one quit request. `None` for the snapshot: the backend
/// has not started (or already stopped), so nothing can be running.
pub fn decide(source: QuitSource, work: Option<&WorkSnapshot>) -> QuitDecision {
    if source == QuitSource::Programmatic {
        return QuitDecision::Proceed;
    }
    match work.and_then(WorkSnapshot::quit_prompt) {
        Some(msg) => QuitDecision::Ask(msg),
        None => QuitDecision::Proceed,
    }
}

/// What one quit request came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuitOutcome {
    /// Nothing running (or a programmatic quit): go ahead, nobody asked.
    Proceed,
    /// The user confirmed quitting earlier: go ahead without asking again.
    AlreadyConfirmed,
    /// Asked with this message; the user said "Quit".
    Confirmed(String),
    /// Asked with this message; the user said "Keep running".
    Cancelled(String),
    /// A prompt is already showing: this request waits for that answer.
    PromptShowing,
}

impl QuitOutcome {
    /// Whether the quit (or close) goes ahead.
    pub fn proceeds(&self) -> bool {
        matches!(
            self,
            QuitOutcome::Proceed | QuitOutcome::AlreadyConfirmed | QuitOutcome::Confirmed(_)
        )
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            QuitOutcome::Proceed => "proceed",
            QuitOutcome::AlreadyConfirmed => "already-confirmed",
            QuitOutcome::Confirmed(_) => "confirmed",
            QuitOutcome::Cancelled(_) => "cancelled",
            QuitOutcome::PromptShowing => "prompt-showing",
        }
    }
}

/// One process's quit state: at most one prompt at a time, and once the
/// user confirmed, the closes and exit requests that follow (window
/// close → last window gone → exit) go ahead without asking again.
#[derive(Debug, Default)]
pub struct QuitGate {
    asking: AtomicBool,
    confirmed: AtomicBool,
}

impl QuitGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide one quit request. `work` is read only when it matters (not
    /// after a confirmation); `ask` shows the prompt with the message and
    /// returns `true` for "Quit".
    pub fn request(
        &self,
        source: QuitSource,
        work: impl FnOnce() -> Option<WorkSnapshot>,
        ask: impl FnOnce(&str) -> bool,
    ) -> QuitOutcome {
        if source == QuitSource::Programmatic {
            return QuitOutcome::Proceed;
        }
        if self.confirmed.load(Ordering::SeqCst) {
            return QuitOutcome::AlreadyConfirmed;
        }
        let QuitDecision::Ask(message) = decide(source, work().as_ref()) else {
            return QuitOutcome::Proceed;
        };
        if self.asking.swap(true, Ordering::SeqCst) {
            return QuitOutcome::PromptShowing;
        }
        let yes = ask(&message);
        if yes {
            self.confirmed.store(true, Ordering::SeqCst);
        }
        self.asking.store(false, Ordering::SeqCst);
        if yes {
            QuitOutcome::Confirmed(message)
        } else {
            QuitOutcome::Cancelled(message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use musaic_api::WorkKind;

    // Idle, or no backend yet: quit at once.
    #[test]
    fn idle_quits() {
        assert_eq!(decide(QuitSource::User, None), QuitDecision::Proceed);
        assert_eq!(
            decide(QuitSource::User, Some(&WorkSnapshot::default())),
            QuitDecision::Proceed
        );
    }

    // A user quit while work runs asks; a programmatic one
    // (signal, confirmed prompt) never does.
    #[test]
    fn busy_asks_only_the_user() {
        let busy = WorkSnapshot {
            kinds: vec![(WorkKind::Apply, 1)],
            ..Default::default()
        };
        match decide(QuitSource::User, Some(&busy)) {
            QuitDecision::Ask(m) => assert!(m.contains("applying changes"), "{m}"),
            d => panic!("{d:?}"),
        }
        assert_eq!(
            decide(QuitSource::Programmatic, Some(&busy)),
            QuitDecision::Proceed
        );
    }

    fn busy() -> WorkSnapshot {
        WorkSnapshot {
            kinds: vec![(WorkKind::Apply, 1)],
            ..Default::default()
        }
    }

    // An exit request without a code is the
    // last window gone (a user action); one with a code is our own exit.
    #[test]
    fn exit_request_source_is_read_from_the_code() {
        assert_eq!(exit_request_source(None), QuitSource::User);
        assert_eq!(exit_request_source(Some(0)), QuitSource::Programmatic);
        assert_eq!(exit_request_source(Some(1)), QuitSource::Programmatic);
    }

    // Idle quits at once without asking (the work is not even
    // needed after a confirmation).
    #[test]
    fn gate_idle_proceeds_without_asking() {
        let g = QuitGate::new();
        let o = g.request(QuitSource::User, || None, |_| panic!("must not ask"));
        assert_eq!(o, QuitOutcome::Proceed);
        let o = g.request(
            QuitSource::User,
            || Some(WorkSnapshot::default()),
            |_| panic!("must not ask"),
        );
        assert_eq!(o, QuitOutcome::Proceed);
        assert!(o.proceeds());
    }

    // Busy asks; "Keep running" holds the quit and the next
    // request asks again; "Quit" lets this and every later request
    // (window close, then last window gone) through without a second
    // prompt.
    #[test]
    fn gate_busy_asks_keep_then_quit() {
        let g = QuitGate::new();
        let o = g.request(
            QuitSource::User,
            || Some(busy()),
            |m| {
                assert!(m.contains("applying changes"), "{m}");
                false
            },
        );
        assert!(matches!(o, QuitOutcome::Cancelled(_)), "{o:?}");
        assert!(!o.proceeds());
        let mut asked = 0;
        let o = g.request(
            QuitSource::User,
            || Some(busy()),
            |_| {
                asked += 1;
                true
            },
        );
        assert_eq!(asked, 1);
        assert!(matches!(o, QuitOutcome::Confirmed(_)), "{o:?}");
        assert!(o.proceeds());
        let o = g.request(
            QuitSource::User,
            || panic!("work not read after a confirmation"),
            |_| panic!("must not ask twice"),
        );
        assert_eq!(o, QuitOutcome::AlreadyConfirmed);
        assert!(o.proceeds());
    }

    // A programmatic quit (signal, our own exit) never asks, busy
    // or not.
    #[test]
    fn gate_programmatic_never_asks() {
        let g = QuitGate::new();
        let o = g.request(
            QuitSource::Programmatic,
            || Some(busy()),
            |_| panic!("must not ask"),
        );
        assert_eq!(o, QuitOutcome::Proceed);
    }

    // While a prompt shows, a second request (another Cmd-Q, the
    // close button) neither stacks a prompt nor quits.
    #[test]
    fn gate_one_prompt_at_a_time() {
        let g = QuitGate::new();
        let o = g.request(
            QuitSource::User,
            || Some(busy()),
            |_| {
                let inner = g.request(
                    QuitSource::User,
                    || Some(busy()),
                    |_| panic!("no second prompt"),
                );
                assert_eq!(inner, QuitOutcome::PromptShowing);
                assert!(!inner.proceeds());
                false
            },
        );
        assert!(matches!(o, QuitOutcome::Cancelled(_)));
        // The prompt closed: the next request asks again.
        let mut asked = false;
        g.request(
            QuitSource::User,
            || Some(busy()),
            |_| {
                asked = true;
                false
            },
        );
        assert!(asked);
    }
}
