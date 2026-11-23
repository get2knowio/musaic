//! The AI job queue's host side of this app: the providers this build has and
//! the jobs-change subscription. The operations are `ops::jobs`; the bodies the
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

    /// The providers this build has: the one the worker uses. The one source of
    /// truth for which `ai_provider_id` values a settings patch accepts (issue
    /// #199).
    pub(crate) fn ai_provider_infos(&self) -> Vec<musaic_ai::provider::ProviderInfo> {
        vec![self.0.ai.provider.info()]
    }

    /// The ids of [`App::ai_provider_infos`].
    pub(crate) fn ai_provider_ids(&self) -> Vec<String> {
        self.ai_provider_infos().into_iter().map(|p| p.id).collect()
    }

    /// The jobs stream's subscription: a receiver of its own (no other
    /// connection or the dispatcher can consume its wake-ups) with every
    /// change so far already seen. Subscribe before reading the first
    /// payload, so a change in between is sent (as an extra frame), never
    /// lost.
    pub fn subscribe_jobs(&self) -> watch::Receiver<u64> {
        let mut changes = self.0.ai.changes.subscribe();
        changes.borrow_and_update();
        changes
    }
}
