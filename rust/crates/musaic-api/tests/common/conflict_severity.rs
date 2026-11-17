//! Deliberate change: an `inconsistent-artist-sort` finding whose `state` is
//! `conflicting` (one artist, two different sort values) has severity `warn`;
//! the Python app recorded it as `info`. The other states stay `info`.

use serde_json::Value;

fn conflicting(o: &serde_json::Map<String, Value>) -> bool {
    if let Some(d) = o.get("detail") {
        return d["state"] == "conflicting";
    }
    o.get("detail_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .is_some_and(|d| d["state"] == "conflicting")
}

/// Raise every such recorded finding in `v` to `warn`; returns how many
/// changed.
pub fn apply(v: &mut Value) -> usize {
    match v {
        Value::Array(a) => a.iter_mut().map(apply).sum(),
        Value::Object(o) => {
            let mut n = 0;
            if o.get("check_id").and_then(Value::as_str) == Some("inconsistent-artist-sort")
                && o.get("severity").and_then(Value::as_str) == Some("info")
                && conflicting(o)
            {
                o.insert("severity".into(), Value::String("warn".into()));
                n += 1;
            }
            n + o.values_mut().map(apply).sum::<usize>()
        }
        _ => 0,
    }
}
