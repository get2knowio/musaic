//! An ordered JSON value that renders exactly like Python's `json.dumps`
//! with default arguments (`", "` / `": "` separators, `ensure_ascii=True`,
//! float `repr`). Every payload this crate produces is one of these, so the
//! HTTP layer can write SSE frames byte-for-byte like the Python app's
//! `services.sse.sse_event`, and tests can compare via [`Json::to_value`].

use musaic_core::pycompat::json_string;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Keys in insertion order (a Python dict).
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn obj<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// `d[key]` for an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `d[key] = value`: replaces in place, or appends.
    pub fn set(&mut self, key: &str, value: Json) {
        if let Json::Obj(pairs) = self {
            match pairs.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key.to_string(), value)),
            }
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// `json.dumps(self)`.
    pub fn dumps(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Int(n) => out.push_str(&n.to_string()),
            Json::Float(f) => out.push_str(&py_float_repr(*f)),
            Json::Str(s) => out.push_str(&json_string(s)),
            Json::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&json_string(k));
                    out.push_str(": ");
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    /// The same value as an (unordered) `serde_json::Value`.
    pub fn to_value(&self) -> serde_json::Value {
        use serde_json::Value;
        match self {
            Json::Null => Value::Null,
            Json::Bool(b) => Value::Bool(*b),
            Json::Int(n) => Value::from(*n),
            Json::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Json::Str(s) => Value::String(s.clone()),
            Json::Arr(items) => Value::Array(items.iter().map(Json::to_value).collect()),
            Json::Obj(pairs) => Value::Object(
                pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect(),
            ),
        }
    }
}

impl From<&str> for Json {
    fn from(s: &str) -> Json {
        Json::Str(s.to_string())
    }
}

impl From<String> for Json {
    fn from(s: String) -> Json {
        Json::Str(s)
    }
}

impl From<bool> for Json {
    fn from(b: bool) -> Json {
        Json::Bool(b)
    }
}

impl From<i64> for Json {
    fn from(n: i64) -> Json {
        Json::Int(n)
    }
}

impl From<usize> for Json {
    fn from(n: usize) -> Json {
        Json::Int(n as i64)
    }
}

impl From<f64> for Json {
    fn from(f: f64) -> Json {
        Json::Float(f)
    }
}

impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Json {
        v.map_or(Json::Null, Into::into)
    }
}

/// Python `repr(float)`: shortest round-trip digits, `.0` on integral values,
/// exponent form below 1e-4 and from 1e16 (`1e+16`, `1.5e-05`).
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        // Rust's `{:e}` gives the shortest round-trip mantissa: "1.5e-5".
        let s = format!("{f:e}");
        let (mant, exp) = s.split_once('e').expect("exponent form");
        let exp: i32 = exp.parse().expect("exponent");
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", exp.abs());
    }
    let s = format!("{f}");
    if s.contains('.') { s } else { format!("{s}.0") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_render_like_python_repr() {
        assert_eq!(py_float_repr(3.0), "3.0");
        assert_eq!(py_float_repr(0.3), "0.3");
        assert_eq!(py_float_repr(-0.0), "-0.0");
        assert_eq!(py_float_repr(1794363720.4971569), "1794363720.4971569");
        assert_eq!(py_float_repr(1e16), "1e+16");
        assert_eq!(py_float_repr(1.5e-5), "1.5e-05");
        assert_eq!(py_float_repr(1e-4), "0.0001");
    }

    #[test]
    fn dumps_uses_python_separators() {
        let j = Json::obj([
            ("a", Json::from(1i64)),
            ("b", Json::Arr(vec![Json::Null, Json::from(true)])),
            ("c", Json::from("é")),
        ]);
        assert_eq!(
            j.dumps(),
            "{\"a\": 1, \"b\": [null, true], \"c\": \"\\u00e9\"}"
        );
    }
}
