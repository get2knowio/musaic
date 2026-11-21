//! The agent bridge's in-memory state and its helpers (research R4, R9).
//!
//! [`AgentHub`] holds what lives only for one app run: the instance id, the
//! single-use nonces of the handshake, the access tokens, the lockout after
//! failed exchanges, the registry of operations an agent started and the
//! idempotency keys. Everything that must outlive a restart (pairings,
//! policies, proposals, approval cards, before-images, activity) is in the
//! database. Time comes from the app's wall clock, so tests script it.
//!
//! Nothing in here writes to the library, takes the work guard or touches the
//! network; the operations in `ops::agent` do, through the services.

pub mod hub_types;
pub mod proposal;
pub mod read;
pub mod run;

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use musaic_commands::auth::{NONCE_TTL_SECS, TOKEN_TTL_SECS, ct_eq, random_hex};
use musaic_commands::tools::operation::{Counts, OperationResult, OperationState};
use musaic_commands::{PermissionKind, ToolError};

pub use hub_types::{Operation, OperationSnapshot, Principal};

use crate::app::WallClock;

/// The most operations the registry keeps; finished ones go oldest first.
pub const MAX_OPERATIONS: usize = 1000;
/// The most outstanding handshake nonces; the oldest is dropped past it.
pub const MAX_NONCES: usize = 1024;
/// Failed exchanges within [`FAILURE_WINDOW`] that lock exchanges.
pub const FAILURES_BEFORE_LOCKOUT: usize = 5;
pub const FAILURE_WINDOW: Duration = Duration::from_secs(60);
pub const FIRST_LOCKOUT: Duration = Duration::from_secs(60);
pub const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);

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
    operations: HashMap<String, Operation>,
    order: VecDeque<String>,
    /// `(pairing id, key)` -> `(input digest, operation id)`.
    idempotency: HashMap<(String, String), (String, String)>,
}

