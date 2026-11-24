//! The agent bridge's in-memory state and its helpers (research R4, R9).
//!
//! [`AgentHub`] holds what lives only for one app run: the instance id, the
//! single-use nonces of the handshake, the access tokens, the lockout after
//! failed exchanges, and cancellation flags for live workers. Pairings,
//! approvals, operations and Task mappings are persisted in the database.
//! Time comes from the app's wall clock.
//!
//! Nothing in here writes to the library, takes the work guard or touches the
//! network; the operations in `ops::agent` do, through the services.

pub mod claude_desktop;
pub(crate) mod delegation;
pub(crate) mod events;
pub mod hosts;
pub mod hub_types;
pub mod operations;
pub mod proposal;
pub mod read;
pub mod run;
pub mod skill_archive;
pub mod tasks;
pub mod undo;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use musaic_commands::PermissionKind;
use musaic_commands::auth::{NONCE_TTL_SECS, TOKEN_TTL_SECS, ct_eq, random_hex};
use musaic_commands::tools::operation::OperationState;

pub use hub_types::Principal;

use crate::app::WallClock;

/// The most outstanding handshake nonces; the oldest is dropped past it.
pub const MAX_NONCES: usize = 1024;
/// Failed exchanges within [`FAILURE_WINDOW`] that lock exchanges.
pub const FAILURES_BEFORE_LOCKOUT: usize = 5;
pub const FAILURE_WINDOW: Duration = Duration::from_secs(60);
pub const FIRST_LOCKOUT: Duration = Duration::from_secs(60);
pub const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);
/// A waiting poll this recent still counts as an active subscription.
pub const SUBSCRIPTION_WINDOW: Duration = Duration::from_secs(90);
const ANNOUNCE_EVERY: Duration = Duration::from_secs(5);

struct Token {
    value: String,
    principal: Principal,
    expires_at: SystemTime,
}

#[derive(Default)]
struct Lockout {
    failures: VecDeque<SystemTime>,
    locked_until: Option<SystemTime>,
    /// Lockouts since the last success (each doubles the next one).
    count: u32,
}

#[derive(Default)]
struct State {
    nonces: HashMap<String, SystemTime>,
    tokens: Vec<Token>,
    lockout: Lockout,
    cancellation: HashMap<String, Arc<AtomicBool>>,
    /// The latest authenticated request of each pairing (diagnostics).
    last_seen: HashMap<String, SystemTime>,
    /// The `musaic-cli` version each pairing's live session reported. Memory
    /// only: it describes a running connection, so nothing stores it.
    cli_versions: HashMap<String, String>,
}

type Waiting = Arc<Mutex<HashMap<String, (usize, SystemTime)>>>;

/// The last diagnostics refresh published for a pairing.
#[derive(Clone, Copy)]
struct Announced {
    at: SystemTime,
    /// The rate limit refused a refresh since: one is still owed.
    owed: bool,
    /// Whether the subscription was active when it was published.
    listening: bool,
}

/// The per-run agent state.
pub struct AgentHub {
    instance_id: String,
    clock: WallClock,
    state: Mutex<State>,
    /// The `agent-op-*` threads, joined at shutdown.
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// A counter that parked event polls wait on: the delegation publish,
    /// a revocation and shutdown bump it. The table is the truth; this only
    /// says "look again".
    wake: Mutex<u64>,
    wake_cv: Condvar,
    /// Parked event polls per pairing and when one last parked.
    waiting: Waiting,
    /// When a diagnostics refresh was last published, per pairing.
    announced: Mutex<HashMap<String, Announced>>,
}

impl AgentHub {
    pub fn new(clock: WallClock) -> Self {
        Self {
            instance_id: format!("i_{}", random_hex(8)),
            clock,
            state: Mutex::new(State::default()),
            workers: Mutex::new(Vec::new()),
            wake: Mutex::new(0),
            wake_cv: Condvar::new(),
            waiting: Waiting::default(),
            announced: Mutex::new(HashMap::new()),
        }
    }

    /// Wake every parked event poll.
    pub fn notify_events(&self) {
        *self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        self.wake_cv.notify_all();
    }

