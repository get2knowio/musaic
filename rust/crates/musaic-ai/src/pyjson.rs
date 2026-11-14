//! Python's `json` module and `str()` / `repr()` for the values the AI layer
//! handles: an ordered value ([`Py`], a Python `dict` keeps insertion order),
//! `json.loads` (the C scanner's grammar: `NaN` / `Infinity` accepted,
//! control characters in strings refused, a repeated key keeps its first
//! position and its last value) and the three `json.dumps` styles the Python app
//! uses: the default (`", "` / `": "`, ASCII only), `indent=2` (the prompt
//! payload) and Starlette's compact response body (`","` / `":"`, UTF-8).
//! Integers beyond `i64` are kept as their digits.

use musaic_core::pycompat::{is_py_space, json_string};
use musaic_services::Json;
use musaic_services::json::py_float_repr;

/// An ordered JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Py {
    Null,
    Bool(bool),
    Int(i64),
    /// An integer outside `i64`, as its decimal digits.
    BigInt(String),
    Float(f64),
    Str(String),
    List(Vec<Py>),
    /// Keys in insertion order.
    Dict(Vec<(String, Py)>),
}

impl Py {
    pub fn dict<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Py)>) -> Py {
        Py::Dict(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn str(s: impl Into<String>) -> Py {
        Py::Str(s.into())
    }

    /// `Some(s)` -> `Str`, `None` -> `Null`.
    pub fn opt_str(s: Option<impl Into<String>>) -> Py {
        s.map_or(Py::Null, |s| Py::Str(s.into()))
    }

    /// `d.get(key)` for a dict (`None` for a missing key or a non-dict).
    pub fn get(&self, key: &str) -> Option<&Py> {
        match self {
            Py::Dict(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `d[key] = value`: replaces in place, or appends.
    pub fn set(&mut self, key: &str, value: Py) {
        if let Py::Dict(pairs) = self {
            match pairs.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key.to_string(), value)),
            }
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Py::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Py::Null => false,
            Py::Bool(b) => *b,
            Py::Int(n) => *n != 0,
            Py::BigInt(_) => true,
            Py::Float(f) => *f != 0.0,
            Py::Str(s) => !s.is_empty(),
            Py::List(l) => !l.is_empty(),
            Py::Dict(d) => !d.is_empty(),
        }
    }

    /// `type(v).__name__`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Py::Null => "NoneType",
            Py::Bool(_) => "bool",
            Py::Int(_) | Py::BigInt(_) => "int",
            Py::Float(_) => "float",
            Py::Str(_) => "str",
            Py::List(_) => "list",
            Py::Dict(_) => "dict",
        }
    }

    /// `v in (None, "")`.
    pub fn is_none_or_empty_str(&self) -> bool {
        matches!(self, Py::Null) || matches!(self, Py::Str(s) if s.is_empty())
    }

    /// `json.dumps(v)`.
    pub fn dumps(&self) -> String {
        let mut out = String::new();
        write(self, &Style::DEFAULT, 0, &mut out);
        out
    }

    /// `json.dumps(v, ensure_ascii=False)`.
    pub fn dumps_unicode(&self) -> String {
        let mut out = String::new();
        write(self, &Style::UNICODE, 0, &mut out);
        out
    }

    /// `json.dumps(v, indent=2)`.
    pub fn dumps_indent2(&self) -> String {
        let mut out = String::new();
        write(self, &Style::INDENT2, 0, &mut out);
        out
    }

    /// Starlette's `JSONResponse` body: `json.dumps(v, ensure_ascii=False,
    /// separators=(",", ":"))`.
    pub fn dumps_compact(&self) -> String {
        let mut out = String::new();
        write(self, &Style::COMPACT, 0, &mut out);
        out
    }

    /// `str(v)`.
    pub fn py_str(&self) -> String {
        match self {
            Py::Str(s) => s.clone(),
            other => other.py_repr(),
        }
    }

    /// `repr(v)`.
    pub fn py_repr(&self) -> String {
        match self {
            Py::Null => "None".into(),
            Py::Bool(true) => "True".into(),
            Py::Bool(false) => "False".into(),
            Py::Int(n) => n.to_string(),
            Py::BigInt(d) => d.clone(),
            Py::Float(f) if f.is_nan() => "nan".into(),
            Py::Float(f) if f.is_infinite() => if *f > 0.0 { "inf" } else { "-inf" }.into(),
            Py::Float(f) => py_float_repr(*f),
            Py::Str(s) => musaic_fsops::py_repr(s),
            Py::List(items) => format!(
                "[{}]",
                items.iter().map(Py::py_repr).collect::<Vec<_>>().join(", ")
            ),
            Py::Dict(pairs) => format!(
                "{{{}}}",
                pairs
                    .iter()
                    .map(|(k, v)| format!("{}: {}", musaic_fsops::py_repr(k), v.py_repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// The same value as an (unordered) `serde_json::Value` (big integers
    /// become floats).
    pub fn to_value(&self) -> serde_json::Value {
        use serde_json::Value;
        match self {
            Py::Null => Value::Null,
            Py::Bool(b) => Value::Bool(*b),
            Py::Int(n) => Value::from(*n),
            Py::BigInt(d) => d
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map_or(Value::Null, Value::Number),
            Py::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Py::Str(s) => Value::String(s.clone()),
            Py::List(items) => Value::Array(items.iter().map(Py::to_value).collect()),
            Py::Dict(pairs) => Value::Object(
                pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect(),
            ),
        }
    }

    /// The same value as musaic-services' ordered [`Json`] (big integers
    /// become floats).
    pub fn to_json(&self) -> Json {
        match self {
            Py::Null => Json::Null,
            Py::Bool(b) => Json::Bool(*b),
            Py::Int(n) => Json::Int(*n),
            Py::BigInt(d) => Json::Float(d.parse().unwrap_or(f64::NAN)),
            Py::Float(f) => Json::Float(*f),
            Py::Str(s) => Json::Str(s.clone()),
            Py::List(items) => Json::Arr(items.iter().map(Py::to_json).collect()),
            Py::Dict(pairs) => Json::Obj(
                pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_json()))
                    .collect(),
            ),
        }
    }

    /// From a `serde_json::Value` (object keys in the map's order).
    pub fn from_value(v: &serde_json::Value) -> Py {
        use serde_json::Value;
        match v {
            Value::Null => Py::Null,
            Value::Bool(b) => Py::Bool(*b),
            Value::Number(n) => match n.as_i64() {
                Some(i) => Py::Int(i),
                None => match n.as_u64() {
                    Some(u) => Py::BigInt(u.to_string()),
                    None => Py::Float(n.as_f64().unwrap_or(0.0)),
                },
            },
            Value::String(s) => Py::Str(s.clone()),
            Value::Array(a) => Py::List(a.iter().map(Py::from_value).collect()),
            Value::Object(m) => Py::Dict(
                m.iter()
                    .map(|(k, x)| (k.clone(), Py::from_value(x)))
                    .collect(),
            ),
        }
    }
}

impl From<&Json> for Py {
    fn from(j: &Json) -> Py {
        match j {
            Json::Null => Py::Null,
            Json::Bool(b) => Py::Bool(*b),
            Json::Int(n) => Py::Int(*n),
            Json::Float(f) => Py::Float(*f),
            Json::Str(s) => Py::Str(s.clone()),
            Json::Arr(items) => Py::List(items.iter().map(Py::from).collect()),
            Json::Obj(pairs) => Py::Dict(
                pairs
                    .iter()
                    .map(|(k, v)| (k.clone(), Py::from(v)))
                    .collect(),
            ),
        }
    }
}

impl From<&str> for Py {
    fn from(s: &str) -> Py {
        Py::Str(s.to_string())
    }
}

impl From<String> for Py {
    fn from(s: String) -> Py {
        Py::Str(s)
    }
}

impl From<bool> for Py {
    fn from(b: bool) -> Py {
        Py::Bool(b)
    }
}

impl From<i64> for Py {
    fn from(n: i64) -> Py {
        Py::Int(n)
    }
}

impl From<f64> for Py {
    fn from(f: f64) -> Py {
        Py::Float(f)
    }
}

impl<T: Into<Py>> From<Option<T>> for Py {
    fn from(v: Option<T>) -> Py {
        v.map_or(Py::Null, Into::into)
    }
}

// ---------------------------------------------------------------- dumps

struct Style {
    item_sep: &'static str,
    key_sep: &'static str,
    ensure_ascii: bool,
    indent: Option<usize>,
}

impl Style {
    const DEFAULT: Style = Style {
        item_sep: ", ",
        key_sep: ": ",
        ensure_ascii: true,
        indent: None,
    };
    const UNICODE: Style = Style {
        item_sep: ", ",
        key_sep: ": ",
        ensure_ascii: false,
        indent: None,
    };
    /// With an indent Python's default item separator is `","`.
    const INDENT2: Style = Style {
        item_sep: ",",
        key_sep: ": ",
        ensure_ascii: true,
        indent: Some(2),
    };
    const COMPACT: Style = Style {
        item_sep: ",",
        key_sep: ":",
        ensure_ascii: false,
        indent: None,
    };
}

/// `json.dumps` of a str with `ensure_ascii=False`.
fn json_string_unicode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn float_text(f: f64) -> String {
    // json's floatstr: repr, with NaN / Infinity spelled out.
    py_float_repr(f)
}

fn newline(style: &Style, level: usize, out: &mut String) {
    if let Some(n) = style.indent {
        out.push('\n');
        out.push_str(&" ".repeat(n * level));
    }
}

fn write(v: &Py, style: &Style, level: usize, out: &mut String) {
    let string = |s: &str| {
        if style.ensure_ascii {
            json_string(s)
        } else {
            json_string_unicode(s)
        }
    };
    match v {
        Py::Null => out.push_str("null"),
        Py::Bool(true) => out.push_str("true"),
        Py::Bool(false) => out.push_str("false"),
        Py::Int(n) => out.push_str(&n.to_string()),
        Py::BigInt(d) => out.push_str(d),
        Py::Float(f) => out.push_str(&float_text(*f)),
        Py::Str(s) => out.push_str(&string(s)),
        Py::List(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(style.item_sep);
                }
                newline(style, level + 1, out);
                write(x, style, level + 1, out);
            }
            newline(style, level, out);
            out.push(']');
        }
        Py::Dict(pairs) => {
            if pairs.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push_str(style.item_sep);
                }
                newline(style, level + 1, out);
                out.push_str(&string(k));
                out.push_str(style.key_sep);
                write(x, style, level + 1, out);
            }
            newline(style, level, out);
            out.push('}');
        }
    }
}

