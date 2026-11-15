//! FastAPI / pydantic v2 request validation, reduced to the field types the
//! Python app's payload models use, in pydantic's lax ("python") mode, and the
//! 422 body FastAPI builds from it:
//! `{"detail": [{"type", "loc", "msg", "input"[, "ctx"]}, ...]}`.
//!
//! Reproduced: `missing`, `string_type`, `int_type`, `int_parsing`,
//! `int_from_float`, `bool_type`, `bool_parsing`, `float_type`,
//! `float_parsing`, `dict_type`, `list_type`, `model_attributes_type`,
//! `literal_error` (string literals), `json_invalid` (message for the two commonest decode errors only, see
//! [`parse_json_body`]). Lax coercions reproduced: numeric strings to int /
//! float, integral floats to int, the pydantic boolean strings
//! (`0 off f false n no 1 on t true y yes`, any case) and 0/1 to bool.
//! Not reproduced: `url` keys (FastAPI drops them anyway), integers beyond
//! i64, `ctx` for anything but `json_invalid` and `literal_error`, unicode /
//! whitespace edge cases of pydantic's number parsing, and the body's key
//! order inside an echoed `input` object (it is rendered sorted; no 422
//! recorded from the Python app echoes an object with more than one key).
//!
//! The 422 bytes: [`validation_detail`] renders every error item in
//! pydantic's `ErrorDetails` key order (`type, loc, msg, input, ctx`), as
//! FastAPI's `JSONResponse` writes them; `serde_json`'s `Map` would sort
//! them (`ctx, input, loc, msg, type`).

use musaic_services::Json;
use serde_json::{Map, Value, json};

use crate::http::ApiError;

/// The key order of one pydantic `ErrorDetails` dict as FastAPI sends it
/// (`include_url=False`, so no `url`).
const ERROR_KEYS: [&str; 5] = ["type", "loc", "msg", "input", "ctx"];

/// `detail` of a 422 as FastAPI renders it: a list of validation error
/// items, each with its keys in [`ERROR_KEYS`] order (any other key after
/// them, sorted; nested objects sorted). `None` when `detail` is not such a
/// list, so other 422s keep the plain rendering.
pub fn validation_detail(detail: &Value) -> Option<Json> {
    let items = detail.as_array()?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let m = item.as_object()?;
        if !["type", "loc", "msg"].iter().all(|k| m.contains_key(*k)) {
            return None;
        }
        let mut pairs: Vec<(String, Json)> = ERROR_KEYS
            .iter()
            .filter_map(|k| m.get(*k).map(|v| ((*k).to_string(), to_json(v))))
            .collect();
        pairs.extend(
            m.iter()
                .filter(|(k, _)| !ERROR_KEYS.contains(&k.as_str()))
                .map(|(k, v)| (k.clone(), to_json(v))),
        );
        out.push(Json::Obj(pairs));
    }
    Some(Json::Arr(out))
}

fn to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Json::Int(i),
            None => Json::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => Json::Str(s.clone()),
        Value::Array(xs) => Json::Arr(xs.iter().map(to_json).collect()),
        Value::Object(m) => Json::Obj(m.iter().map(|(k, x)| (k.clone(), to_json(x))).collect()),
    }
}

pub fn err(ty: &str, loc: Vec<Value>, msg: &str, input: Value) -> Value {
    json!({"type": ty, "loc": loc, "msg": msg, "input": input})
}

pub fn unprocessable(errors: Vec<Value>) -> ApiError {
    ApiError {
        status: 422,
        detail: Value::Array(errors),
    }
}

