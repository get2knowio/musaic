//! Every (method, path) of the Python app's OpenAPI document
//! (testdata/golden/api/routes.json) is a route of
//! the router with the same method; the routes not built yet are exactly
//! the 501 stubs listed in `musaic_api::STUBS`. Routes the Python app does not
//! have are exactly `musaic_api::EXTRA_ROUTES`, each registered.
//!
//! The router's own table (`musaic_api::route_table()`, recorded by the
//! same calls that register the handlers, see src/route_table.rs) equals
//! the Python app's inventory in method, path (so path-parameter names) and
//! query-parameter names, apart from the explicitly listed EXTRA_ROUTES
//! (decided unknowns) and ROUTING_ALIASES (reproductions of Starlette's
//! first-match routing, not routes on the wire). Handlers can read only the
//! query parameters their route declares (debug assertion in
//! `lax::QueryParams::get`), so a renamed or added parameter fails here or
//! in the first test that reaches the handler.

mod common;

use std::collections::BTreeSet;

use axum::body::Body;
use axum::http::Request;
use common::{Sandbox, golden};
use musaic_api::{
    EXTRA_ROUTES, PLACEHOLDERS, ROUTING_ALIASES, RouteTag, STUBS, route_table, router,
};
use tower::ServiceExt;

fn axum_path(python: &str) -> String {
    python.replace("{full_path}", "{*full_path}")
}

fn concrete(python: &str) -> String {
    let mut out = String::new();
    let mut in_param = false;
    for c in python.chars() {
        match c {
            '{' => {
                in_param = true;
                out.push('1');
            }
            '}' => in_param = false,
            c if !in_param => out.push(c),
            _ => {}
        }
    }
    out
}

/// Status and matched route, without reading the body (streams never end).
async fn probe(app: &axum::Router, method: &str, path: &str) -> (u16, Option<String>) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("host", common::HOST)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let tag = resp.extensions().get::<RouteTag>().map(|t| t.0.clone());
    (resp.status().as_u16(), tag)
}

fn python_routes() -> Vec<(String, String)> {
    golden("routes.json")["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["method"].as_str().unwrap().to_string(),
                r["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_python_route_is_registered_with_its_method() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    let routes = python_routes();
    assert_eq!(routes.len(), 67, "the Python app's OpenAPI document");
    let stubs: BTreeSet<(String, String)> = STUBS
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect();
    let mut missing = Vec::new();
    for (method, path) in &routes {
        let (status, tag) = probe(&app, method, &concrete(path)).await;
        if tag.as_deref() != Some(axum_path(path).as_str()) {
            missing.push(format!("{method} {path}: matched {tag:?} ({status})"));
            continue;
        }
        let is_stub = stubs.contains(&(method.clone(), path.clone()));
        assert_eq!(
            status == 501,
            is_stub,
            "{method} {path}: status {status}, stub {is_stub}"
        );
    }
    assert!(
        missing.is_empty(),
        "routes not registered:\n{}",
        missing.join("\n")
    );
    // Stubs and placeholders name only Python app routes.
    let all: BTreeSet<(String, String)> = routes.into_iter().collect();
    for (m, p, why) in STUBS.iter().chain(PLACEHOLDERS) {
        assert!(
            all.contains(&(m.to_string(), p.to_string())),
            "{m} {p} ({why}) is not a Python app route"
        );
    }
}

// The exact list of routes not built: none. All 67 Python app routes
// are implemented (the kept AI-queue routes: tests/research_jobs_http.rs; the
// duplicate-tracks canonical lookup, the last stub:
// tests/recording_canonical_http.rs).
#[test]
fn the_stub_list_is_exactly_this() {
    let got: Vec<String> = STUBS.iter().map(|(m, p, _)| format!("{m} {p}")).collect();
    assert!(got.is_empty(), "stubs: {got:?}");
    assert!(
        PLACEHOLDERS.is_empty(),
        "the fingerprint pool routes are wired"
    );
}

// A static check-id segment does not shadow the generic routes
// (FastAPI matched in registration order; axum must fall back to the
// parameterised route).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn static_check_segments_fall_back_to_generic_routes() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    for (method, path, want) in [
        (
            "POST",
            "/api/checks/duplicate-tracks/findings/5/ignore",
            "/api/checks/{check_id}/findings/{finding_id}/ignore",
        ),
        (
            "POST",
            "/api/checks/filename-mismatch/findings/5/unignore",
            "/api/checks/{check_id}/findings/{finding_id}/unignore",
        ),
        ("GET", "/api/checks/stream", "/api/checks/stream"),
        ("GET", "/api/checks/sweep", "/api/checks/sweep"),
        ("GET", "/api/checks/findings", "/api/checks/findings"),
        (
            "GET",
            "/api/checks/duplicate-tracks",
            "/api/checks/{check_id}",
        ),
        ("GET", "/api/ai/jobs/stream", "/api/ai/jobs/stream"),
    ] {
        let (_, tag) = probe(&app, method, path).await;
        assert_eq!(tag.as_deref(), Some(want), "{method} {path}");
    }
}

