//! Filename / directory-name templates (port of the Python app's
//! `filename_template.py`).
//!
//! A template is a `str.format`-ish string with `{name}` / `{name:spec}`
//! placeholders. Unknown names render empty, `tracknumber` and `disc` are
//! coerced to integers when they can be, and a spec the value cannot take
//! (Python's `ValueError`) falls back to the bare value. The result always
//! goes through [`sanitize_for_filename`].
//!
//! `spec` follows Python's format-spec mini-language exactly as CPython 3.13
//! parses it, because the Python app hands user-written specs straight to
//! `format()`; the golden table in `testdata/golden/fsops/pure.json` pins it.

use std::collections::HashMap;
use std::sync::LazyLock;

use musaic_core::pycompat::{decimal_digit_value, is_py_space, strip};
use regex::Regex;

/// One template variable as the Python app passes it (tag strings, the album row's
/// integers, or nothing).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}

impl From<Option<i64>> for Value {
    fn from(v: Option<i64>) -> Self {
        v.map_or(Value::None, Value::Int)
    }
}

pub type Values<'a> = HashMap<&'a str, Value>;

/// The cases where the Python app's `render` raises instead of falling back
/// (exceptions other than ValueError/TypeError escape `_format_value`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// `format(n, "c")` with n outside 0..=0x10FFFF (or a surrogate, which a
    /// Rust string cannot hold).
    #[error("%c arg not in range(0x110000)")]
    CharOutOfRange,
    /// int too large to convert to float, for a float presentation type.
    #[error("int too large to convert to float")]
    FloatOverflow,
}

static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{(?P<name>[a-zA-Z_][a-zA-Z0-9_]*)(?::(?P<spec>[^}]*))?\}").unwrap()
});

const INT_FIELDS: [&str; 2] = ["tracknumber", "disc"];

/// `render(template, values)`: substitute, then sanitize. No extension.
pub fn render(template: &str, values: &Values<'_>) -> Result<String, RenderError> {
    if template.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::with_capacity(template.len());
    let mut last = 0;
    for caps in PLACEHOLDER.captures_iter(template) {
        let m = caps.get(0).unwrap();
        out.push_str(&template[last..m.start()]);
        let name = caps.name("name").unwrap().as_str();
        let spec = caps.name("spec").map_or("", |s| s.as_str());
        let raw = values.get(name).unwrap_or(&Value::None);
        out.push_str(&format_value(name, raw, spec)?);
        last = m.end();
    }
    out.push_str(&template[last..]);
    Ok(sanitize_for_filename(&out))
}

/// The leading number part of a filename template: the run at its start that
/// holds only `{tracknumber[:spec]}`, `{disc[:spec]}` and literal separators,
/// up to (not including) the separator before the next placeholder or the end.
/// What a renumber re-applies to a file's numeric prefix, leaving the rest of
/// the name alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberTemplate {
    tokens: Vec<NumberToken>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NumberToken {
    Literal(String),
    /// A `{tracknumber}` or `{disc}` placeholder, in full.
    Field {
        name: String,
        text: String,
    },
}

impl NumberTemplate {
    /// `None` when the template does not start with its track number (it
    /// opens with other text or another placeholder, or has no
    /// `{tracknumber}` in that leading run).
    pub fn parse(template: &str) -> Option<NumberTemplate> {
        let mut tokens: Vec<NumberToken> = Vec::new();
        let mut last = 0;
        for caps in PLACEHOLDER.captures_iter(template) {
            let m = caps.get(0).unwrap();
            let name = caps.name("name").unwrap().as_str();
            if !INT_FIELDS.contains(&name) {
                break;
            }
            if m.start() > last {
                tokens.push(NumberToken::Literal(template[last..m.start()].to_string()));
            }
            tokens.push(NumberToken::Field {
                name: name.to_string(),
                text: m.as_str().to_string(),
            });
            last = m.end();
        }
        // The separator before the next placeholder (or the end) belongs to
        // the rest of the name.
        while matches!(tokens.last(), Some(NumberToken::Literal(_))) {
            tokens.pop();
        }
        // The number has to come first: no text before the first field.
        if !matches!(tokens.first(), Some(NumberToken::Field { .. })) {
            return None;
        }
        tokens
            .iter()
            .any(|t| matches!(t, NumberToken::Field { name, .. } if name == "tracknumber"))
            .then_some(NumberTemplate { tokens })
    }

