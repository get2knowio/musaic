//! Backend text names AI only when a provider is ready (a deliberate
//! change), as an enumerated delta for the Python app comparisons.
//!
//! These tests run the build's default provider (`musaic_ai::Unconfigured`,
//! never ready), so every response is in the no-provider wording. The delta
//! turns the Python app's record into that wording, and nothing else:
//!
//! - a check description listed in `musaic_checks::wording::NEUTRAL_DESCRIPTIONS`
//!   (matched exactly against the registry's original text) becomes the neutral one;
//! - the resolver's trace note `UNANCHORED_NOTE` becomes `UNANCHORED_NOTE_NEUTRAL`;
//! - the `summary` of a tags-out-of-sync finding ending with `NO_ANCHOR_SUFFIX`
//!   ends with `NO_ANCHOR_SUFFIX_NEUTRAL`.
//!
//! [`json`] rewrites a JSON value; [`text`] rewrites SSE / JSON text, where a
//! string may be written with or without `\uXXXX` escapes. Both return how
//! many replacements they made so a caller can require the delta to occur.
//! With a ready provider the Python app's record stands unchanged (pinned in
//! musaic-checks golden_pure.rs, musaic-ai golden_units.rs / golden_worker.rs
//! and this crate's ai_wording.rs).
#![allow(dead_code)]

use musaic_checks::Registry;
use musaic_checks::checks::tags_out_of_sync::{NO_ANCHOR_SUFFIX, NO_ANCHOR_SUFFIX_NEUTRAL};
use musaic_checks::wording::NEUTRAL_DESCRIPTIONS;
use musaic_resolve::resolution::{UNANCHORED_NOTE, UNANCHORED_NOTE_NEUTRAL};
use serde_json::Value;

/// (original text, no-provider text) for whole strings.
pub fn whole_strings() -> Vec<(&'static str, &'static str)> {
    let reg = Registry::default();
    let mut out = vec![(UNANCHORED_NOTE, UNANCHORED_NOTE_NEUTRAL)];
    for (id, neutral) in NEUTRAL_DESCRIPTIONS {
        let m = reg
            .all_meta()
            .into_iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{id}"));
        out.push((m.description, neutral));
    }
    out
}

/// The Python app's JSON value in the no-provider wording.
pub fn json(v: &mut Value) -> usize {
    let pairs = whole_strings();
    walk(v, &pairs)
}

fn walk(v: &mut Value, pairs: &[(&str, &str)]) -> usize {
    match v {
        Value::String(s) => match pairs.iter().find(|(o, _)| s == o) {
            Some((_, n)) => {
                *s = (*n).to_string();
                1
            }
            None => 0,
        },
        Value::Array(a) => a.iter_mut().map(|x| walk(x, pairs)).sum(),
        Value::Object(o) => {
            let mut n = 0;
            if o.get("check_id").and_then(Value::as_str) == Some("tags-out-of-sync")
                && let Some(Value::String(s)) = o.get_mut("summary")
                && let Some(head) = s.strip_suffix(NO_ANCHOR_SUFFIX)
            {
                *s = format!("{head}{NO_ANCHOR_SUFFIX_NEUTRAL}");
                n += 1;
            }
            n + o.values_mut().map(|x| walk(x, pairs)).sum::<usize>()
        }
        _ => 0,
    }
}

/// JSON string-literal body of `s` (no quotes), raw or `\uXXXX`-escaped.
fn literals(s: &str) -> [String; 2] {
    let raw = serde_json::to_string(s).unwrap();
    let raw = raw[1..raw.len() - 1].to_string();
    let mut ascii = String::new();
    for c in raw.chars() {
        if c.is_ascii() {
            ascii.push(c);
        } else {
            let mut b = [0u16; 2];
            for u in c.encode_utf16(&mut b) {
                ascii.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    [raw, ascii]
}

/// The Python app's SSE / JSON text in the no-provider wording: each whole
/// string (quoted) and each tags-out-of-sync summary suffix (before its
/// closing quote) replaced in either encoding.
pub fn text(t: &str) -> (String, usize) {
    let mut out = t.to_string();
    let mut n = 0;
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (o, nw) in whole_strings() {
        for (a, b) in literals(o).into_iter().zip(literals(nw)) {
            pairs.push((format!("\"{a}\""), format!("\"{b}\"")));
        }
    }
    for (a, b) in literals(NO_ANCHOR_SUFFIX)
        .into_iter()
        .zip(literals(NO_ANCHOR_SUFFIX_NEUTRAL))
    {
        pairs.push((format!("{a}\""), format!("{b}\"")));
    }
    for (a, b) in pairs {
        let c = out.matches(&a).count();
        if c > 0 {
            out = out.replace(&a, &b);
            n += c;
        }
    }
    (out, n)
}