// The routes this build adds to the Python app's, pinned. A
// new one must be added here (and documented as a deliberate addition).
#[test]
fn the_extra_route_list_is_exactly_this() {
    let got: Vec<String> = EXTRA_ROUTES
        .iter()
        .map(|(m, p, _)| format!("{m} {p}"))
        .collect();
    assert_eq!(
        got,
        [
            "GET /api/ai/jobs/{job_id}/cover-preview",
            // Deliberate addition: a job-less cover preview (issues #183, #184).
            "GET /api/albums/{album_id}/cover-preview",
            // Deliberate addition: cancelling a running scan (issue #193).
            "POST /api/scan/cancel",
            // Deliberate addition: following a scan without starting one
            // (issue #332).
            "GET /api/scan/follow",
            // Deliberate addition: the background check sweep's progress
            // (issue #187).
            "GET /api/checks/sweep",
            // Deliberate addition: the multi-disc conversion preview
            // (issue #296).
            "GET /api/checks/multi-disc-layout/findings/{finding_id}/conversion-preview",
            // Deliberate addition: converting a multi-disc set kept in one
            // folder (issue #296).
            "POST /api/checks/multi-disc-layout/findings/{finding_id}/convert",
            // Deliberate addition: abandoning an unfinished multi-disc
            // conversion (issue #296).
            "POST /api/checks/multi-disc-layout/findings/{finding_id}/abandon-conversion",
            // Deliberate additions: the one-time AcoustID prompt's dismissal
            // (issue #194), kept on the server.
            "GET /api/prompts/acoustid",
            "POST /api/prompts/acoustid/dismiss",
            // Deliberate additions: the theme and tile size, kept on the server.
            "GET /api/ui-prefs",
            "PUT /api/ui-prefs",
            // Deliberate additions: the agent bridge (spec 003): the webview's
            // management routes, and the agent CLI's own route family.
            "GET /api/research/groups/{group}",
            // Deliberate addition (spec 005): the delegated work in Review.
            "GET /api/research/work",
            "POST /api/research/{id}/preview/{digest}",
            "POST /api/research/{id}/decision",
            // Deliberate additions (spec 006): the research batches bulk
            // Fix all makes, one entry each, with their cancel.
            "GET /api/research/batches",
            "GET /api/research/batches/{batch_id}",
            "POST /api/research/batches/{batch_id}/cancel",
            // Deliberate addition (spec 006, FR-036): Cancel on a research
            // request's card cancels the request behind it.
            "POST /api/ai/jobs/{job_id}/cancel",
            "GET /api/agent/pairings",
            "POST /api/agent/pairings",
            "DELETE /api/agent/pairings/{pairing_id}",
            "PUT /api/agent/pairings/{pairing_id}/policy",
            "GET /api/agent/pairings/{pairing_id}/activity",
            "GET /api/agent/approvals",
            "POST /api/agent/approvals/{approval_id}/approve",
            "POST /api/agent/approvals/{approval_id}/deny",
            "GET /api/agent/changes",
            // Deliberate addition (spec 005): per-pairing diagnostics.
            "GET /api/agent/diagnostics",
            "POST /api/agent/changes/{change_id}/undo",
            "POST /__musaic/agent/v1/hello",
            "POST /__musaic/agent/v1/exchange",
            "GET /__musaic/agent/v1/tools",
            "POST /__musaic/agent/v1/tools/{tool_id}",
            "POST /__musaic/agent/v1/tasks/create",
            "POST /__musaic/agent/v1/tasks/get",
            "POST /__musaic/agent/v1/tasks/update",
            "POST /__musaic/agent/v1/tasks/cancel",
            // Deliberate additions (spec 005): events and host observations.
            "POST /__musaic/agent/v1/events/list",
            "POST /__musaic/agent/v1/events/poll",
            "POST /__musaic/agent/v1/session/observe",
            "GET /__musaic/agent/v1/session",
        ]
    );
}