/// The request body as FastAPI reads it for a JSON model parameter.
/// `Ok(None)` for an empty body.
pub fn parse_json_body(bytes: &[u8]) -> Result<Option<Value>, ApiError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            let text = String::from_utf8_lossy(bytes);
            let pos = char_offset(&text, e.line(), e.column());
            let s = e.to_string();
            // Python's json module wording for the cases serde can name.
            let msg = if s.starts_with("key must be a string") {
                "Expecting property name enclosed in double quotes"
            } else if s.starts_with("trailing comma") {
                "Illegal trailing comma before end of object"
            } else if s.starts_with("trailing characters") {
                "Extra data"
            } else {
                "Expecting value"
            };
            Err(unprocessable(vec![json!({
                "type": "json_invalid",
                "loc": ["body", pos],
                "msg": "JSON decode error",
                "input": {},
                "ctx": {"error": msg},
            })]))
        }
    }
}

fn char_offset(text: &str, line: usize, column: usize) -> usize {
    let mut off = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i + 1 == line {
            return off + column.saturating_sub(1);
        }
        off += l.chars().count() + 1;
    }
    off
}

/// The key order of every object in a JSON body, as Python's `json.loads`
/// builds dicts: keys in first-seen order, a repeated key keeping its
/// first position and taking the last value (serde_json's `Map` is sorted,
/// so the body's order is read separately). Only the order
/// is kept; values come from the parsed [`Value`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeyOrder(Vec<(String, KeyOrder)>);

static NO_ORDER: KeyOrder = KeyOrder(Vec::new());

impl KeyOrder {
    /// The order of `bytes` (empty when it is not JSON; callers parse the
    /// body first and refuse it then).
    pub fn of_body(bytes: &[u8]) -> KeyOrder {
        serde_json::from_slice(bytes).unwrap_or_default()
    }

    /// The order inside the object at `key` (empty when absent).
    pub fn child(&self, key: &str) -> &KeyOrder {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map_or(&NO_ORDER, |(_, o)| o)
    }

    /// The keys, in body order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    /// `m`'s entries in body order (any key the order does not know, last,
    /// sorted).
    pub fn entries(&self, m: Map<String, Value>) -> Vec<(String, Value)> {
        let mut v: Vec<(String, Value)> = m.into_iter().collect();
        let pos = |k: &str| {
            self.0
                .iter()
                .position(|(o, _)| o == k)
                .unwrap_or(usize::MAX)
        };
        v.sort_by_key(|(k, _)| pos(k));
        v
    }
}

impl<'de> serde::Deserialize<'de> for KeyOrder {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = KeyOrder;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<KeyOrder, A::Error> {
                let mut out: Vec<(String, KeyOrder)> = Vec::new();
                while let Some((k, v)) = a.next_entry::<String, KeyOrder>()? {
                    match out.iter_mut().find(|(o, _)| *o == k) {
                        Some(slot) => slot.1 = v,
                        None => out.push((k, v)),
                    }
                }
                Ok(KeyOrder(out))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<KeyOrder, A::Error> {
                while a.next_element::<KeyOrder>()?.is_some() {}
                Ok(KeyOrder::default())
            }
            fn visit_bool<E>(self, _: bool) -> Result<KeyOrder, E> {
                Ok(KeyOrder::default())
            }
            fn visit_i64<E>(self, _: i64) -> Result<KeyOrder, E> {
                Ok(KeyOrder::default())
            }
            fn visit_u64<E>(self, _: u64) -> Result<KeyOrder, E> {
                Ok(KeyOrder::default())
            }
            fn visit_f64<E>(self, _: f64) -> Result<KeyOrder, E> {
                Ok(KeyOrder::default())
            }
            fn visit_str<E>(self, _: &str) -> Result<KeyOrder, E> {
                Ok(KeyOrder::default())
            }
            fn visit_unit<E>(self) -> Result<KeyOrder, E> {
                Ok(KeyOrder::default())
            }
        }
        d.deserialize_any(V)
    }
}

/// The fields of one body model.
pub struct Fields {
    map: Map<String, Value>,
    whole: Value,
    errors: Vec<Value>,
    order: KeyOrder,
}

impl Fields {
    /// A required model body: missing -> `missing` at `["body"]`; not an
    /// object -> `model_attributes_type`.
    pub fn required(body: Option<Value>) -> Result<Fields, ApiError> {
        match body {
            None => Err(unprocessable(vec![err(
                "missing",
                vec![json!("body")],
                "Field required",
                Value::Null,
            )])),
            Some(v) => Fields::from_value(v),
        }
    }