/// The per-run agent state.
pub struct AgentHub {
    instance_id: String,
    clock: WallClock,
    state: Mutex<State>,
    /// The `agent-op-*` threads, joined at shutdown.
    workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl AgentHub {
    pub fn new(clock: WallClock) -> Self {
        Self {
            instance_id: format!("i_{}", random_hex(8)),
            clock,
            state: Mutex::new(State::default()),
            workers: Mutex::new(Vec::new()),
        }
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
        for op in self.lock().operations.values() {
            if !op.state.is_finished() {
                op.cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
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
        let s = self.lock();
        let mut found = None;
        for t in &s.tokens {
            let same = ct_eq(t.value.as_bytes(), presented.as_bytes());
            if same && t.expires_at > now {
                found = Some(t.principal.clone());
            }
        }
        found
    }

    /// Drop every token of a pairing (revoke).
    pub fn drop_tokens_of(&self, pairing_id: &str) {
        self.lock()
            .tokens
            .retain(|t| t.principal.pairing_id != pairing_id);
    }

    // --------------------------------------------------------- operations

    /// Register a new operation. A finished one is evicted, oldest first,
    /// when the registry is full.
    pub fn register_operation(&self, op: Operation) {
        let mut s = self.lock();
        while s.order.len() >= MAX_OPERATIONS {
            let evict = s
                .order
                .iter()
                .position(|id| s.operations.get(id).is_none_or(|o| o.state.is_finished()));
            match evict {
                Some(i) => {
                    if let Some(id) = s.order.remove(i) {
                        s.operations.remove(&id);
                        // A key naming a forgotten operation would only
                        // answer "gone"; it goes with it, so the map stays
                        // bounded by the registry.
                        s.idempotency.retain(|_, (_, op)| *op != id);
                    }
                }
                // Nothing finished to evict: the registry is full of live
                // work; grow rather than drop a running operation.
                None => break,
            }
        }
        s.order.push_back(op.id.clone());
        s.operations.insert(op.id.clone(), op);
    }

    /// A copy of an operation's state, when `pairing_id` owns it.
    pub fn operation(&self, id: &str, pairing_id: &str) -> Option<OperationSnapshot> {
        self.lock()
            .operations
            .get(id)
            .filter(|o| o.pairing_id == pairing_id)
            .map(Operation::snapshot)
    }

    /// A copy of an operation's state whoever owns it (the desktop's own
    /// approve path).
    pub fn operation_any(&self, id: &str) -> Option<OperationSnapshot> {
        self.lock().operations.get(id).map(Operation::snapshot)
    }

    /// Change an operation under the lock.
    pub fn update_operation<R>(&self, id: &str, f: impl FnOnce(&mut Operation) -> R) -> Option<R> {
        self.lock().operations.get_mut(id).map(f)
    }

    /// Set an operation's state unless it is finished already; returns whether
    /// it changed.
    pub fn set_state(&self, id: &str, state: OperationState) -> bool {
        self.update_operation(id, |o| {
            if o.state.is_finished() {
                false
            } else {
                o.state = state;
                true
            }
        })
        .unwrap_or(false)
    }

    /// Finish an operation with a result or an error (once).
    pub fn finish_operation(
        &self,
        id: &str,
        state: OperationState,
        result: Option<OperationResult>,
        error: Option<ToolError>,
    ) {
        self.update_operation(id, |o| {
            if !o.state.is_finished() {
                o.state = state;
                o.result = result;
                o.error = error;
            }
        });
    }

    pub fn set_counts(&self, id: &str, counts: Counts) {
        self.update_operation(id, |o| o.counts = counts);
    }

    // -------------------------------------------------------- idempotency

    /// The operation a pairing's key already names: `Some(Ok(id))` for the
    /// same input, `Some(Err(()))` when the key was used with other input.
    pub fn idempotent(
        &self,
        pairing_id: &str,
        key: &str,
        input_digest: &str,
    ) -> Option<Result<String, ()>> {
        self.lock()
            .idempotency
            .get(&(pairing_id.to_string(), key.to_string()))
            .map(|(digest, op)| {
                if digest == input_digest {
                    Ok(op.clone())
                } else {
                    Err(())
                }
            })
    }

    pub fn remember_idempotent(
        &self,
        pairing_id: &str,
        key: &str,
        input_digest: &str,
        operation_id: &str,
    ) {
        self.lock().idempotency.insert(
            (pairing_id.to_string(), key.to_string()),
            (input_digest.to_string(), operation_id.to_string()),
        );
    }

    /// Forget a key (the operation it named was never created).
    pub fn forget_idempotent(&self, pairing_id: &str, key: &str) {
        self.lock()
            .idempotency
            .remove(&(pairing_id.to_string(), key.to_string()));
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

/// Deny every pending card of a pairing (it was revoked), and end the
/// operations waiting on them. Publishes the approvals change.
pub fn deny_cards_of(
    app: &crate::App,
    conn: &rusqlite::Connection,
    pairing_id: &str,
    reason: &str,
) {
    let now = app.now_iso();
    let mut ids = Vec::new();
    let cards = musaic_db::agent::list_approvals(conn, Some("pending")).unwrap_or_default();
    for card in cards.into_iter().filter(|c| c.pairing_id == pairing_id) {
        if musaic_db::agent::decide_approval(conn, &card.id, "denied", Some(reason), &now)
            .unwrap_or(false)
        {
            if let Some(op) = &card.operation_id {
                app.0
                    .agent
                    .finish_operation(op, OperationState::Denied, None, None);
            }
            ids.push(card.id);
        }
    }
    if !ids.is_empty() {
        app.bus().publish(&musaic_events::Event::AgentStateChanged {
            what: musaic_events::AgentWhat::Approvals,
            ids,
        });
    }
}

/// How long proposals and before-images are kept.
pub const PROPOSAL_KEEP: Duration = Duration::from_secs(7 * 24 * 3600);
pub const CHANGE_KEEP: Duration = Duration::from_secs(90 * 24 * 3600);

/// At startup: a restart ends the operation handles a card waits on, so every
/// pending card expires ("app restarted"); old proposals and before-images
/// are pruned, the latter with their directories. Failures are logged, never
/// fatal.
pub fn startup_housekeeping(app: &crate::App) {
    let Ok(conn) = musaic_db::open_connection(app.db_path()) else {
        return;
    };
    let now = app.now();
    let at = musaic_db::backup::iso_seconds(now);
    match musaic_db::agent::expire_all_pending(&conn, "app restarted", &at) {
        Ok(ids) if !ids.is_empty() => app.log().info(
            "musaic.agent",
            &format!("{} pending approval card(s) expired at start", ids.len()),
        ),
        Ok(_) => {}
        Err(e) => app.log().warn(
            "musaic.agent",
            &format!("expiring pending cards failed: {e}"),
        ),
    }
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

/// A new operation id (`op_` + 16 hex).
pub fn new_operation_id() -> String {
    format!("op_{}", random_hex(8))
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
                        crate::App(inner).ops().agent().expire_due();
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
    fn operations_are_owned_evicted_oldest_finished_first_and_idempotent() {
        let t = start();
        let h = hub_at(&t);
        let mk =
            |id: &str, pairing: &str| Operation::new(id, pairing, "musaic_scan_start", h.now());
        h.register_operation(mk("op_1", "p_1"));
        assert!(h.operation("op_1", "p_1").is_some());
        assert!(
            h.operation("op_1", "p_2").is_none(),
            "not another pairing's"
        );
        h.finish_operation("op_1", OperationState::Succeeded, None, None);
        h.finish_operation("op_1", OperationState::Failed, None, None);
        assert_eq!(
            h.operation("op_1", "p_1").unwrap().state,
            OperationState::Succeeded
        );
        assert!(
            !h.set_state("op_1", OperationState::Running),
            "finished stays finished"
        );
        for i in 0..MAX_OPERATIONS {
            h.register_operation(mk(&format!("op_x{i}"), "p_1"));
        }
        assert!(
            h.operation("op_1", "p_1").is_none(),
            "the finished one was evicted"
        );
        assert!(
            h.operation("op_x0", "p_1").is_some(),
            "running ones are kept"
        );

        assert!(h.idempotent("p_1", "k", "d1").is_none());
        h.remember_idempotent("p_1", "k", "d1", "op_x0");
        assert_eq!(h.idempotent("p_1", "k", "d1"), Some(Ok("op_x0".into())));
        assert_eq!(h.idempotent("p_1", "k", "d2"), Some(Err(())));
        assert!(h.idempotent("p_2", "k", "d1").is_none(), "per pairing");
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