// Every extra route is registered with its method and is
// not a Python app route (where the Python app answered a GET on that path from
// its SPA catch-all: a JSON 404 under /api/).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extra_routes_are_registered_and_not_python_routes() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    let python: BTreeSet<(String, String)> = python_routes().into_iter().collect();
    for (method, path, why) in EXTRA_ROUTES {
        assert!(
            !python.contains(&(method.to_string(), path.to_string())),
            "{method} {path} is a Python app route ({why})"
        );
        let (_, tag) = probe(&app, method, &concrete(path)).await;
        assert_eq!(tag.as_deref(), Some(*path), "{method} {path}");
    }
}

/// (method, path in the Python app's spelling) -> (path params, sorted query
/// params), from testdata/golden/api/routes.json.
type Inventory = std::collections::BTreeMap<(String, String), (Vec<String>, Vec<String>)>;

fn python_inventory() -> Inventory {
    golden("routes.json")["routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let names = |k: &str| -> Vec<String> {
                let mut v: Vec<String> = r[k]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|x| x.as_str().unwrap().to_string())
                    .collect();
                if k == "query_params" {
                    v.sort();
                }
                v
            };
            (
                (
                    r["method"].as_str().unwrap().to_string(),
                    r["path"].as_str().unwrap().to_string(),
                ),
                (names("path_params"), names("query_params")),
            )
        })
        .collect()
}

fn python_spelling(axum: &str) -> String {
    axum.replace("{*full_path}", "{full_path}")
}

fn path_params(python_path: &str) -> Vec<String> {
    python_path
        .split('/')
        .filter_map(|seg| seg.strip_prefix('{')?.strip_suffix('}'))
        .map(str::to_string)
        .collect()
}

