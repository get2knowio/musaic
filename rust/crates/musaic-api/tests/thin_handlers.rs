//! FR-021 and FR-007: a route file is a thin adapter. It parses, calls
//! one operation, and renders; it takes no work guard, publishes nothing, sends no
//! notification, and reaches into the app only through `ops()`.

use std::path::PathBuf;

use regex::Regex;

/// `musaic_services::` paths a converted file may still name: pure helpers that are
/// not operations. Add an entry only with a comment saying why.
const ALLOWED_SERVICES_PATHS: &[&str] = &[];

/// Methods a handler may call on the app state besides `ops()` and
/// `shutdown_signal()`: the HTTP-only settings `AppState` carries.
const ALLOWED_STATE_METHODS: &[&str] = &[
    "ops",
    "shutdown_signal",
    "frontend_dir",
    "ping_interval",
    "allowed_hosts",
    "cover_body_limit",
    "restore_upload_limit",
];

const FORBIDDEN: &[&str] = &[
    "begin_work",
    ".gate",
    ".publish(",
    "ai_jobs_changed",
    "notify_",
    "st.0.",
];

fn violations(file: &str, src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for w in FORBIDDEN {
        if src.contains(w) {
            out.push(format!("{file}: contains `{w}`"));
        }
    }
    let path =
        Regex::new(r"musaic_services::((?:[A-Za-z_][A-Za-z0-9_]*::)*[A-Za-z_][A-Za-z0-9_]*)")
            .unwrap();
    for c in path.captures_iter(src) {
        let p = &c[1];
        if !ALLOWED_SERVICES_PATHS.contains(&p) {
            out.push(format!("{file}: names `musaic_services::{p}`"));
        }
    }
    let call = Regex::new(r"\b(?:st|state|app)\.([A-Za-z_][A-Za-z0-9_]*)\(").unwrap();
    for c in call.captures_iter(src) {
        let m = &c[1];
        if !ALLOWED_STATE_METHODS.contains(&m) {
            out.push(format!("{file}: calls `.{m}(` on the app state"));
        }
    }
    out
}

#[test]
fn converted_route_files_are_thin() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/routes");
    let mut found = Vec::new();
    let mut seen = 0;
    // Every file under routes/ but mod.rs, so a new one is covered unlisted.
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if p.extension().is_none_or(|x| x != "rs") || name == "mod.rs" {
            continue;
        }
        let src =
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{name} is not readable: {e}"));
        found.extend(violations(&name, &src));
        seen += 1;
    }
    assert!(seen > 0, "no route files found under {}", dir.display());
    assert!(found.is_empty(), "not thin:\n{}", found.join("\n"));
}

#[test]
fn the_check_notices_a_violation() {
    assert!(!violations("x.rs", "let g = st.begin_work()?;").is_empty());
    assert!(!violations("x.rs", "st.0.bus.publish(e);").is_empty());
    assert!(!violations("x.rs", "use musaic_services::scan::ScanService;").is_empty());
    assert!(!violations("x.rs", "st.work_snapshot()").is_empty());
    assert!(violations("x.rs", "st.ops().system().health()").is_empty());
}
