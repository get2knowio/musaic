//! The HTTP layer: the Python app's FastAPI routes (`musaic/main.py`,
//! `musaic/api/*.py`) on axum, wiring the crates together.
//!
//! * [`router`] builds the app from an [`AppState`]
//!   ([`AppState::builder`]: config dir, secrets store, network, clock).
//! * Every route of the Python app's OpenAPI document is registered:
//!   implemented ones in [`router`], the rest as explicit `501` stubs listed
//!   in [`STUBS`] ([`PLACEHOLDERS`], routes answering a fixed state, is
//!   empty now that the fingerprint pool is wired). Nothing is silently missing; `tests/inventory.rs`
//!   checks the router against `testdata/golden/api/routes.json`.
//! * Unknown `/api/*` paths are a JSON 404, never the SPA; other
//!   methods on unknown paths are 405, as in the Python app.
//! * Blocking crate calls run on `spawn_blocking` with the musaic-events
//!   guard flags carried over ([`http::blocking`]).

pub mod ai;
pub mod bind;
pub mod files;
pub mod gate;
pub mod host_check;
pub mod http;
pub mod jpegdec;
pub mod keyscrub;
pub mod launch;
pub mod lax;
pub mod logging;
pub mod loopback;
pub mod render;
pub mod route_table;
pub mod routes;
pub mod sources;
pub mod sse;
pub mod starlette_routing;
pub mod state;
pub mod thumb_pool;
pub mod thumbs;
pub mod verification;
pub mod work;

use axum::Router;
use axum::extract::{MatchedPath, Request};
use axum::middleware::{self, Next};
use axum::response::Response;

pub use gate::{Busy, RestoreGuard, WorkGate, WorkGuard, WorkKind};
pub use host_check::AllowedHosts;
pub use launch::LaunchGate;
pub use loopback::{LoopbackOptions, LoopbackServer, StopReport, serve_loopback};
pub use route_table::RouteRow;
pub use sources::{Fingerprinting, Network};
pub use state::{
    AppState, AppStateBuilder, COVER_BODY_LIMIT, RESTORE_UPLOAD_LIMIT, SharedAnalyzer,
    SharedSecrets, StartupError, WallClock,
};
pub use work::WorkSnapshot;

use routes::{analyze, apply, checks, fixes, jobs, library, preview, settings, system, tags};

/// Python app routes registered as `501 Not Implemented` (method, path in the
/// Python app's spelling, why). None left: the last one, the duplicate-tracks
/// canonical lookup, is built.
pub const STUBS: &[(&str, &str, &str)] = &[];