    /// An optional model body (`payload: Model | None = None`).
    pub fn optional(body: Option<Value>) -> Result<Option<Fields>, ApiError> {
        match body {
            None | Some(Value::Null) => Ok(None),
            Some(v) => Fields::from_value(v).map(Some),
        }
    }

    fn from_value(v: Value) -> Result<Fields, ApiError> {
        match v {
            Value::Object(map) => Ok(Fields {
                whole: Value::Object(map.clone()),
                map,
                errors: Vec::new(),
                order: KeyOrder::default(),
            }),
            other => Err(unprocessable(vec![err(
                "model_attributes_type",
                vec![json!("body")],
                "Input should be a valid dictionary or object to extract fields from",
                other,
            )])),
        }
    }

    /// Read dict fields ([`Fields::str_map`], [`Fields::str_map_map`]) in
    /// the body's key order (`KeyOrder::of_body` of the same bytes).
    pub fn with_order(mut self, order: KeyOrder) -> Fields {
        self.order = order;
        self
    }

    fn loc(name: &str) -> Vec<Value> {
        vec![json!("body"), json!(name)]
    }

    fn present(&self, name: &str) -> Option<&Value> {
        self.map.get(name)
    }

    pub fn req_str(&mut self, name: &str) -> Option<String> {
        match self.present(name).cloned() {
            None => {
                let whole = self.whole.clone();
                self.errors
                    .push(err("missing", Self::loc(name), "Field required", whole));
                None
            }
            Some(v) => self.as_str(name, v),
        }
    }

    pub fn opt_str(&mut self, name: &str) -> Option<String> {
        match self.present(name).cloned() {
            None | Some(Value::Null) => None,
            Some(v) => self.as_str(name, v),
        }
    }

    fn as_str(&mut self, name: &str, v: Value) -> Option<String> {
        match v {
            Value::String(s) => Some(s),
            other => {
                self.errors.push(err(
                    "string_type",
                    Self::loc(name),
                    "Input should be a valid string",
                    other,
                ));
                None
            }
        }
    }

    pub fn opt_int(&mut self, name: &str) -> Option<i64> {
        match self.present(name).cloned() {
            None | Some(Value::Null) => None,
            Some(v) => match lax_int(&v) {
                Ok(n) => Some(n),
                Err((ty, msg)) => {
                    self.errors.push(err(ty, Self::loc(name), msg, v));
                    None
                }
            },
        }
    }

    pub fn opt_float(&mut self, name: &str) -> Option<f64> {
        match self.present(name).cloned() {
            None | Some(Value::Null) => None,
            Some(v) => match lax_float(&v) {
                Ok(n) => Some(n),
                Err((ty, msg)) => {
                    self.errors.push(err(ty, Self::loc(name), msg, v));
                    None
                }
            },
        }
    }

    pub fn opt_bool(&mut self, name: &str) -> Option<bool> {
        match self.present(name).cloned() {
            None | Some(Value::Null) => None,
            Some(v) => match lax_bool(&v) {
                Ok(b) => Some(b),
                Err((ty, msg)) => {
                    self.errors.push(err(ty, Self::loc(name), msg, v));
                    None
                }
            },
        }
    }

    /// `bool = default` (null is not allowed for a non-Optional bool).
    pub fn bool_or(&mut self, name: &str, default: bool) -> bool {
        match self.present(name).cloned() {
            None => default,
            Some(v) => match lax_bool(&v) {
                Ok(b) => b,
                Err((ty, msg)) => {
                    self.errors.push(err(ty, Self::loc(name), msg, v));
                    default
                }
            },
        }
    }

