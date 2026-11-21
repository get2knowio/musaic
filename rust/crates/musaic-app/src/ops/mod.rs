//! The operations surface: what a face (HTTP, MCP, a test) calls instead of the
//! services. Blocking functions, typed inputs and results, [`OpError`] for every
//! failure; nothing here knows a status code or a wire format. Rules:
//! `specs/001-operations-without-http/contracts/operations-surface.md`.

pub mod agent;
pub mod analyze;
pub mod apply;
pub mod checks;
pub mod database;
pub mod error;
pub mod events;
pub mod fixes;
pub mod jobs;
pub mod library;
pub mod record;
pub mod scan;
pub mod settings;
pub mod system;
pub mod tags;

pub use error::{OpError, Subject};
pub use events::{Feed, Listening};

use crate::App;

/// The handle a face holds: an [`App`] and one accessor per group.
#[derive(Clone)]
pub struct Ops {
    app: App,
}

impl Ops {
    pub fn system(&self) -> system::System<'_> {
        system::System { app: &self.app }
    }
    pub fn library(&self) -> library::Library<'_> {
        library::Library { app: &self.app }
    }
    pub fn scan(&self) -> scan::Scan<'_> {
        scan::Scan { app: &self.app }
    }
    pub fn settings(&self) -> settings::Settings<'_> {
        settings::Settings { app: &self.app }
    }
    pub fn database(&self) -> database::Database<'_> {
        database::Database { app: &self.app }
    }
    pub fn tags(&self) -> tags::Tags<'_> {
        tags::Tags { app: &self.app }
    }
    pub fn analyze(&self) -> analyze::Analyze<'_> {
        analyze::Analyze { app: &self.app }
    }
    pub fn apply(&self) -> apply::Apply<'_> {
        apply::Apply { app: &self.app }
    }
    pub fn checks(&self) -> checks::Checks<'_> {
        checks::Checks { app: &self.app }
    }
    pub fn fixes(&self) -> fixes::Fixes<'_> {
        fixes::Fixes { app: &self.app }
    }
    pub fn jobs(&self) -> jobs::Jobs<'_> {
        jobs::Jobs { app: &self.app }
    }
    pub fn agent(&self) -> agent::Agent<'_> {
        agent::Agent { app: &self.app }
    }
}

impl App {
    /// The operations surface over this app.
    pub fn ops(&self) -> Ops {
        Ops { app: self.clone() }
    }
}