/// Routes the Python app does not have: deliberate additions (method, path,
/// why). `tests/inventory.rs` pins this list and checks that every
/// registered route is either a Python app route or listed here.
pub const EXTRA_ROUTES: &[(&str, &str, &str)] = &[
    (
        "GET",
        routes::preview::COVER_PREVIEW_ROUTE,
        "same-origin preview of a job's suggested remote cover (the desktop CSP allows img-src 'self' only); not a route of the Python app",
    ),
    (
        "GET",
        routes::preview::ALBUM_COVER_PREVIEW_ROUTE,
        "same-origin preview of the Cover Art Archive cover the resolver proposes for an anchored album, with no job (issues #183, #184); not a route of the Python app",
    ),
    (
        "POST",
        SCAN_CANCEL_ROUTE,
        "cancel the running library scan (the Python app could not cancel a scan); not a route of the Python app",
    ),
    (
        "GET",
        SCAN_FOLLOW_ROUTE,
        "follow the running (or last) library scan without starting one, for the Activity panel (issue #332); not a route of the Python app",
    ),
    (
        "GET",
        CHECK_SWEEP_ROUTE,
        "the background check sweep's progress (issue #187: every check runs on its own); not a route of the Python app",
    ),
    (
        "GET",
        MULTI_DISC_PREVIEW_ROUTE,
        "read-only preview of converting a multi-disc set to one folder, re-derived from disk (issue #296; the multi-disc-layout check is not a check of the Python app); not a route of the Python app",
    ),
    (
        "POST",
        MULTI_DISC_CONVERT_ROUTE,
        "convert a multi-disc set, kept in one folder or in one folder per disc, exactly as its preview shows, re-derived from disk, with a record kept under the config dir (issue #296); answers JSON, or an SSE stream of its progress when asked for one; not a route of the Python app",
    ),
    (
        "POST",
        MULTI_DISC_ABANDON_ROUTE,
        "stop treating an unfinished multi-disc conversion as one: its record is closed as abandoned and every file stays where it is (issue #296); not a route of the Python app",
    ),
    (
        "GET",
        ACOUSTID_PROMPT_ROUTE,
        "whether the one-time \"add an AcoustID key\" prompt was dismissed; kept on the server because the desktop window's address changes on every launch, so browser storage would forget it; not a route of the Python app",
    ),
    (
        "POST",
        ACOUSTID_PROMPT_DISMISS_ROUTE,
        "dismiss that prompt for good; not a route of the Python app",
    ),
    (
        "GET",
        UI_PREFS_ROUTE,
        "the light/dark theme and Library tile size, kept on the server because the desktop window's address changes on every launch, so browser storage would forget them; not a route of the Python app",
    ),
    (
        "PUT",
        UI_PREFS_ROUTE,
        "change those preferences; not a route of the Python app",
    ),
];

/// `GET` / `PUT /api/ui-prefs` ([`EXTRA_ROUTES`]).
pub const UI_PREFS_ROUTE: &str = "/api/ui-prefs";

/// `GET /api/prompts/acoustid` ([`EXTRA_ROUTES`]).
pub const ACOUSTID_PROMPT_ROUTE: &str = "/api/prompts/acoustid";

/// `POST /api/prompts/acoustid/dismiss` ([`EXTRA_ROUTES`]).
pub const ACOUSTID_PROMPT_DISMISS_ROUTE: &str = "/api/prompts/acoustid/dismiss";

/// `GET /api/checks/multi-disc-layout/findings/{finding_id}/conversion-preview`
/// ([`EXTRA_ROUTES`]).
pub const MULTI_DISC_PREVIEW_ROUTE: &str =
    "/api/checks/multi-disc-layout/findings/{finding_id}/conversion-preview";

/// `POST /api/checks/multi-disc-layout/findings/{finding_id}/convert`
/// ([`EXTRA_ROUTES`]).
pub const MULTI_DISC_CONVERT_ROUTE: &str =
    "/api/checks/multi-disc-layout/findings/{finding_id}/convert";

/// `POST /api/checks/multi-disc-layout/findings/{finding_id}/abandon-conversion`
/// ([`EXTRA_ROUTES`]).
pub const MULTI_DISC_ABANDON_ROUTE: &str =
    "/api/checks/multi-disc-layout/findings/{finding_id}/abandon-conversion";

/// `GET /api/checks/sweep` ([`EXTRA_ROUTES`]).
pub const CHECK_SWEEP_ROUTE: &str = "/api/checks/sweep";

/// `POST /api/scan/cancel` ([`EXTRA_ROUTES`]).
pub const SCAN_CANCEL_ROUTE: &str = "/api/scan/cancel";

/// `GET /api/scan/follow` ([`EXTRA_ROUTES`]).
pub const SCAN_FOLLOW_ROUTE: &str = "/api/scan/follow";

/// Registrations that are not routes of their own on the wire: each
/// reproduces how Starlette routes a request the Python app has no route for
/// at that pattern (first full match in registration order, where axum
/// prefers the static segment). (method, axum path, the Python app route that
/// answers it.) Pinned in `tests/inventory.rs`; the behaviour against
/// testdata/golden/api/route_order.json in `tests/starlette_routing.rs`.
/// Reproductions, not deliberate changes.
pub const ROUTING_ALIASES: &[(&str, &str, &str)] = &[
    (
        "DELETE",
        "/api/ai/jobs/stream",
        "DELETE /api/ai/jobs/{job_id}",
    ),
    (
        "POST",
        "/api/ai/jobs/active/{album_id}",
        "POST /api/ai/jobs/{job_id}/acted-on",
    ),
];