    /// Whether the template has a `{disc}` part.
    pub fn has_disc(&self) -> bool {
        self.tokens
            .iter()
            .any(|t| matches!(t, NumberToken::Field { name, .. } if name == "disc"))
    }

    /// The number part for a track and, when the set has more than one disc,
    /// the file's disc. Without a disc value `{disc}` is dropped together with
    /// the literal text up to the next field, so `{disc}.{tracknumber:02}`
    /// never leaves `.05` behind.
    pub fn render(&self, track: i64, disc: Option<i64>) -> Option<String> {
        let mut keep = vec![true; self.tokens.len()];
        if disc.is_none() {
            for (i, t) in self.tokens.iter().enumerate() {
                if !matches!(t, NumberToken::Field { name, .. } if name == "disc") {
                    continue;
                }
                keep[i] = false;
                let lit = |j: usize| matches!(self.tokens.get(j), Some(NumberToken::Literal(_)));
                if lit(i + 1) && i + 2 < self.tokens.len() {
                    keep[i + 1] = false;
                } else if i > 0 && lit(i - 1) {
                    keep[i - 1] = false;
                }
            }
        }
        let text: String = self
            .tokens
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(t, _)| match t {
                NumberToken::Literal(l) => l.as_str(),
                NumberToken::Field { text, .. } => text.as_str(),
            })
            .collect();
        let mut values = Values::new();
        values.insert("tracknumber", Value::Int(track));
        values.insert("disc", disc.into());
        render(&text, &values).ok()
    }
}

fn format_value(name: &str, raw: &Value, spec: &str) -> Result<String, RenderError> {
    if *raw == Value::None {
        return Ok(String::new());
    }
    if INT_FIELDS.contains(&name) {
        if let Some(n) = coerce_int(raw) {
            if spec.is_empty() {
                return Ok(n.to_string());
            }
            return match format_int(&n, spec) {
                Ok(s) => Ok(s),
                Err(FormatError::Value) => Ok(n.to_string()),
                Err(FormatError::Raise(e)) => Err(e),
            };
        }
        return Ok(strip(&py_str(raw)).to_string());
    }
    let s = strip(&py_str(raw)).to_string();
    if spec.is_empty() {
        return Ok(s);
    }
    Ok(format_str(&s, spec).unwrap_or(s))
}

/// `str(raw)`.
fn py_str(v: &Value) -> String {
    match v {
        Value::None => "None".into(),
        Value::Str(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Float(f) => py_float_repr(*f),
    }
}

fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.into();
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        // Rust "1e16" -> Python "1e+16"
        let s = format!("{f:e}");
        let (m, e) = s.split_once('e').unwrap();
        let e: i32 = e.parse().unwrap();
        return format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    format!("{f:?}")
}

// ---------- arbitrary-size integers (Python int) ----------

/// A Python int: sign + decimal digits (ASCII, no leading zeros; "0" for 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PyInt {
    neg: bool,
    digits: String,
}

impl PyInt {
    fn from_i64(n: i64) -> Self {
        PyInt {
            neg: n < 0,
            digits: n.unsigned_abs().to_string(),
        }
    }

    fn is_zero(&self) -> bool {
        self.digits == "0"
    }

    /// The magnitude in another base (2, 8 or 16), lower-case digits.
    fn magnitude_in_base(&self, base: u32) -> String {
        let mut dec: Vec<u32> = self.digits.bytes().map(|b| (b - b'0') as u32).collect();
        if dec == [0] {
            return "0".into();
        }
        let mut out = Vec::new();
        while !(dec.len() == 1 && dec[0] == 0) {
            let mut rem = 0u32;
            let mut next = Vec::with_capacity(dec.len());
            for d in &dec {
                let cur = rem * 10 + d;
                let q = cur / base;
                rem = cur % base;
                if !(next.is_empty() && q == 0) {
                    next.push(q);
                }
            }
            if next.is_empty() {
                next.push(0);
            }
            out.push(std::char::from_digit(rem, base).unwrap());
            dec = next;
        }
        out.iter().rev().collect()
    }

