//! An ordered JSON value that serialises exactly like Python's `json.dumps`
//! with default arguments (`ensure_ascii=True`, `", "` / `": "` separators,
//! `repr()` floats). Finding details are stored this way so the bytes in
//! `check_findings.detail_json` match what the Python app writes, key order
//! included.

use musaic_core::pycompat::json_string;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum PyJson {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<PyJson>),
    Dict(Vec<(String, PyJson)>),
}

impl PyJson {
    /// `json.dumps(self)`.
    pub fn dumps(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            PyJson::Null => out.push_str("null"),
            PyJson::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            PyJson::Int(i) => out.push_str(&i.to_string()),
            PyJson::Float(f) => out.push_str(&py_float_repr(*f)),
            PyJson::Str(s) => out.push_str(&json_string(s)),
            PyJson::List(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    v.write(out);
                }
                out.push(']');
            }
            PyJson::Dict(items) => {
                out.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
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

    /// The same value as a `serde_json::Value` (key order not kept).
    pub fn to_value(&self) -> Value {
        match self {
            PyJson::Null => Value::Null,
            PyJson::Bool(b) => Value::Bool(*b),
            PyJson::Int(i) => Value::from(*i),
            PyJson::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            PyJson::Str(s) => Value::String(s.clone()),
            PyJson::List(items) => Value::Array(items.iter().map(PyJson::to_value).collect()),
            PyJson::Dict(items) => Value::Object(
                items
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect(),
            ),
        }
    }

    /// From a parsed value (object key order is serde_json's, i.e. sorted).
    pub fn from_value(v: &Value) -> PyJson {
        match v {
            Value::Null => PyJson::Null,
            Value::Bool(b) => PyJson::Bool(*b),
            Value::Number(n) => match n.as_i64() {
                Some(i) => PyJson::Int(i),
                None => PyJson::Float(n.as_f64().unwrap_or(0.0)),
            },
            Value::String(s) => PyJson::Str(s.clone()),
            Value::Array(a) => PyJson::List(a.iter().map(PyJson::from_value).collect()),
            Value::Object(o) => PyJson::Dict(
                o.iter()
                    .map(|(k, v)| (k.clone(), PyJson::from_value(v)))
                    .collect(),
            ),
        }
    }

    pub fn get(&self, key: &str) -> Option<&PyJson> {
        match self {
            PyJson::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PyJson::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Build a dict from `(key, value)` pairs, keeping their order.
pub fn dict<const N: usize>(items: [(&str, PyJson); N]) -> PyJson {
    PyJson::Dict(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

impl From<&str> for PyJson {
    fn from(s: &str) -> Self {
        PyJson::Str(s.to_string())
    }
}
impl From<String> for PyJson {
    fn from(s: String) -> Self {
        PyJson::Str(s)
    }
}
impl From<&String> for PyJson {
    fn from(s: &String) -> Self {
        PyJson::Str(s.clone())
    }
}
impl From<bool> for PyJson {
    fn from(b: bool) -> Self {
        PyJson::Bool(b)
    }
}
impl From<i64> for PyJson {
    fn from(i: i64) -> Self {
        PyJson::Int(i)
    }
}
impl From<f64> for PyJson {
    fn from(f: f64) -> Self {
        PyJson::Float(f)
    }
}
impl<T: Into<PyJson>> From<Option<T>> for PyJson {
    fn from(o: Option<T>) -> Self {
        o.map_or(PyJson::Null, Into::into)
    }
}
impl<T: Into<PyJson>> From<Vec<T>> for PyJson {
    fn from(v: Vec<T>) -> Self {
        PyJson::List(v.into_iter().map(Into::into).collect())
    }
}

/// Python `repr(float)` (what `json.dumps` writes for a float).
pub fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    // Shortest round-trip digits and the decimal exponent.
    let sci = format!("{:e}", x.abs());
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let n = digits.len() as i32;
    let sign = if x.is_sign_negative() { "-" } else { "" };
    let body = if (-4..16).contains(&exp) {
        if exp >= n - 1 {
            format!("{digits}{}.0", "0".repeat((exp + 1 - n) as usize))
        } else if exp >= 0 {
            let (a, b) = digits.split_at((exp + 1) as usize);
            format!("{a}.{b}")
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let (a, b) = digits.split_at(1);
        let frac = if b.is_empty() {
            String::new()
        } else {
            format!(".{b}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{a}{frac}e{esign}{:02}", exp.abs())
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_repr_like_python() {
        // Values and their CPython repr() (documented float formatting rules).
        for (x, s) in [
            (10.0, "10.0"),
            (0.1, "0.1"),
            (180.5, "180.5"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-2.5, "-2.5"),
            (0.0, "0.0"),
            (123456789.125, "123456789.125"),
        ] {
            assert_eq!(py_float_repr(x), s);
        }
    }
}