/// Python app routes that answer with the Python app's shape but a fixed state,
/// because the subsystem behind them is not built. None left.
pub const PLACEHOLDERS: &[(&str, &str, &str)] = &[];

/// The route a request matched, as a response extension (tests read it; it
/// never reaches the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteTag(pub String);

async fn tag_route(req: Request, next: Next) -> Response {
    let tag = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());
    let mut resp = next.run(req).await;
    if let Some(t) = tag {
        resp.extensions_mut().insert(RouteTag(t));
    }
    resp
}

/// Optional layers in front of the routes ([`router_with`]).
#[derive(Clone, Default)]
pub struct RouterOptions {
    /// Require the per-launch cookie on every request (the desktop
    /// app). `None` (the server binary): no token.
    pub launch: Option<std::sync::Arc<LaunchGate>>,
}

/// The application router. Every route, the fallback and the
/// method-not-allowed answers sit behind the Host / Origin check.
/// No launch token: the plain server binary.
pub fn router(state: AppState) -> Router {
    router_with(state, RouterOptions::default())
}

/// Every route of the application, registered through
/// [`route_table::Table`] so the table of (method, path, query parameters)
/// is the router's own (`tests/inventory.rs`).
fn routes() -> (Router<AppState>, Vec<RouteRow>) {
    use route_table::{Table, delete, get, on, post};
    let mut t = Table::default()
        .route("/api/health", get(system::health))
        .route(
            "/api/config",
            get(library::get_config).post(library::post_config),
        )
        .route("/api/albums", get(library::list_albums).query(&["refresh"]))
        .route("/api/scan/status", get(library::scan_status))
        .route(
            "/api/library-events/stream",
            get(system::library_events_stream),
        )
        .route(
            "/api/fingerprint-pool/status",
            get(system::fingerprint_pool_status),
        )
        .route(
            "/api/fingerprint-pool/stream",
            get(system::fingerprint_pool_stream),
        )
        .route(
            "/api/scan/stream",
            get(library::scan_stream).query(&["force"]),
        )
        .route("/api/scan/rescan", post(library::rescan))
        // Not a Python app route: cancel a running scan ([`EXTRA_ROUTES`]).
        .route(SCAN_CANCEL_ROUTE, post(library::scan_cancel))
        // Not a Python app route: watch a scan, never start one ([`EXTRA_ROUTES`]).
        .route(SCAN_FOLLOW_ROUTE, get(library::scan_follow))
        .route(
            "/api/albums/{album_id}",
            get(library::get_album).delete(library::delete_album),
        )
        .route(
            "/api/albums/{album_id}/cover",
            get(library::get_cover).query(&["size"]),
        )
        .route("/api/albums/{album_id}/tracks", get(library::get_tracks))
        .route(
            "/api/albums/{album_id}/mb-anchor/clear",
            post(library::clear_mb_anchor),
        )
        .route(
            "/api/albums/{album_id}/resolution",
            get(library::get_resolution).query(&["force", "hint"]),
        )
        .route(
            "/api/settings",
            get(settings::get_settings).patch(settings::patch_settings),
        )
        // Not Python app routes: the AcoustID prompt's dismissal ([`EXTRA_ROUTES`]).
        .route(ACOUSTID_PROMPT_ROUTE, get(settings::acoustid_prompt))
        .route(
            ACOUSTID_PROMPT_DISMISS_ROUTE,
            post(settings::dismiss_acoustid_prompt),
        )
        // Not Python app routes: the UI preferences ([`EXTRA_ROUTES`]).
        .route(
            UI_PREFS_ROUTE,
            get(settings::ui_prefs).put(settings::put_ui_prefs),
        )
        .route("/api/db/info", get(settings::db_info))
        .route("/api/db/backup", get(settings::db_backup))
        .route(
            "/api/db/restore",
            post(settings::db_restore).no_body_limit(),
        )
        .route(
            "/api/db/snapshots",
            get(settings::list_snapshots).post(settings::create_snapshot),
        )
        .route(
            "/api/db/snapshots/{name}",
            delete(settings::delete_snapshot),
        )
        .route(
            "/api/db/snapshots/{name}/restore",
            post(settings::restore_snapshot),
        )
        .route("/api/checks/stream", get(checks::checks_stream))
        // Not a Python app route: the check sweep's progress ([`EXTRA_ROUTES`]).
        .route(CHECK_SWEEP_ROUTE, get(checks::sweep_status))
        .route(
            "/api/checks/findings",
            get(checks::list_all_findings).query(&[
                "album_id",
                "check_id",
                "include_ignored",
                "include_resolved",
            ]),
        )
        .route("/api/checks", get(checks::list_checks))
        .route("/api/checks/{check_id}", get(checks::get_check))
        .route("/api/checks/{check_id}/run", post(checks::run_check))
        .route(
            "/api/checks/{check_id}/findings/{finding_id}/ignore",
            post(checks::ignore_finding),
        )
        .route(
            "/api/checks/{check_id}/findings/{finding_id}/unignore",
            post(checks::unignore_finding),
        )
        .route(
            "/api/checks/filename-mismatch/findings/{finding_id}/plan-preview",
            get(checks::plan_preview),
        )
        .route(
            "/api/checks/duplicate-tracks/findings/{finding_id}/cluster-preview",
            get(checks::cluster_preview),
        )
        // Not a Python app route: the multi-disc conversion preview ([`EXTRA_ROUTES`]).
        .route(MULTI_DISC_PREVIEW_ROUTE, get(checks::conversion_preview))
        .route(
            "/api/checks/duplicate-tracks/findings/{finding_id}/canonical",
            get(checks::duplicate_canonical),
        )
        .route(
            "/api/albums/{album_id}/apply/stream",
            post(apply::apply_stream),
        )
        .route(
            "/api/albums/{album_id}/analyze/stream",
            post(analyze::analyze_stream),
        )
        .route(
            "/api/albums/{album_id}/cover/upload",
            // Capped in the handler (AppState::cover_body_limit).
            post(apply::upload_cover).no_body_limit(),
        )
        .route(
            "/api/albums/{album_id}/tags",
            get(tags::get_album_tags)
                .put(tags::replace_album_tags)
                .post(tags::add_album_tag),
        )
        .route(
            "/api/albums/{album_id}/tags/{tag}",
            delete(tags::remove_album_tag),
        )
        .route(
            "/api/albums/{album_id}/tracks/{filename}/tags",
            post(tags::add_track_tag),
        )
        .route(
            "/api/albums/{album_id}/tracks/{filename}/tags/{tag}",
            delete(tags::remove_track_tag),
        )
        .route("/api/tags", get(tags::list_tags))
        .route("/api/tags/{tag}/tracks", get(tags::tracks_for_tag))
        .route(
            "/api/vocabulary",
            get(tags::list_vocabulary).post(tags::add_vocabulary_entry),
        )
        .route(
            "/api/vocabulary/orphans/{tag}/promote",
            post(tags::promote_orphan_tag),
        )
        .route(
            "/api/vocabulary/{tag}",
            delete(tags::delete_vocabulary_entry)
                .query(&["strip_from_tracks"])
                .patch(tags::patch_vocabulary_entry),
        )
        .route(
            "/api/vocabulary/{tag}/merge",
            post(tags::merge_vocabulary_entry),
        )
        .route(
            "/api/vocabulary/{tag}/rename",
            post(tags::rename_vocabulary_entry),
        )
        .route(
            "/api/checks/{check_id}/findings/{finding_id}/fix",
            post(fixes::fix_finding),
        )
        .route(
            "/api/checks/{check_id}/quick-fix-all",
            post(fixes::quick_fix_all),
        )
        .route(
            "/api/checks/filename-mismatch/findings/{finding_id}/apply",
            post(fixes::apply_filename_rename),
        )
        .route(
            "/api/checks/duplicate-tracks/findings/{finding_id}/delete-file",
            post(fixes::delete_duplicate_file),
        )
        .route(
            "/api/checks/inconsistent-artist-sort/findings/{finding_id}/apply-sort",
            post(fixes::apply_cluster_sort),
        )
        // Not a Python app route: the multi-disc conversion ([`EXTRA_ROUTES`]).
        .route(MULTI_DISC_CONVERT_ROUTE, post(fixes::convert_multi_disc))
        .route(
            MULTI_DISC_ABANDON_ROUTE,
            post(fixes::abandon_multi_disc_conversion),
        )
        .route(
            "/api/ai/jobs",
            get(jobs::list_jobs)
                .query(&["status"])
                .post(jobs::enqueue_job)
                .delete(jobs::bulk_delete_jobs)
                .query(&["status"]),
        )
        // Starlette takes the first full match in registration order, so
        // DELETE on the static `stream` path reaches `/api/ai/jobs/{job_id}`
        // (testdata/golden/api/route_order.json).
        .route(
            "/api/ai/jobs/stream",
            get(jobs::jobs_stream).delete(jobs::delete_job_shadowed),
        )
        .route(
            "/api/ai/jobs/active/{album_id}",
            get(jobs::active_job_for_album).post(jobs::post_shadowed_by_active),
        )
        .route(
            "/api/ai/jobs/{job_id}",
            get(jobs::get_job).delete(jobs::delete_job),
        )
        .route(
            "/api/ai/jobs/{job_id}/acted-on",
            post(jobs::mark_job_acted_on),
        )
        .route("/api/ai/jobs/{job_id}/retry", post(jobs::retry_job))
        // Not Python app routes: the cover preview proxy ([`EXTRA_ROUTES`]).
        .route(
            preview::COVER_PREVIEW_ROUTE,
            get(preview::job_cover_preview),
        )
        .route(
            preview::ALBUM_COVER_PREVIEW_ROUTE,
            get(preview::album_cover_preview),
        )
        .route("/api/ai/queue/resume", post(jobs::resume_queue))
        .route("/api/ai/providers", get(jobs::ai_providers))
        .route("/", get(system::spa_root))
        .route("/{*full_path}", get(system::spa_fallback));
    for (method, path, _) in STUBS {
        t = t.route(path, on(method, system::not_implemented));
    }
    t.finish()
}