// ---------------------------------------------------------------- loads

/// `json.loads(s)`; `Err` with Python's message for anything it refuses.
pub fn loads(s: &str) -> Result<Py, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut p = Parser { s: &chars, i: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != chars.len() {
        return Err("Extra data".into());
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [char],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.i += 1;
        }
    }

    fn lit(&mut self, word: &str) -> bool {
        let w: Vec<char> = word.chars().collect();
        if self.s.len() >= self.i + w.len() && self.s[self.i..self.i + w.len()] == w[..] {
            self.i += w.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Py, String> {
        match self.peek() {
            None => Err("Expecting value".into()),
            Some('"') => {
                self.i += 1;
                self.string().map(Py::Str)
            }
            Some('{') => {
                self.i += 1;
                self.object()
            }
            Some('[') => {
                self.i += 1;
                self.array()
            }
            Some('n') if self.lit("null") => Ok(Py::Null),
            Some('t') if self.lit("true") => Ok(Py::Bool(true)),
            Some('f') if self.lit("false") => Ok(Py::Bool(false)),
            Some('N') if self.lit("NaN") => Ok(Py::Float(f64::NAN)),
            Some('I') if self.lit("Infinity") => Ok(Py::Float(f64::INFINITY)),
            Some('-') if self.lit("-Infinity") => Ok(Py::Float(f64::NEG_INFINITY)),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(_) => Err("Expecting value".into()),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Result<Py, String> {
        let start = self.i;
        if self.peek() == Some('-') {
            self.i += 1;
        }
        match self.peek() {
            Some('0') => self.i += 1,
            Some(c) if c.is_ascii_digit() => {
                self.digits();
            }
            _ => return Err("Expecting value".into()),
        }
        let int_end = self.i;
        let mut is_float = false;
        if self.peek() == Some('.') && self.s.get(self.i + 1).is_some_and(char::is_ascii_digit) {
            self.i += 1;
            self.digits();
            is_float = true;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                self.i = save;
            } else {
                is_float = true;
            }
        }
        let text: String = self.s[start..self.i].iter().collect();
        if is_float {
            return Ok(Py::Float(text.parse::<f64>().unwrap_or(f64::NAN)));
        }
        let int_text: String = self.s[start..int_end].iter().collect();
        Ok(match int_text.parse::<i64>() {
            Ok(n) => Py::Int(n),
            Err(_) => Py::BigInt(int_text),
        })
    }

    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.s.len() {
            return Err("Invalid \\uXXXX escape".into());
        }
        let h: String = self.s[self.i..self.i + 4].iter().collect();
        let v = u32::from_str_radix(&h, 16).map_err(|_| "Invalid \\uXXXX escape".to_string())?;
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, String> {
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err("Unterminated string starting at".into());
            };
            self.i += 1;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let Some(e) = self.peek() else {
                        return Err("Unterminated string starting at".into());
                    };
                    self.i += 1;
                    match e {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u)
                                && self.peek() == Some('\\')
                                && self.s.get(self.i + 1) == Some(&'u')
                            {
                                let save = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xdc00..0xe000).contains(&lo) {
                                    let cp = 0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00);
                                    out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                                    continue;
                                }
                                self.i = save;
                            }
                            // A lone surrogate cannot live in a Rust string.
                            out.push(char::from_u32(u).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err("Invalid \\escape".into()),
                    }
                }
                c if (c as u32) < 0x20 => return Err("Invalid control character at".into()),
                c => out.push(c),
            }
        }
    }

    fn object(&mut self) -> Result<Py, String> {
        let mut pairs: Vec<(String, Py)> = Vec::new();
        self.ws();
        if self.peek() == Some('}') {
            self.i += 1;
            return Ok(Py::Dict(pairs));
        }
        loop {
            if self.peek() != Some('"') {
                return Err("Expecting property name enclosed in double quotes".into());
            }
            self.i += 1;
            let k = self.string()?;
            self.ws();
            if self.peek() != Some(':') {
                return Err("Expecting ':' delimiter".into());
            }
            self.i += 1;
            self.ws();
            let v = self.value()?;
            // dict(pairs): a repeated key keeps its first position.
            match pairs.iter_mut().find(|(x, _)| *x == k) {
                Some(slot) => slot.1 = v,
                None => pairs.push((k, v)),
            }
            self.ws();
            match self.peek() {
                Some(',') => {
                    self.i += 1;
                    self.ws();
                }
                Some('}') => {
                    self.i += 1;
                    return Ok(Py::Dict(pairs));
                }
                _ => return Err("Expecting ',' delimiter".into()),
            }
        }
    }

    fn array(&mut self) -> Result<Py, String> {
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(']') {
            self.i += 1;
            return Ok(Py::List(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.peek() {
                Some(',') => {
                    self.i += 1;
                    self.ws();
                }
                Some(']') => {
                    self.i += 1;
                    return Ok(Py::List(items));
                }
                _ => return Err("Expecting ',' delimiter".into()),
            }
        }
    }
}

/// `str.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `len(s)` in code points.
pub fn py_len(s: &str) -> usize {
    s.chars().count()
}

/// `s[..n]` in code points.
pub fn py_head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `s[-n:]` in code points (`n == 0` is the whole string, as in Python).
pub fn py_tail(s: &str, n: usize) -> String {
    let len = py_len(s);
    if n == 0 {
        return s.to_string();
    }
    s.chars().skip(len.saturating_sub(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_like_python() {
        let v = loads(r#"{"a": [1, 2.5, "é"], "b": {}, "a": null}"#).unwrap();
        assert_eq!(v.dumps(), r#"{"a": null, "b": {}}"#);
        assert_eq!(
            loads("[1, {\"x\": []}]").unwrap().dumps_indent2(),
            "[\n  1,\n  {\n    \"x\": []\n  }\n]"
        );
        assert_eq!(loads("-0").unwrap(), Py::Int(0));
        assert!(loads("{\"a\": 1,}").is_err());
        assert!(loads("\"a\nb\"").is_err());
        assert_eq!(
            loads("123456789012345678901234567890").unwrap().dumps(),
            "123456789012345678901234567890"
        );
    }

    #[test]
    fn str_and_repr_like_python() {
        let v = loads(r#"{"a": [1, true, null, 2.0, "it's"]}"#).unwrap();
        assert_eq!(v.py_str(), r#"{'a': [1, True, None, 2.0, "it's"]}"#);
        assert_eq!(Py::Float(1999.0).py_str(), "1999.0");
    }
}