    fn to_f64(&self) -> Result<f64, RenderError> {
        let v: f64 = self.digits.parse().unwrap();
        if v.is_infinite() {
            return Err(RenderError::FloatOverflow);
        }
        Ok(if self.neg { -v } else { v })
    }

    fn to_u32(&self) -> Option<u32> {
        if self.neg {
            return None;
        }
        self.digits.parse().ok()
    }
}

impl std::fmt::Display for PyInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.neg {
            f.write_str("-")?;
        }
        f.write_str(&self.digits)
    }
}

/// Python `int(s)` for a string: whitespace, sign, Unicode decimal digits with
/// single underscores between them. `None` where Python raises ValueError.
pub fn py_int(s: &str) -> Option<PyInt> {
    let t = strip(s);
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut digits = String::new();
    let mut seen = false;
    let mut last_us = false;
    for c in body.chars() {
        if c == '_' {
            if !seen || last_us {
                return None;
            }
            last_us = true;
            continue;
        }
        let d = decimal_digit_value(c)?;
        digits.push(char::from_digit(d, 10).unwrap());
        seen = true;
        last_us = false;
    }
    if !seen || last_us {
        return None;
    }
    let trimmed = digits.trim_start_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed }.to_string();
    let neg = neg && digits != "0";
    Some(PyInt { neg, digits })
}

/// `_coerce_int`: ints as is (bool is an int), floats truncated, strings
/// stripped, "5/12" -> 5, then `int()`.
fn coerce_int(raw: &Value) -> Option<PyInt> {
    match raw {
        Value::Int(n) => Some(PyInt::from_i64(*n)),
        Value::Bool(b) => Some(PyInt::from_i64(*b as i64)),
        Value::Float(f) if f.is_finite() => Some(PyInt::from_i64(f.trunc() as i64)),
        Value::Str(s) => {
            let mut s = strip(s);
            if s.is_empty() {
                return None;
            }
            if let Some((head, _)) = s.split_once('/') {
                s = strip(head);
            }
            py_int(s)
        }
        _ => None,
    }
}

// ---------- format-spec mini-language ----------

enum FormatError {
    /// Python raises ValueError: the Python app falls back to the bare value.
    Value,
    /// Python raises something else: it escapes `render`.
    Raise(RenderError),
}