/// The router's route table: every registered (method, path) with the
/// query parameters its handler reads, in registration order. Built from
/// the same calls that register the handlers (see [`route_table`]).
pub fn route_table() -> Vec<RouteRow> {
    routes().1
}

/// [`router`] with the optional layers. With a launch gate the order,
/// outermost first, is Host / Origin check → launch token → Starlette-style routing
/// → routes (see [`launch`]).
pub fn router_with(state: AppState, opts: RouterOptions) -> Router {
    let (r, _) = routes();
    let frontend = state.0.frontend_dir.is_some();
    let outer_state = state.clone();
    let inner = r
        .route_layer(middleware::from_fn(tag_route))
        .method_not_allowed_fallback(system::method_not_allowed)
        .fallback(system::unmatched)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(
            state,
            host_check::host_origin_guard,
        ));
    // Route on the decoded path, refuse HEAD and set the
    // Python app's `Allow` (starlette_routing).
    let outer = Router::new()
        .fallback(move |req: Request| starlette_routing::dispatch(inner.clone(), frontend, req));
    match opts.launch {
        None => outer,
        Some(gate) => outer
            .layer(middleware::from_fn_with_state(gate, launch::launch_guard))
            .layer(middleware::from_fn_with_state(
                outer_state,
                host_check::host_origin_guard,
            )),
    }
}