    /// A required `Literal["a", "b", ...]` of strings: `missing`, or
    /// `literal_error` with pydantic's `ctx.expected` (`'a' or 'b'`,
    /// `'a', 'b' or 'c'`) for any other value, strings and non-strings alike.
    pub fn req_literal(&mut self, name: &str, allowed: &[&str]) -> Option<String> {
        let v = match self.present(name).cloned() {
            None => {
                let whole = self.whole.clone();
                self.errors
                    .push(err("missing", Self::loc(name), "Field required", whole));
                return None;
            }
            Some(v) => v,
        };
        if let Value::String(s) = &v
            && allowed.contains(&s.as_str())
        {
            return Some(s.clone());
        }
        let quoted: Vec<String> = allowed.iter().map(|a| format!("'{a}'")).collect();
        let expected = match quoted.split_last() {
            None => String::new(),
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        };
        let mut e = err(
            "literal_error",
            Self::loc(name),
            &format!("Input should be {expected}"),
            v,
        );
        e["ctx"] = json!({ "expected": expected });
        self.errors.push(e);
        None
    }

    /// `str = default` (null rejected).
    pub fn str_or(&mut self, name: &str, default: &str) -> String {
        match self.present(name).cloned() {
            None => default.to_string(),
            Some(v) => self.as_str(name, v).unwrap_or_default(),
        }
    }

    /// `list[str] | None`.
    pub fn opt_str_list(&mut self, name: &str) -> Option<Vec<String>> {
        match self.present(name).cloned() {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => {
                let mut out = Vec::new();
                for (i, x) in items.into_iter().enumerate() {
                    match x {
                        Value::String(s) => out.push(s),
                        other => self.errors.push(err(
                            "string_type",
                            vec![json!("body"), json!(name), json!(i)],
                            "Input should be a valid string",
                            other,
                        )),
                    }
                }
                Some(out)
            }
            Some(other) => {
                self.errors.push(err(
                    "list_type",
                    Self::loc(name),
                    "Input should be a valid list",
                    other,
                ));
                None
            }
        }
    }

    /// `dict[str, str] = {}`: entries in body order.
    pub fn str_map(&mut self, name: &str) -> Vec<(String, String)> {
        match self.present(name).cloned() {
            None => Vec::new(),
            Some(Value::Object(m)) => {
                let mut out = Vec::new();
                for (k, v) in self.order.child(name).entries(m) {
                    match v {
                        Value::String(s) => out.push((k, s)),
                        other => self.errors.push(err(
                            "string_type",
                            vec![json!("body"), json!(name), json!(k)],
                            "Input should be a valid string",
                            other,
                        )),
                    }
                }
                out
            }
            Some(other) => {
                self.errors.push(err(
                    "dict_type",
                    Self::loc(name),
                    "Input should be a valid dictionary",
                    other,
                ));
                Vec::new()
            }
        }
    }

    /// `dict[str, dict[str, str]] = {}`.
    pub fn str_map_map(&mut self, name: &str) -> Vec<(String, Vec<(String, String)>)> {
        match self.present(name).cloned() {
            None => Vec::new(),
            Some(Value::Object(m)) => {
                let mut out = Vec::new();
                let order = self.order.child(name).clone();
                for (k, v) in order.entries(m) {
                    match v {
                        Value::Object(inner) => {
                            let mut row = Vec::new();
                            for (ik, iv) in order.child(&k).entries(inner) {
                                match iv {
                                    Value::String(s) => row.push((ik, s)),
                                    other => self.errors.push(err(
                                        "string_type",
                                        vec![json!("body"), json!(name), json!(k), json!(ik)],
                                        "Input should be a valid string",
                                        other,
                                    )),
                                }
                            }
                            out.push((k, row));
                        }
                        other => self.errors.push(err(
                            "dict_type",
                            vec![json!("body"), json!(name), json!(k)],
                            "Input should be a valid dictionary",
                            other,
                        )),
                    }
                }
                out
            }
            Some(other) => {
                self.errors.push(err(
                    "dict_type",
                    Self::loc(name),
                    "Input should be a valid dictionary",
                    other,
                ));
                Vec::new()
            }
        }
    }

    /// 422 with every collected error, in field order.
    pub fn finish(self) -> Result<(), ApiError> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(unprocessable(self.errors))
        }
    }
}

