//! Python built-in behaviour the Python app depends on, reproduced exactly where a
//! Rust std function would differ on real-world tag or settings values.

use unicode_general_category::{GeneralCategory, get_general_category};

/// Value of a Unicode decimal digit (general category Nd), like the digits
/// Python's `int()` accepts: ASCII, Arabic-Indic, fullwidth, ...
///
/// Unicode keeps Nd characters in contiguous runs of ten starting at a zero,
/// so the value is the offset from the start of the run, modulo ten.
pub fn decimal_digit_value(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(c as u32 - '0' as u32);
    }
    if get_general_category(c) != GeneralCategory::DecimalNumber {
        return None;
    }
    let mut start = c as u32;
    while let Some(prev) = start.checked_sub(1).and_then(char::from_u32) {
        if get_general_category(prev) != GeneralCategory::DecimalNumber {
            break;
        }
        start -= 1;
    }
    Some((c as u32 - start) % 10)
}

/// `str.isspace()` for one character: Unicode White_Space plus the four
/// ASCII separators U+001C..U+001F that Python also treats as whitespace.
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()` with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// `int(s)` for a string, base 10: surrounding whitespace, an optional sign,
/// then Unicode decimal digits with single underscores allowed between digits.
/// `None` where Python raises `ValueError` (and on overflow of i64, which no
/// real tag or setting reaches).
pub fn int(s: &str) -> Option<i64> {
    let t = strip(s);
    let (negative, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let mut value: i64 = 0;
    let mut seen_digit = false;
    let mut last_was_underscore = false;
    for c in digits.chars() {
        if c == '_' {
            if !seen_digit || last_was_underscore {
                return None;
            }
            last_was_underscore = true;
            continue;
        }
        let d = decimal_digit_value(c)?;
        value = value.checked_mul(10)?.checked_add(d as i64)?;
        seen_digit = true;
        last_was_underscore = false;
    }
    if !seen_digit || last_was_underscore {
        return None;
    }
    Some(if negative { -value } else { value })
}

/// `round(x, 1)`: correctly rounded on the exact binary value, ties to even.
pub fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}

/// A string literal as `json.dumps` writes it with the default
/// `ensure_ascii=True`: everything outside printable ASCII is `\uXXXX`
/// (surrogate pairs above the BMP), with the short escapes for the usual
/// control characters.
pub fn json_string(s: &str) -> String {
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
            ' '..='~' => out.push(c),
            _ => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_follows_python() {
        assert_eq!(int("10"), Some(10));
        assert_eq!(int("  -3\n"), Some(-3));
        assert_eq!(int("+7"), Some(7));
        assert_eq!(int("1_000"), Some(1000));
        assert_eq!(int("١٢"), Some(12));
        assert_eq!(int("５"), Some(5));
        assert_eq!(int("\u{1d7d7}"), Some(9)); // mathematical bold digit nine
        for bad in [
            "", " ", "abc", "1.5", "_1", "1_", "1__0", "0x10", "- 3", "²", "1e2", "+", "-",
        ] {
            assert_eq!(int(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn strip_follows_python() {
        assert_eq!(strip(" \t a b \u{1c}\u{a0}\n"), "a b");
        assert_eq!(strip(""), "");
    }

    #[test]
    fn round1_ties_follow_the_binary_value() {
        assert_eq!(round1(0.25), 0.2); // exact tie -> even
        assert_eq!(round1(0.35), 0.3); // 0.35 is slightly below the tie in binary
        assert_eq!(round1(30.04999), 30.0);
        assert_eq!(round1(50.0), 50.0);
    }

    #[test]
    fn json_strings_follow_python() {
        assert_eq!(json_string("Soir\u{e9}e"), r#""Soir\u00e9e""#);
        assert_eq!(json_string("a\"b\\c\n\t"), r#""a\"b\\c\n\t""#);
        assert_eq!(json_string("\u{1}\u{7f}"), r#""\u0001\u007f""#);
        assert_eq!(json_string("\u{1f3b5}"), r#""\ud83c\udfb5""#);
    }
}
