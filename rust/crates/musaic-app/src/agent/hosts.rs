//! What the desktop knows about each paired host (spec 005): the latest
//! observation the host's stdio process sent, and four separate facts with
//! times, so the person can see what is true instead of a single "ready".
//!
//! * paired: the pairing exists (always true);
//! * bridge reachable: the pairing last made an authenticated request;
//! * subscription active: an events poll is parked, or the pairing polled within 90 s;
//! * last work claimed: the pairing's latest claim.
//!
//! The in-memory facts reset when the app restarts; the times are shown as
//! times, never as a promise.

use std::time::{Duration, SystemTime};

use musaic_commands::events::{HostObservation, SessionStatus};
use musaic_commands::skill;
use musaic_db::agent_hosts::{self as store, Observation};
use musaic_events::{AgentWhat, Event};

use crate::App;
use crate::agent::Principal;
use crate::ops::agent::{AgentError, conn};

/// A host's strings are shown as it reported them, bounded and cleaned.
const MAX_TEXT: usize = 80;

/// What a host reported, as the desktop shows it ("reported by the host").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRecord {
    pub client_name: String,
    pub client_version: String,
    pub profile: String,
    pub tasks: bool,
    pub skills: bool,
    pub events: bool,
    pub observed_at: String,
}

/// The facts about one pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingDiagnostics {
    pub pairing_id: String,
    pub name: String,
    pub scopes: Vec<musaic_commands::PermissionKind>,
    pub revoked: bool,
    pub paired_at: String,
    pub reachable_at: Option<String>,
    pub subscription_active: bool,
    pub subscription_at: Option<String>,
    pub last_claim_at: Option<String>,
    pub last_claim_work_id: Option<String>,
    pub host: Option<HostRecord>,
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TEXT)
        .collect::<String>()
        .trim()
        .to_string()
}

fn clean_profile(text: &str) -> String {
    // A profile is a date; anything else is shown as nothing.
    let t = clean(text);
    if t.len() == 10 && t.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        t
    } else {
        String::new()
    }
}

fn iso(t: SystemTime) -> String {
    musaic_db::backup::iso_seconds(t)
}

fn at_ms(ms: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

/// Tell the desktop to refresh diagnostics for one pairing, at most once per
/// pairing every five seconds.
pub(crate) fn announce(app: &App, pairing: &str) {
    if app.0.agent.allow_announce(pairing) {
        app.bus().publish(&Event::AgentStateChanged {
            what: AgentWhat::Hosts,
            ids: vec![pairing.to_string()],
        });
    }
}

/// Publish the refreshes the rate limit held back and the subscriptions that
/// began or lapsed since they were last shown (the app's 30-second tick).
pub(crate) fn flush(app: &App) {
    for pairing in app.0.agent.due_announces() {
        app.bus().publish(&Event::AgentStateChanged {
            what: AgentWhat::Hosts,
            ids: vec![pairing],
        });
    }
}

/// Store what the host reported. Bad values are cleaned or dropped; this
/// never fails a session. Publishes when what is stored changed.
pub fn observe_host(app: &App, principal: &Principal, o: &HostObservation) {
    let observation = Observation {
        client_name: clean(&o.client_name),
        client_version: clean(&o.client_version),
        profile: clean_profile(&o.profile),
        tasks: o.tasks,
        skills: o.skills,
        events: o.events,
    };
    let now = crate::agent::delegation::now_ms(app);
    let changed = conn(app)
        .ok()
        .and_then(|c| store::upsert(&c, &principal.pairing_id, &observation, now).ok())
        .unwrap_or(false);
    if changed {
        announce(app, &principal.pairing_id);
    }
}

pub fn session_status(app: &App, principal: &Principal) -> Result<SessionStatus, AgentError> {
    let c = conn(app)?;
    let observation = store::get(&c, &principal.pairing_id)?.map(|s| HostObservation {
        client_name: s.observation.client_name,
        client_version: s.observation.client_version,
        profile: s.observation.profile,
        tasks: s.observation.tasks,
        skills: s.observation.skills,
        events: s.observation.events,
    });
    Ok(SessionStatus {
        observation,
        skill_version: skill::bundle().version().to_string(),
        subscription_active: app.0.agent.subscription_active(&principal.pairing_id),
    })
}

pub fn diagnostics(app: &App) -> Result<Vec<PairingDiagnostics>, AgentError> {
    let c = conn(app)?;
    let hub = &app.0.agent;
    let mut out = Vec::new();
    for row in musaic_db::agent::list_pairings(&c)? {
        let (_, parked_at) = hub.waiting_of(&row.id);
        let claim = musaic_db::delegated_work::last_claim_of(&c, &row.id)?;
        let host = store::get(&c, &row.id)?.map(|s| HostRecord {
            client_name: s.observation.client_name,
            client_version: s.observation.client_version,
            profile: s.observation.profile,
            tasks: s.observation.tasks,
            skills: s.observation.skills,
            events: s.observation.events,
            observed_at: iso(at_ms(s.observed_at)),
        });
        out.push(PairingDiagnostics {
            scopes: crate::agent::parse_kinds(&row.scopes),
            revoked: row.revoked_at.is_some(),
            paired_at: row.created_at.clone(),
            reachable_at: hub.last_seen(&row.id).map(iso),
            subscription_active: hub.subscription_active(&row.id),
            subscription_at: parked_at.map(iso),
            last_claim_at: claim.as_ref().map(|(t, _)| iso(at_ms(*t))),
            last_claim_work_id: claim.map(|(_, w)| w),
            pairing_id: row.id,
            name: row.name,
            host,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_bounded_and_cleaned() {
        assert_eq!(clean("  Host\u{7}\n Name  "), "Host Name");
        assert_eq!(clean(&"x".repeat(500)).len(), MAX_TEXT);
        assert_eq!(clean_profile("2026-07-28"), "2026-07-28");
        assert_eq!(clean_profile("<script>"), "");
    }
}
