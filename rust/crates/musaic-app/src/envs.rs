//! The services' environments over this app: what the apply
//! pipeline ([`ApplyEnv`]), the analyses ([`AnalyzeEnv`]), the track-tag
//! and vocabulary operations ([`TagEnv`]) and the check fixes
//! ([`musaic_services::check_fixes::FixEnv`], through
//! [`fix_flow::with_fix_env`]) need, assembled from the app's parts. The
//! host opens the connection (and answers its own way when it cannot) and
//! hands it in; each method builds the environment and runs `f` with it.

use musaic_services::ApplyEnv;
use musaic_services::analyze::AnalyzeEnv;
use musaic_services::check_fixes::{FixEnv, FixSources};
use musaic_services::fix_flow::{self, FixHost};
use musaic_services::multi_disc_convert::ConvertEnv;
use musaic_services::production::NetCovers;
use musaic_services::track_tags::TagEnv;
use rusqlite::Connection;

use crate::App;

impl App {
    /// The apply pipeline's environment over `conn`: the bus, covers
    /// downloaded through the app's transport, the app clock, the track
    /// order.
    pub fn with_apply_env<R>(
        &self,
        conn: &mut Connection,
        f: impl FnOnce(&mut ApplyEnv<'_>) -> R,
    ) -> R {
        let covers = NetCovers {
            transport: self.0.transport.clone(),
        };
        let now = || self.now_iso();
        let mut env = ApplyEnv {
            conn,
            bus: &self.0.bus,
            covers: &covers,
            now: &now,
            track_order: self.0.track_order,
        };
        f(&mut env)
    }

    /// The analyses' environment over `conn`: the bus, the app's analyzer,
    /// the app clock, the track order.
    pub fn with_analyze_env<R>(
        &self,
        conn: &mut Connection,
        f: impl FnOnce(&mut AnalyzeEnv<'_>) -> R,
    ) -> R {
        let now = || self.now_iso();
        let mut env = AnalyzeEnv {
            conn,
            bus: &self.0.bus,
            analyzer: self.0.analyzer.as_ref(),
            now: &now,
            track_order: self.0.track_order,
        };
        f(&mut env)
    }

    /// The multi-disc conversion's environment over `conn`: the bus, the
    /// library as configured now, the app clock, the track order, the
    /// conversion records under the config dir, the scan service (scans
    /// wait while a conversion runs).
    pub fn with_convert_env<R>(
        &self,
        conn: &mut Connection,
        f: impl FnOnce(&mut ConvertEnv<'_>) -> R,
    ) -> R {
        let lib = self.library_dir();
        let now = || self.now_iso();
        let logs = musaic_services::multi_disc_convert::log_dir(self.config_dir());
        let mut env = ConvertEnv {
            conn,
            bus: &self.0.bus,
            library_dir: lib.as_deref(),
            now: &now,
            track_order: self.0.track_order,
            logs: &logs,
            scan: self.scan(),
            stop_at: None,
        };
        f(&mut env)
    }

    /// The track-tag environment over `conn`: the library as configured
    /// now, the track order, one timestamp taken now.
    pub fn with_tag_env<R>(
        &self,
        conn: &mut Connection,
        f: impl FnOnce(&mut TagEnv<'_>) -> R,
    ) -> R {
        let lib = self.library_dir();
        let now = self.now_iso();
        let mut env = TagEnv {
            conn,
            library_dir: lib.as_deref(),
            order: self.0.track_order,
            now: &now,
        };
        f(&mut env)
    }

    /// The check fixes' environment over `conn`
    /// ([`fix_flow::with_fix_env`]): the library as configured now, the
    /// resolver's parts with the AcoustID key as it is now, the app clock,
    /// the test override of the fixes' sources, and whether a model backend
    /// exists.
    pub fn with_fix_env<T>(&self, conn: Connection, f: impl FnOnce(&mut FixEnv<'_>) -> T) -> T {
        let lib = self.library_dir();
        let parts = self.parts();
        let now = || self.now_iso();
        // The fixes decide on this in musaic-services.
        let model_unavailable = musaic_ai::model_unavailable(self.0.ai.provider.as_ref());
        fix_flow::with_fix_env(
            FixHost {
                conn,
                bus: &self.0.bus,
                checks: &self.0.checks_env,
                fix_sources: self.0.fix_sources.as_deref().map(|s| s as &dyn FixSources),
                library_dir: lib,
                parts: &parts,
                fingerprinting: &self.0.fingerprinting,
                transport: self.0.transport.clone(),
                config_dir: self.config_dir(),
                now_iso: &now,
                track_order: self.0.track_order,
                model_unavailable,
            },
            f,
        )
    }
}
