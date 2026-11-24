//! The research requests' host side of this app: the library-change
//! notice and the jobs-change subscription. The operations are `ops::jobs`; the bodies the
//! Python app answers with are rendered by `musaic-api`.

use tokio::sync::watch;

use crate::App;

impl App {
    pub(crate) fn research_library_changed(&self) {
        for what in [
            musaic_events::AgentWhat::Work,
            musaic_events::AgentWhat::Operations,
        ] {
            self.bus()
                .publish(&musaic_events::Event::AgentStateChanged {
                    what,
                    ids: Vec::new(),
                });
        }
        self.ai_jobs_changed();
        // The epoch may have changed (a restore, another library folder):
        // parked event polls answer `truncated` now, not at their wait.
        self.0.agent.notify_events();
    }

    /// The jobs stream's subscription: a receiver of its own (no other
    /// connection can consume its wake-ups) with every
    /// change so far already seen. Subscribe before reading the first
    /// payload, so a change in between is sent (as an extra frame), never
    /// lost.
    pub fn subscribe_jobs(&self) -> watch::Receiver<u64> {
        let mut changes = self.0.ai.changes.subscribe();
        changes.borrow_and_update();
        changes
    }
}