impl From<RenderError> for FormatError {
    fn from(e: RenderError) -> Self {
        FormatError::Raise(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grouping {
    None,
    Comma,
    Underscore,
}

#[derive(Debug, Clone)]
struct Spec {
    fill: char,
    fill_specified: bool,
    align: Option<char>,
    sign: Option<char>,
    no_neg_0: bool,
    alternate: bool,
    width: usize,
    grouping: Grouping,
    precision: Option<usize>,
    ty: Option<char>,
}

fn is_align(c: char) -> bool {
    matches!(c, '<' | '>' | '=' | '^')
}

/// CPython's `parse_internal_render_format_spec`.
fn parse_spec(spec: &str, default_align: char) -> Result<Spec, FormatError> {
    let c: Vec<char> = spec.chars().collect();
    let mut s = Spec {
        fill: ' ',
        fill_specified: false,
        align: None,
        sign: None,
        no_neg_0: false,
        alternate: false,
        width: 0,
        grouping: Grouping::None,
        precision: None,
        ty: None,
    };
    let mut pos = 0;
    let end = c.len();
    if end >= 2 && is_align(c[1]) {
        s.fill = c[0];
        s.fill_specified = true;
        s.align = Some(c[1]);
        pos = 2;
    } else if end >= 1 && is_align(c[0]) {
        s.align = Some(c[0]);
        pos = 1;
    }
    let align_specified = s.align.is_some();
    if pos < end && matches!(c[pos], '+' | '-' | ' ') {
        s.sign = Some(c[pos]);
        pos += 1;
    }
    if pos < end && c[pos] == 'z' {
        s.no_neg_0 = true;
        pos += 1;
    }
    if pos < end && c[pos] == '#' {
        s.alternate = true;
        pos += 1;
    }
    if !s.fill_specified && pos < end && c[pos] == '0' {
        s.fill = '0';
        if !align_specified && default_align == '>' {
            s.align = Some('=');
        }
        pos += 1;
    }
    let (w, n) = read_number(&c[pos..])?;
    if n > 0 {
        s.width = w;
        pos += n;
    }
    if pos < end && c[pos] == ',' {
        s.grouping = Grouping::Comma;
        pos += 1;
    }
    if pos < end && c[pos] == '_' {
        if s.grouping != Grouping::None {
            return Err(FormatError::Value);
        }
        s.grouping = Grouping::Underscore;
        pos += 1;
    }
    if pos < end && c[pos] == ',' && s.grouping == Grouping::Underscore {
        return Err(FormatError::Value);
    }
    if pos < end && c[pos] == '.' {
        pos += 1;
        let (p, n) = read_number(&c[pos..])?;
        if n == 0 {
            return Err(FormatError::Value); // "Format specifier missing precision"
        }
        s.precision = Some(p);
        pos += n;
    }
    if end - pos > 1 {
        return Err(FormatError::Value); // "Invalid format specifier"
    }
    if end - pos == 1 {
        s.ty = Some(c[pos]);
    }
    if s.grouping != Grouping::None {
        match s.ty {
            None | Some('d' | 'e' | 'f' | 'g' | 'E' | 'G' | '%' | 'F') => {}
            Some('b' | 'o' | 'x' | 'X') if s.grouping == Grouping::Underscore => {}
            _ => return Err(FormatError::Value),
        }
    }
    Ok(s)
}

/// ASCII digits only, as CPython's `get_integer` reads them.
fn read_number(c: &[char]) -> Result<(usize, usize), FormatError> {
    let mut v: usize = 0;
    let mut n = 0;
    while n < c.len() && c[n].is_ascii_digit() {
        v = v
            .checked_mul(10)
            .and_then(|v| v.checked_add(c[n] as usize - '0' as usize))
            .ok_or(FormatError::Value)?;
        n += 1;
    }
    Ok((v, n))
}

fn pad(s: Spec, body_len: usize, default_align: char) -> (usize, usize) {
    let total = s.width.saturating_sub(body_len);
    match s.align.unwrap_or(default_align) {
        '<' => (0, total),
        '^' => (total / 2, total - total / 2),
        _ => (total, 0), // '>' (and '=' handled by the caller)
    }
}

fn repeat(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

/// `format(s, spec)` for a str.
fn format_str(value: &str, spec: &str) -> Result<String, FormatError> {
    let s = parse_spec(spec, '<')?;
    if s.sign.is_some()
        || s.alternate
        || s.no_neg_0
        || s.align == Some('=')
        || s.grouping != Grouping::None
    {
        return Err(FormatError::Value);
    }
    if !matches!(s.ty, None | Some('s')) {
        return Err(FormatError::Value);
    }
    let body: String = match s.precision {
        Some(p) => value.chars().take(p).collect(),
        None => value.to_string(),
    };
    let (l, r) = pad(s.clone(), body.chars().count(), '<');
    Ok(format!("{}{body}{}", repeat(s.fill, l), repeat(s.fill, r)))
}

/// Insert `sep` every `every` digits from the right; when `min_width` is set
/// (zero padding with '=' alignment), left-pad with zeros until the grouped
/// string is at least that long, as CPython's grouping with min_width does.
fn group(digits: &str, sep: char, every: usize, min_width: Option<usize>) -> String {
    let grouped = |d: &str| {
        let chars: Vec<char> = d.chars().collect();
        let mut out = String::new();
        for (i, ch) in chars.iter().enumerate() {
            if i > 0 && (chars.len() - i).is_multiple_of(every) {
                out.push(sep);
            }
            out.push(*ch);
        }
        out
    };
    let mut d = digits.to_string();
    let mut g = grouped(&d);
    if let Some(min) = min_width {
        while g.chars().count() < min {
            d.insert(0, '0');
            g = grouped(&d);
        }
    }
    g
}

/// Lay out sign + prefix + digits(+ remainder) with fill/align/zero padding.
fn assemble(
    s: &Spec,
    negative: bool,
    prefix: &str,
    digits: &str,
    remainder: &str,
    group_every: usize,
) -> String {
    let sign = match (negative, s.sign) {
        (true, _) => "-",
        (false, Some('+')) => "+",
        (false, Some(' ')) => " ",
        _ => "",
    };
    let zero_eq = s.fill == '0' && s.align == Some('=');
    let fixed = sign.chars().count() + prefix.chars().count() + remainder.chars().count();
    let digits = match s.grouping {
        Grouping::None => {
            if zero_eq && digits.chars().count() + fixed < s.width {
                format!(
                    "{}{digits}",
                    repeat('0', s.width - fixed - digits.chars().count())
                )
            } else {
                digits.to_string()
            }
        }
        g => {
            let sep = if g == Grouping::Comma { ',' } else { '_' };
            let min = zero_eq.then(|| s.width.saturating_sub(fixed));
            group(digits, sep, group_every, min)
        }
    };
    let body_len = fixed + digits.chars().count();
    let total = s.width.saturating_sub(body_len);
    if s.align == Some('=') {
        return format!("{sign}{prefix}{}{digits}{remainder}", repeat(s.fill, total));
    }
    let (l, r) = pad(s.clone(), body_len, '>');
    format!(
        "{}{sign}{prefix}{digits}{remainder}{}",
        repeat(s.fill, l),
        repeat(s.fill, r)
    )
}

/// `format(n, spec)` for an int.
fn format_int(n: &PyInt, spec: &str) -> Result<String, FormatError> {
    let s = parse_spec(spec, '>')?;
    let ty = s.ty.unwrap_or('d');
    match ty {
        'b' | 'c' | 'd' | 'o' | 'x' | 'X' | 'n' => {}
        'e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%' => return format_float(n.to_f64()?, &s),
        _ => return Err(FormatError::Value),
    }
    if s.precision.is_some() || s.no_neg_0 {
        return Err(FormatError::Value);
    }
    if ty == 'c' {
        if s.sign.is_some() || s.alternate {
            return Err(FormatError::Value);
        }
        let ch = n
            .to_u32()
            .and_then(char::from_u32)
            .ok_or(RenderError::CharOutOfRange)?;
        let body = ch.to_string();
        let (l, r) = pad(s.clone(), 1, '>');
        if s.align == Some('=') {
            return Ok(format!("{}{body}", repeat(s.fill, l + r)));
        }
        return Ok(format!("{}{body}{}", repeat(s.fill, l), repeat(s.fill, r)));
    }
    let (digits, prefix, every) = match ty {
        'b' => (n.magnitude_in_base(2), "0b", 4),
        'o' => (n.magnitude_in_base(8), "0o", 4),
        'x' => (n.magnitude_in_base(16), "0x", 4),
        'X' => (n.magnitude_in_base(16).to_uppercase(), "0X", 4),
        _ => (n.digits.clone(), "", 3),
    };
    let prefix = if s.alternate { prefix } else { "" };
    Ok(assemble(
        &s,
        n.neg && !n.is_zero(),
        prefix,
        &digits,
        "",
        every,
    ))
}

/// `format(float(n), spec)` for the float presentation types.
fn format_float(v: f64, s: &Spec) -> Result<String, FormatError> {
    let ty = s.ty.unwrap();
    let prec = s.precision.unwrap_or(6);
    let neg = v.is_sign_negative() && v != 0.0;
    let a = v.abs();
    let (body, pct) = match ty {
        'f' | 'F' => (fixed(a, prec, s.alternate), ""),
        '%' => (fixed(a * 100.0, prec, s.alternate), "%"),
        'e' | 'E' => {
            let b = sci(a, prec, s.alternate);
            (if ty == 'E' { b.to_uppercase() } else { b }, "")
        }
        _ => {
            let b = general(a, if prec == 0 { 1 } else { prec }, s.alternate);
            (if ty == 'G' { b.to_uppercase() } else { b }, "")
        }
    };
    // Split integer digits (grouped) from the remainder ('.', fraction, exponent).
    let split = body
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(body.len());
    let (int_part, rest) = body.split_at(split);
    let remainder = format!("{rest}{pct}");
    Ok(assemble(s, neg, "", int_part, &remainder, 3))
}

fn fixed(a: f64, prec: usize, alt: bool) -> String {
    let mut out = format!("{a:.prec$}");
    if alt && prec == 0 {
        out.push('.');
    }
    out
}

fn sci(a: f64, prec: usize, alt: bool) -> String {
    let r = format!("{a:.prec$e}");
    let (m, e) = r.split_once('e').unwrap();
    let e: i32 = e.parse().unwrap();
    let mut m = m.to_string();
    if alt && prec == 0 {
        m.push('.');
    }
    format!("{m}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs())
}

/// Python's 'g': P significant digits; fixed when -4 <= exp < P, else
/// scientific; trailing zeros (and a bare '.') removed unless '#'.
fn general(a: f64, p: usize, alt: bool) -> String {
    let r = format!("{a:.prec$e}", prec = p - 1);
    let (_, e) = r.split_once('e').unwrap();
    let exp: i32 = e.parse().unwrap();
    let out = if exp >= -4 && exp < p as i32 {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        let mut f = format!("{a:.decimals$}");
        if alt && decimals == 0 {
            f.push('.');
        }
        f
    } else {
        sci(a, p - 1, alt)
    };
    if alt {
        return out;
    }
    let (mant, exp_part) = match out.find('e') {
        Some(i) => out.split_at(i),
        None => (out.as_str(), ""),
    };
    let mant = if mant.contains('.') {
        mant.trim_end_matches('0').trim_end_matches('.')
    } else {
        mant
    };
    format!("{mant}{exp_part}")
}

// ---------- sanitising ----------

static UNSAFE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[/\\:*?"<>|]"#).unwrap());
static CONTROL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\x00-\x1f\x7f]").unwrap());
static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static SEP_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\s*-\s*){2,}").unwrap());
static SEP_LEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:\s*-\s*)+").unwrap());
static SEP_TRAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:\s*-\s*)+$").unwrap());
static TRAILING_BAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[.\s]+$").unwrap());

/// `sanitize_for_filename`: unsafe characters become " - ", control
/// characters go, whitespace collapses, runs of separators merge, and leading
/// / trailing separators, whitespace and dots are trimmed.
///
/// Python's `\s` (str.isspace) and the regex crate's `\s` (White_Space)
/// differ only on U+001C..U+001F, which the control-character pass has
/// already removed.
pub fn sanitize_for_filename(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let s = UNSAFE.replace_all(name, " - ");
    let s = CONTROL.replace_all(&s, "");
    let s = WS.replace_all(&s, " ");
    let s = s.trim_matches(is_py_space);
    let s = SEP_RUN.replace_all(s, " - ");
    let s = SEP_LEAD.replace_all(&s, "");
    let s = SEP_TRAIL.replace_all(&s, "");
    TRAILING_BAD.replace_all(&s, "").into_owned()
}

#[cfg(test)]
mod number_template_tests {
    use super::NumberTemplate;

    fn nt(t: &str) -> Option<NumberTemplate> {
        NumberTemplate::parse(t)
    }

    #[test]
    fn the_leading_number_run_stops_before_the_next_placeholder() {
        assert_eq!(
            nt("{tracknumber:02} - {title}")
                .unwrap()
                .render(5, None)
                .as_deref(),
            Some("05")
        );
        let t = nt("{disc}-{tracknumber:02} {title}").unwrap();
        assert_eq!(t.render(5, Some(2)).as_deref(), Some("2-05"));
        assert_eq!(t.render(5, None).as_deref(), Some("05"));
        assert!(t.has_disc());
    }

    #[test]
    fn a_missing_disc_never_leaves_a_stray_separator() {
        let t = nt("{disc}.{tracknumber:02} - {title}").unwrap();
        assert_eq!(t.render(5, None).as_deref(), Some("05"));
        assert_eq!(t.render(5, Some(1)).as_deref(), Some("1.05"));
        let t = nt("{tracknumber:02}-{disc} {title}").unwrap();
        assert_eq!(t.render(5, None).as_deref(), Some("05"));
        assert_eq!(t.render(5, Some(2)).as_deref(), Some("05-2"));
    }

    #[test]
    fn a_template_whose_number_does_not_come_first_has_no_number_part() {
        assert_eq!(nt("{artist} - {tracknumber:02}"), None);
        assert_eq!(nt("Track {tracknumber:02}"), None);
        assert_eq!(nt("{disc} - {title}"), None);
        assert_eq!(nt("{title}"), None);
        assert_eq!(nt(""), None);
    }

    #[test]
    fn a_template_of_only_the_number_is_all_number() {
        assert_eq!(
            nt("{tracknumber}").unwrap().render(7, None).as_deref(),
            Some("7")
        );
    }
}