    /// The wake counter now; pass it to [`Self::wait_events`] after looking.
    pub fn events_generation(&self) -> u64 {
        *self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Park until the counter moves past `seen` or `max` passes.
    pub fn wait_events(&self, seen: u64, max: Duration) {
        let guard = self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = self
            .wake_cv
            .wait_timeout_while(guard, max, |g| *g == seen)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }

    /// Count a parked poll for `pairing`; `None` past `max` of them.
    pub fn park_waiter(&self, pairing: &str, max: usize) -> Option<WaiterGuard> {
        let now = self.now();
        let mut map = self
            .waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(pairing.to_string()).or_insert((0, now));
        if entry.0 >= max {
            return None;
        }
        *entry = (entry.0 + 1, now);
        Some(WaiterGuard {
            waiting: Arc::clone(&self.waiting),
            pairing: pairing.to_string(),
        })
    }

    /// Records an events poll that did not park. A host that only polls
    /// (the server asks it back every 30 seconds) is listening too.
    pub fn note_poll(&self, pairing: &str) {
        let now = self.now();
        let mut map = self
            .waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(pairing.to_string()).or_insert((0, now)).1 = now;
    }

    /// A poll is parked now, or the pairing polled within the last 90 seconds.
    pub fn subscription_active(&self, pairing: &str) -> bool {
        let (n, at) = self.waiting_of(pairing);
        n > 0
            || at.is_some_and(|t| {
                self.now().duration_since(t).unwrap_or_default() < SUBSCRIPTION_WINDOW
            })
    }

    /// Whether a diagnostics refresh may be published for `pairing` now (one
    /// per pairing every five seconds). A refused one is owed, and
    /// [`Self::due_announces`] hands it out later, so a change made inside
    /// the window (the host's report or a first parked poll right after the
    /// token exchange) still reaches the desktop.
    pub fn allow_announce(&self, pairing: &str) -> bool {
        let now = self.now();
        let listening = self.subscription_active(pairing);
        let mut map = self
            .announced
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(a) = map.get_mut(pairing)
            && now.duration_since(a.at).unwrap_or_default() < ANNOUNCE_EVERY
        {
            a.owed = true;
            return false;
        }
        map.insert(
            pairing.to_string(),
            Announced {
                at: now,
                owed: false,
                listening,
            },
        );
        true
    }

    /// The pairings owed a diagnostics refresh now: one the rate limit held
    /// back, or one whose subscription began or lapsed since its last refresh
    /// (a lapse is the end of a window, which nothing else announces). Each
    /// returned pairing counts as announced.
    pub fn due_announces(&self) -> Vec<String> {
        let now = self.now();
        let pairings: Vec<String> = self
            .announced
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        let mut due = Vec::new();
        for pairing in pairings {
            let listening = self.subscription_active(&pairing);
            let mut map = self
                .announced
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(a) = map.get_mut(&pairing) else {
                continue;
            };
            if now.duration_since(a.at).unwrap_or_default() >= ANNOUNCE_EVERY
                && (a.owed || a.listening != listening)
            {
                *a = Announced {
                    at: now,
                    owed: false,
                    listening,
                };
                due.push(pairing);
            }
        }
        due
    }

    /// Parked polls of a pairing now, and when one last parked.
    pub fn waiting_of(&self, pairing: &str) -> (usize, Option<SystemTime>) {
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(pairing)
            .map_or((0, None), |(n, t)| (*n, Some(*t)))
    }

    /// When the pairing last made an authenticated request.
    pub fn last_seen(&self, pairing: &str) -> Option<SystemTime> {
        self.lock().last_seen.get(pairing).copied()
    }

    /// Keep the program version a pairing's session reported; true when it is
    /// new or different. An empty text clears it.
    pub fn set_cli_version(&self, pairing: &str, version: &str) -> bool {
        let mut s = self.lock();
        if version.is_empty() {
            return s.cli_versions.remove(pairing).is_some();
        }
        s.cli_versions
            .insert(pairing.to_string(), version.to_string())
            .as_deref()
            != Some(version)
    }

    /// The program version a pairing's session last reported.
    pub fn cli_version(&self, pairing: &str) -> Option<String> {
        self.lock().cli_versions.get(pairing).cloned()
    }

    /// Forget what is kept in memory about a pairing (revoke).
    pub fn forget_pairing(&self, pairing: &str) {
        self.lock().last_seen.remove(pairing);
        self.lock().cli_versions.remove(pairing);
        self.waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(pairing);
        self.announced
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(pairing);
    }

    /// Remember a worker thread so shutdown can wait for it.
    pub fn keep_worker(&self, handle: std::thread::JoinHandle<()>) {
        let mut w = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        w.retain(|h| !h.is_finished());
        w.push(handle);
    }

    /// Ask every unfinished operation to stop at its next item boundary, then
    /// wait up to `wait` for the worker threads to end. Returns whether they
    /// all did.
    pub fn stop_workers(&self, wait: Duration) -> bool {
        for flag in self.lock().cancellation.values() {
            flag.store(true, Ordering::SeqCst);
        }
        let deadline = std::time::Instant::now() + wait;
        loop {
            let mut w = self
                .workers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            w.retain(|h| !h.is_finished());
            if w.is_empty() {
                return true;
            }
            drop(w);
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn now(&self) -> SystemTime {
        (self.clock)()
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    // ------------------------------------------------------------- nonces

    /// A new single-use server nonce, valid for 60 seconds.
    pub fn issue_nonce(&self) -> String {
        let now = self.now();
        let nonce = random_hex(16);
        let mut s = self.lock();
        s.nonces.retain(|_, exp| *exp > now);
        // `hello` is unauthenticated: a burst of calls must not grow the map
        // without bound, so past the cap the nonce closest to expiry goes.
        if s.nonces.len() >= MAX_NONCES
            && let Some(oldest) = s
                .nonces
                .iter()
                .min_by_key(|(_, exp)| **exp)
                .map(|(n, _)| n.clone())
        {
            s.nonces.remove(&oldest);
        }
        s.nonces
            .insert(nonce.clone(), now + Duration::from_secs(NONCE_TTL_SECS));
        nonce
    }

    /// Use a nonce: true only the first time, and only before it expires. A
    /// nonce that is unknown, expired or used already is refused.
    pub fn take_nonce(&self, nonce: &str) -> bool {
        let now = self.now();
        let mut s = self.lock();
        matches!(s.nonces.remove(nonce), Some(exp) if exp > now)
    }

    // ------------------------------------------------------------ lockout

    /// How long exchanges stay refused, if they are.
    pub fn locked_for(&self) -> Option<Duration> {
        let now = self.now();
        let s = self.lock();
        s.lockout
            .locked_until
            .and_then(|until| until.duration_since(now).ok())
            .filter(|d| !d.is_zero())
    }

    /// Count a failed exchange. The fifth within a minute locks exchanges for
    /// a minute; each further lockout doubles it, up to 15 minutes.
    pub fn record_failure(&self) {
        let now = self.now();
        let mut s = self.lock();
        let l = &mut s.lockout;
        l.failures.push_back(now);
        while l
            .failures
            .front()
            .is_some_and(|t| now.duration_since(*t).unwrap_or_default() >= FAILURE_WINDOW)
        {
            l.failures.pop_front();
        }
        if l.failures.len() >= FAILURES_BEFORE_LOCKOUT {
            let secs = FIRST_LOCKOUT
                .as_secs()
                .saturating_mul(1u64 << l.count.min(16))
                .min(MAX_LOCKOUT.as_secs());
            l.locked_until = Some(now + Duration::from_secs(secs));
            l.count = l.count.saturating_add(1);
            l.failures.clear();
        }
    }

    /// A successful exchange clears the failures and the doubling.
    pub fn record_success(&self) {
        self.lock().lockout = Lockout::default();
    }

    // ------------------------------------------------------------- tokens

    /// A new 256-bit token for `principal`, valid for 15 minutes.
    pub fn issue_token(&self, principal: Principal) -> (String, SystemTime) {
        let now = self.now();
        let value = random_hex(32);
        let expires_at = now + Duration::from_secs(TOKEN_TTL_SECS);
        let mut s = self.lock();
        s.tokens.retain(|t| t.expires_at > now);
        s.last_seen.insert(principal.pairing_id.clone(), now);
        s.tokens.push(Token {
            value: value.clone(),
            principal,
            expires_at,
        });
        (value, expires_at)
    }

    /// The principal of an unexpired token. Every stored token is compared in
    /// constant time, so the time taken does not say how much matched.
    pub fn lookup_token(&self, presented: &str) -> Option<Principal> {
        let now = self.now();
        let mut s = self.lock();
        let mut found = None;
        for t in &s.tokens {
            let same = ct_eq(t.value.as_bytes(), presented.as_bytes());
            if same && t.expires_at > now {
                found = Some(t.principal.clone());
            }
        }
        if let Some(p) = &found {
            s.last_seen.insert(p.pairing_id.clone(), now);
        }
        found
    }

    /// Drop every token of a pairing (revoke).
    pub fn drop_tokens_of(&self, pairing_id: &str) {
        self.lock()
            .tokens
            .retain(|t| t.principal.pairing_id != pairing_id);
    }

    pub fn cancellation_flag(&self, id: &str) -> Arc<AtomicBool> {
        self.lock()
            .cancellation
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    pub fn forget_cancellation_flag(&self, id: &str) {
        self.lock().cancellation.remove(id);
    }
}

/// A parked event poll; dropping it frees the slot.
pub struct WaiterGuard {
    waiting: Waiting,
    pairing: String,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        let mut map = self
            .waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(e) = map.get_mut(&self.pairing) {
            e.0 = e.0.saturating_sub(1);
        }
    }
}

/// Compact JSON text of a value (what the database keeps for a patch, a diff
/// or a before-image). `BTreeMap` keys make it canonical.
pub fn to_json_text<T: serde::Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).expect("a value of ours is JSON")
}

/// The scratch-run credential file of a pairing.
pub fn dev_credential_path(config_dir: &std::path::Path, pairing_id: &str) -> std::path::PathBuf {
    config_dir.join("agent-dev-credentials").join(pairing_id)
}

/// Write the secret (hex) of a pairing for a CLI in another process to read,
/// mode 0600 on Unix. Only runs whose secrets are in memory call this.
pub fn write_dev_credential(
    config_dir: &std::path::Path,
    pairing_id: &str,
    secret_hex: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let path = dev_credential_path(config_dir, pairing_id);
    let dir = path.parent().expect("a file has a parent");
    std::fs::create_dir_all(dir)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        opts.mode(0o600);
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    opts.open(path)?.write_all(secret_hex.as_bytes())
}

pub fn remove_dev_credential(config_dir: &std::path::Path, pairing_id: &str) {
    let _ = std::fs::remove_file(dev_credential_path(config_dir, pairing_id));
}

/// Close pending approvals and their operations inside the pairing revocation
/// transaction, returning the cards the desktop needs to refresh after commit.
pub(crate) fn deny_cards_of(
    app: &crate::App,
    conn: &rusqlite::Transaction<'_>,
    pairing_id: &str,
) -> Result<Vec<String>, musaic_commands::ToolError> {
    let mut ids = Vec::new();
    let cards = musaic_db::agent::list_approvals(conn, Some("pending")).map_err(run::db_err)?;
    for card in cards.into_iter().filter(|c| c.pairing_id == pairing_id) {
        if musaic_db::agent::decide_approval(
            conn,
            &card.id,
            "denied",
            Some("pairing revoked"),
            &app.now_iso(),
        )
        .map_err(run::db_err)?
        {
            if let Some(op) = &card.operation_id {
                let error = musaic_commands::ToolError::new(
                    musaic_commands::ToolErrorCode::Denied,
                    "the pairing was revoked",
                );
                operations::finish_on(
                    conn,
                    op,
                    OperationState::Denied,
                    None,
                    Some(&error),
                    operations::now(app),
                )?;
            }
            ids.push(card.id);
        }
    }
    Ok(ids)
}

/// How long proposals and before-images are kept.
pub const PROPOSAL_KEEP: Duration = Duration::from_secs(7 * 24 * 3600);
pub const CHANGE_KEEP: Duration = Duration::from_secs(90 * 24 * 3600);

/// Recover durable operations and prune old proposals and before-images.
pub fn startup_housekeeping(app: &crate::App) {
    let Ok(conn) = musaic_db::open_connection(app.db_path()) else {
        return;
    };
    if let Err(e) = operations::recover(app, &conn) {
        app.log().warn(
            "musaic.agent",
            &format!("recovering agent operations failed: {e}"),
        );
    }
    run::expire_due(app);
    let now = app.now();
    let cutoff = |keep: Duration| musaic_db::backup::iso_seconds(now - keep);
    let _ = musaic_db::agent::prune_proposals(&conn, &cutoff(PROPOSAL_KEEP));
    if let Ok(ids) = musaic_db::agent::prune_changes(&conn, &cutoff(CHANGE_KEEP)) {
        for id in ids {
            let _ = std::fs::remove_dir_all(app.config_dir().join("agent-changes").join(id));
        }
    }
}

/// The diff a stored proposal carries, for a card's label.
pub fn diff_of(
    proposal: Option<&musaic_db::agent::ProposalRow>,
) -> Option<musaic_commands::tools::fix::FixDiff> {
    serde_json::from_str(&proposal?.diff).ok()
}

/// A new 128-bit operation ID.
pub fn new_operation_id() -> String {
    format!("op_{}", random_hex(16))
}

/// The kinds of an approval or proposal as stored (a JSON array of strings),
/// parsed back; unknown names are dropped.
pub fn parse_kinds(stored: &str) -> Vec<PermissionKind> {
    serde_json::from_str::<Vec<String>>(stored)
        .unwrap_or_default()
        .iter()
        .filter_map(|k| PermissionKind::parse(k))
        .collect()
}

/// Every 30 seconds, expire the approval cards whose time has come. The thread
/// holds the app only while it works and ends with it or at shutdown.
pub fn start_expiry_ticker(app: &crate::App) {
    let weak = std::sync::Arc::downgrade(&app.0);
    let _ = std::thread::Builder::new()
        .name("agent-expiry".into())
        .spawn(move || {
            loop {
                for _ in 0..150 {
                    std::thread::sleep(Duration::from_millis(200));
                    match weak.upgrade() {
                        None => return,
                        Some(inner) if *inner.shutting_down.borrow() => return,
                        Some(_) => {}
                    }
                }
                match weak.upgrade() {
                    Some(inner) => {
                        let app = crate::App(inner);
                        app.ops().agent().expire_due();
                        app.ops().agent().sweep_delegation();
                        claude_desktop::sweep_old(&app);
                    }
                    None => return,
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn hub_at(t: &Arc<Mutex<SystemTime>>) -> AgentHub {
        let t = t.clone();
        AgentHub::new(Arc::new(move || *t.lock().unwrap()))
    }

    fn start() -> Arc<Mutex<SystemTime>> {
        Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        ))
    }

    fn advance(t: &Arc<Mutex<SystemTime>>, secs: u64) {
        *t.lock().unwrap() += Duration::from_secs(secs);
    }

    fn principal(id: &str) -> Principal {
        Principal {
            pairing_id: id.into(),
            pairing_name: "n".into(),
            scopes: vec![PermissionKind::LibraryRead],
            instance_id: "i".into(),
        }
    }

    #[test]
    fn nonces_are_single_use_and_expire() {
        let t = start();
        let h = hub_at(&t);
        let n = h.issue_nonce();
        assert!(h.take_nonce(&n));
        assert!(!h.take_nonce(&n), "single use");
        let n = h.issue_nonce();
        advance(&t, 59);
        assert!(h.take_nonce(&n));
        let n = h.issue_nonce();
        advance(&t, 60);
        assert!(!h.take_nonce(&n), "expired after 60 s");
        assert!(!h.take_nonce("never issued"));
    }

    #[test]
    fn tokens_live_fifteen_minutes_and_drop_with_their_pairing() {
        let t = start();
        let h = hub_at(&t);
        let (tok, exp) = h.issue_token(principal("p_1"));
        let (tok2, _) = h.issue_token(principal("p_2"));
        assert_eq!(exp, h.now() + Duration::from_secs(900));
        assert_eq!(h.lookup_token(&tok).unwrap().pairing_id, "p_1");
        assert!(h.lookup_token("nope").is_none());
        assert!(h.lookup_token("").is_none());
        h.drop_tokens_of("p_1");
        assert!(h.lookup_token(&tok).is_none());
        assert!(h.lookup_token(&tok2).is_some());
        advance(&t, 899);
        assert!(h.lookup_token(&tok2).is_some());
        advance(&t, 1);
        assert!(h.lookup_token(&tok2).is_none(), "expired at 15 minutes");
    }

    #[test]
    fn lockout_doubles_up_to_fifteen_minutes_and_a_success_resets() {
        let t = start();
        let h = hub_at(&t);
        for _ in 0..4 {
            h.record_failure();
            advance(&t, 1);
        }
        assert!(h.locked_for().is_none(), "four failures do not lock");
        h.record_failure();
        assert_eq!(h.locked_for(), Some(Duration::from_secs(60)));
        advance(&t, 59);
        assert!(h.locked_for().is_some());
        advance(&t, 1);
        assert!(h.locked_for().is_none());
        for want in [120, 240, 480, 900, 900] {
            for _ in 0..5 {
                h.record_failure();
            }
            assert_eq!(h.locked_for(), Some(Duration::from_secs(want)), "{want}");
            advance(&t, want);
        }
        h.record_success();
        for _ in 0..5 {
            h.record_failure();
        }
        assert_eq!(
            h.locked_for(),
            Some(Duration::from_secs(60)),
            "reset by a success"
        );
    }

    #[test]
    fn failures_older_than_a_minute_do_not_count() {
        let t = start();
        let h = hub_at(&t);
        for _ in 0..4 {
            h.record_failure();
        }
        advance(&t, 61);
        h.record_failure();
        assert!(h.locked_for().is_none());
    }

    #[test]
    fn stored_kinds_parse_back() {
        assert_eq!(
            parse_kinds(r#"["tags.write","nope","files.rename"]"#),
            [PermissionKind::TagsWrite, PermissionKind::FilesRename]
        );
        assert!(parse_kinds("garbage").is_empty());
    }
}