// The router's own route table (not a hand-kept list) equals the
// Python app's OpenAPI inventory in methods, paths, path parameters and query
// parameters. The only differences are listed: EXTRA_ROUTES (each against a
// decided unknown) and ROUTING_ALIASES (no new behaviour on the wire;
// pinned by tests/starlette_routing.rs against testdata/golden/api/route_order.json).
// Removals: none (the Python app's /openapi.json, /docs, /docs/oauth2-redirect
// and /redoc are not in its OpenAPI document; deliberately not served).
// The desktop launch path (`launch::LAUNCH_PATH`) is a
// middleware in front of the router, not a route, and is not in the table.
#[test]
fn route_table_equals_the_python_inventory() {
    let python = python_inventory();
    assert_eq!(python.len(), 67, "the Python app's OpenAPI document");
    let table = route_table();
    let extra: BTreeSet<(String, String)> = EXTRA_ROUTES
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect();
    let aliases: BTreeSet<(String, String)> = ROUTING_ALIASES
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect();

    let mut seen = BTreeSet::new();
    let mut ours: Inventory = Inventory::new();
    let mut got_extra = BTreeSet::new();
    let mut got_aliases = BTreeSet::new();
    for row in &table {
        let key = (row.method.to_string(), row.path.to_string());
        assert!(seen.insert(key.clone()), "{key:?} registered twice");
        if extra.contains(&key) {
            got_extra.insert(key);
            continue;
        }
        if aliases.contains(&key) {
            assert!(row.query.is_empty(), "{key:?}: an alias reads no query");
            got_aliases.insert(key);
            continue;
        }
        let path = python_spelling(row.path);
        let mut query: Vec<String> = row.query.iter().map(|q| q.to_string()).collect();
        query.sort();
        let n = query.len();
        query.dedup();
        assert_eq!(n, query.len(), "{key:?}: query parameter declared twice");
        ours.insert(
            (row.method.to_string(), path.clone()),
            (path_params(&path), query),
        );
    }
    assert_eq!(got_extra, extra, "every EXTRA_ROUTES entry is in the table");
    assert_eq!(
        got_aliases, aliases,
        "every ROUTING_ALIASES entry is in the table"
    );

    let only_python: Vec<_> = python.keys().filter(|k| !ours.contains_key(*k)).collect();
    let only_rust: Vec<_> = ours.keys().filter(|k| !python.contains_key(*k)).collect();
    assert!(
        only_python.is_empty() && only_rust.is_empty(),
        "routes only in the Python app: {only_python:?}; only in the Rust table: {only_rust:?}"
    );
    // Query parameters this build adds to a Python app route, deliberately.
    let added_query: &[(&str, &str, &str)] = &[
        // Spec 006: bulk Fix all may send what the resolver cannot settle
        // for research (`research=1`).
        ("POST", "/api/checks/{check_id}/quick-fix-all", "research"),
    ];
    for (key, (opath, oquery)) in &python {
        let (rpath, rquery) = &ours[key];
        assert_eq!(rpath, opath, "{key:?}: path parameters");
        let rquery: Vec<String> = rquery
            .iter()
            .filter(|q| {
                !added_query
                    .iter()
                    .any(|(m, p, n)| (*m, *p, *n) == (key.0.as_str(), key.1.as_str(), q.as_str()))
            })
            .cloned()
            .collect();
        assert_eq!(&rquery, oquery, "{key:?}: query parameters");
    }
    for (m, p, n) in added_query {
        let (_, rquery) = &ours[&(m.to_string(), p.to_string())];
        assert!(rquery.iter().any(|q| q == n), "{m} {p}: {n} is declared");
    }
    // Guard against a vacuous pass: the golden does carry query parameters.
    assert!(python.values().any(|(_, q)| !q.is_empty()));
}

// Each alias answers as the Python app route it names, which exists.
#[test]
fn routing_aliases_name_python_routes() {
    let python = python_inventory();
    let got: Vec<String> = ROUTING_ALIASES
        .iter()
        .map(|(m, p, _)| format!("{m} {p}"))
        .collect();
    assert_eq!(
        got,
        [
            "DELETE /api/ai/jobs/stream",
            "POST /api/ai/jobs/active/{album_id}"
        ]
    );
    for (_, _, answered_by) in ROUTING_ALIASES {
        let (m, p) = answered_by.split_once(' ').unwrap();
        assert!(
            python.contains_key(&(m.to_string(), p.to_string())),
            "{answered_by} is not a Python app route"
        );
    }
    for (m, p, _) in ROUTING_ALIASES {
        assert!(!python.contains_key(&(m.to_string(), p.to_string())));
    }
}

// The table is what the router serves: every row, probed with its
// method on a concrete path, is matched by its own pattern.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_table_row_is_served_by_its_own_route() {
    let sb = Sandbox::new();
    let app = router(sb.state_no_lifespan());
    let mut wrong = Vec::new();
    for row in route_table() {
        let python_path = python_spelling(row.path);
        let (status, tag) = probe(&app, row.method, &concrete(&python_path)).await;
        if tag.as_deref() != Some(row.path) {
            wrong.push(format!(
                "{} {}: matched {tag:?} ({status})",
                row.method, row.path
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

// A handler cannot read a query parameter its route did not
// declare (debug builds: every test, and any debug binary).
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "not declared for this route")]
fn reading_an_undeclared_query_parameter_panics() {
    let q = musaic_api::lax::QueryParams(vec![("forse".into(), "1".into())], Some(&["force"]));
    let _ = q.get("forse");
}

// Declared names are read normally.
#[test]
fn reading_a_declared_query_parameter_works() {
    let q = musaic_api::lax::QueryParams(
        vec![("force".into(), "0".into()), ("force".into(), "1".into())],
        Some(&["force"]),
    );
    assert_eq!(q.get("force"), Some("1"));
}