type LaxErr = (&'static str, &'static str);

const INT_TYPE: LaxErr = ("int_type", "Input should be a valid integer");
const INT_PARSING: LaxErr = (
    "int_parsing",
    "Input should be a valid integer, unable to parse string as an integer",
);
const BOOL_TYPE: LaxErr = ("bool_type", "Input should be a valid boolean");
const BOOL_PARSING: LaxErr = (
    "bool_parsing",
    "Input should be a valid boolean, unable to interpret input",
);
const FLOAT_TYPE: LaxErr = ("float_type", "Input should be a valid number");
const FLOAT_PARSING: LaxErr = (
    "float_parsing",
    "Input should be a valid number, unable to parse string as a number",
);

pub fn lax_int(v: &Value) -> Result<i64, LaxErr> {
    match v {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.fract() == 0.0 && f.is_finite() {
                    Ok(f as i64)
                } else {
                    Err((
                        "int_from_float",
                        "Input should be a valid integer, got a number with a fractional part",
                    ))
                }
            }
        }
        Value::String(s) => int_str(s).ok_or(INT_PARSING),
        _ => Err(INT_TYPE),
    }
}

fn int_str(s: &str) -> Option<i64> {
    let t = s.trim();
    let t = t.replace('_', "");
    t.parse::<i64>().ok()
}

pub fn lax_float(v: &Value) -> Result<f64, LaxErr> {
    match v {
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => n.as_f64().ok_or(FLOAT_TYPE),
        Value::String(s) => s.trim().parse::<f64>().map_err(|_| FLOAT_PARSING),
        _ => Err(FLOAT_TYPE),
    }
}

pub fn lax_bool(v: &Value) -> Result<bool, LaxErr> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => {
            let x = n.as_f64();
            if x == Some(0.0) {
                Ok(false)
            } else if x == Some(1.0) {
                Ok(true)
            } else {
                Err(BOOL_PARSING)
            }
        }
        Value::String(s) => bool_str(s).ok_or(BOOL_PARSING),
        _ => Err(BOOL_TYPE),
    }
}

fn bool_str(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
        "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
        _ => None,
    }
}

/// Query parameters in request order (Starlette keeps the last value of a
/// repeated key for a scalar parameter), with the names the matched route
/// declared in the route table (`crate::route_table`).
#[derive(Debug, Default, Clone)]
pub struct QueryParams(
    pub Vec<(String, String)>,
    pub Option<&'static [&'static str]>,
);

impl QueryParams {
    /// The last value of `name`. Debug builds refuse a name the route did
    /// not declare (`route_table::Endpoint::query`), so the route table's
    /// query parameters are exactly what handlers read.
    pub fn get(&self, name: &str) -> Option<&str> {
        debug_assert!(
            self.1.is_some_and(|d| d.contains(&name)),
            "query parameter {name:?} read but not declared for this route \
             (route_table::Endpoint::query; declared: {:?})",
            self.1
        );
        self.0
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Collects query / path errors before the handler runs.
#[derive(Default)]
pub struct Params {
    errors: Vec<Value>,
}

impl Params {
    pub fn query_bool(&mut self, q: &QueryParams, name: &str, default: bool) -> bool {
        match q.get(name) {
            None => default,
            Some(s) => match bool_str(s) {
                Some(b) => b,
                None => {
                    self.errors.push(err(
                        BOOL_PARSING.0,
                        vec![json!("query"), json!(name)],
                        BOOL_PARSING.1,
                        json!(s),
                    ));
                    default
                }
            },
        }
    }

    pub fn path_int(&mut self, raw: &str, name: &str) -> i64 {
        match int_str(raw) {
            Some(n) => n,
            None => {
                self.errors.push(err(
                    INT_PARSING.0,
                    vec![json!("path"), json!(name)],
                    INT_PARSING.1,
                    json!(raw),
                ));
                0
            }
        }
    }

    pub fn finish(self) -> Result<(), ApiError> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(unprocessable(self.errors))
        }
    }
}
