//! FR-002: `docs/operations.md` has one inventory row for every route the router
//! registers, and no row the router lacks.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// `(method, path)` of each data row: a table line whose first two cells are
/// each a backticked string.
fn doc_rows(text: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix('|') else {
            continue;
        };
        let mut cells = rest.split('|').map(str::trim);
        let (Some(m), Some(p)) = (cells.next(), cells.next()) else {
            continue;
        };
        let tick = |s: &str| {
            s.strip_prefix('`')
                .and_then(|s| s.strip_suffix('`'))
                .map(str::to_string)
        };
        if let (Some(m), Some(p)) = (tick(m), tick(p)) {
            rows.push((m, p));
        }
    }
    rows
}

fn counts(items: impl IntoIterator<Item = (String, String)>) -> BTreeMap<(String, String), usize> {
    let mut m = BTreeMap::new();
    for k in items {
        *m.entry(k).or_insert(0) += 1;
    }
    m
}

#[test]
fn every_route_has_exactly_one_inventory_row() {
    let doc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../docs/operations.md");
    let text = std::fs::read_to_string(&doc)
        .unwrap_or_else(|e| panic!("{} is not readable: {e}", doc.display()));
    let in_doc = counts(doc_rows(&text));
    let in_router = counts(
        musaic_api::route_table()
            .into_iter()
            .map(|r| (r.method.to_string(), r.path.to_string())),
    );
    let mut problems = Vec::new();
    for (k, n) in &in_router {
        match in_doc.get(k) {
            None => problems.push(format!("no inventory row for {} {}", k.0, k.1)),
            Some(1) => {}
            Some(d) => problems.push(format!("{} {} has {d} rows", k.0, k.1)),
        }
        if *n != 1 {
            problems.push(format!("router lists {} {} {n} times", k.0, k.1));
        }
    }
    for k in in_doc.keys() {
        if !in_router.contains_key(k) {
            problems.push(format!("inventory row {} {} is not a route", k.0, k.1));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// One problem per operation-bearing row whose Operation cell is empty, or is not
/// exactly one `group.name` in backticks (SC-001); the Result, Errors and Streamed
/// cells must be filled too. A row whose group is "HTTP face only" has no operation.
fn row_problems(text: &str) -> Vec<String> {
    let ident = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_');
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix('|') else {
            continue;
        };
        let cells: Vec<&str> = rest
            .trim_end()
            .trim_end_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() < 9 || !cells[0].starts_with('`') || !cells[1].starts_with('`') {
            continue;
        }
        let at = format!("{} {}", cells[0], cells[1]);
        if cells[2] == "HTTP face only" {
            continue;
        }
        let op = cells[5];
        let one = op
            .strip_prefix('`')
            .and_then(|o| o.strip_suffix('`'))
            .and_then(|o| o.split_once('.'))
            .is_some_and(|(g, n)| ident(g) && ident(n));
        if !one {
            out.push(format!(
                "{at}: operation cell `{op}` is not exactly one operation"
            ));
        }
        for (i, name) in [(6, "result"), (7, "errors"), (8, "streamed")] {
            if cells[i].is_empty() || cells[i] == "-" {
                out.push(format!("{at}: {name} cell is empty"));
            }
        }
    }
    out
}

#[test]
fn every_operation_row_names_one_operation() {
    let doc = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../docs/operations.md");
    let text = std::fs::read_to_string(&doc).unwrap();
    let problems = row_problems(&text);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_row_check_notices_an_empty_or_double_operation() {
    let row = |op: &str| {
        format!("| `GET` | `/x` | g | thin adapter | m | {op} | R | none | no | default |\n")
    };
    assert!(row_problems(&row("`a.b`")).is_empty());
    assert!(!row_problems(&row("")).is_empty());
    assert!(!row_problems(&row("`a.b` + `a.c`")).is_empty());
    assert!(!row_problems(&row("`a.b`, `a.c`")).is_empty());
    let alias = "| `GET` | `/x` | HTTP face only | thin adapter | m | - | - | - | no | default |\n";
    assert!(row_problems(alias).is_empty());
}

#[test]
fn the_parser_reads_a_row() {
    let rows = doc_rows("| Method | Path |\n|---|---|\n| `GET` | `/x/{id}` | a | b |\n");
    assert_eq!(rows, vec![("GET".into(), "/x/{id}".into())]);
}
