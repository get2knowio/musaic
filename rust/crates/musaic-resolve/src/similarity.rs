//! Title similarity for the anchor vote's tie-break.
//!
//! The Python reference rule (`pick`, tie-break v2) computes
//! `round(difflib.SequenceMatcher(None, _norm_title(a), _norm_title(b)).ratio(), 2)`.
//! This module ports that exactly:
//!
//! * [`norm_title`] — `re.sub(r"[^a-z0-9]+", " ", unicodedata.normalize("NFKD", t.lower())).strip()`
//! * [`SequenceMatcher`] — Ratcliff/Obershelp longest-matching-block recursion,
//!   with `isjunk=None` and `autojunk=True` (elements of `b` occurring more than
//!   `len(b) // 100 + 1` times are "popular" and dropped from the index when
//!   `len(b) >= 200`), over Unicode scalar values (Python iterates `str` by code point).
//! * [`title_similarity_hundredths`] — the ratio rounded to two places, as an
//!   integer number of hundredths so callers compare integers, not floats.
//!
//! The port is checked against Python's own difflib by
//! `tests/difflib_golden.rs` (a golden recorded from Python's difflib).

use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

/// Python `_norm_title`: lowercase, NFKD, every run of characters outside
/// `[a-z0-9]` becomes one space, then trimmed.
pub fn norm_title(t: &str) -> String {
    let decomposed: String = t.to_lowercase().nfkd().collect();
    let mut out = String::with_capacity(decomposed.len());
    let mut in_gap = false;
    for c in decomposed.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if in_gap {
                out.push(' ');
                in_gap = false;
            }
            out.push(c);
        } else {
            in_gap = true;
        }
    }
    // A gap at the very start was pushed in front of the first kept character;
    // `strip()` removes it. A trailing gap is never pushed.
    out.trim_start_matches(' ').to_owned()
}

/// A matching block `(i, j, size)`: `a[i..i+size] == b[j..j+size]`.
pub type Block = (usize, usize, usize);

/// Port of `difflib.SequenceMatcher(None, a, b)` (default `autojunk=True`).
#[derive(Debug, Clone)]
pub struct SequenceMatcher<T> {
    a: Vec<T>,
    b: Vec<T>,
    /// For each non-popular element of `b`, the ascending indices where it occurs.
    b2j: HashMap<T, Vec<usize>>,
}

impl<T: Eq + std::hash::Hash + Clone> SequenceMatcher<T> {
    pub fn new(a: Vec<T>, b: Vec<T>) -> Self {
        let mut b2j: HashMap<T, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            b2j.entry(elt.clone()).or_default().push(i);
        }
        // isjunk is None, so there is no junk set. autojunk:
        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            b2j.retain(|_, idxs| idxs.len() <= ntest);
        }
        Self { a, b, b2j }
    }

    /// `find_longest_match(alo, ahi, blo, bhi)`. With no junk, only the first
    /// pair of extension loops in CPython can do anything; they extend the
    /// match over popular elements, which were never indexed.
    pub fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Block {
        let (a, b) = (&self.a, &self.b);
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();
        for (i, ai) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(ai) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let prev = if j == 0 {
                        0
                    } else {
                        j2len.get(&(j - 1)).copied().unwrap_or(0)
                    };
                    let k = prev + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && a[besti + bestsize] == b[bestj + bestsize]
        {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    }

    /// `get_matching_blocks()`: sorted, adjacent blocks collapsed, ending with
    /// the `(len(a), len(b), 0)` sentinel.
    pub fn matching_blocks(&self) -> Vec<Block> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks: Vec<Block> = Vec::new();
        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let (i, j, k) = self.find_longest_match(alo, ahi, blo, bhi);
            if k > 0 {
                blocks.push((i, j, k));
                if alo < i && blo < j {
                    queue.push((alo, i, blo, j));
                }
                if i + k < ahi && j + k < bhi {
                    queue.push((i + k, ahi, j + k, bhi));
                }
            }
        }
        blocks.sort_unstable();
        let mut out: Vec<Block> = Vec::with_capacity(blocks.len() + 1);
        let (mut i1, mut j1, mut k1) = (0, 0, 0);
        for (i2, j2, k2) in blocks {
            if i1 + k1 == i2 && j1 + k1 == j2 {
                k1 += k2;
            } else {
                if k1 > 0 {
                    out.push((i1, j1, k1));
                }
                (i1, j1, k1) = (i2, j2, k2);
            }
        }
        if k1 > 0 {
            out.push((i1, j1, k1));
        }
        out.push((la, lb, 0));
        out
    }

    /// Number of matched elements (sum of matching-block sizes).
    pub fn matches(&self) -> usize {
        self.matching_blocks().iter().map(|b| b.2).sum()
    }

    /// `ratio()`: `2.0 * matches / (len(a) + len(b))`, or 1.0 when both are empty.
    pub fn ratio(&self) -> f64 {
        let length = self.a.len() + self.b.len();
        if length == 0 {
            return 1.0;
        }
        2.0 * self.matches() as f64 / length as f64
    }
}

/// `SequenceMatcher(None, a, b).ratio()` over the code points of two strings.
pub fn ratio(a: &str, b: &str) -> f64 {
    SequenceMatcher::new(a.chars().collect(), b.chars().collect()).ratio()
}

/// Python `round(x, 2)` as an integer number of hundredths.
///
/// Python rounds the exact binary value correctly (half-to-even on exact ties).
/// No `f64` is ever exactly halfway between two hundredths (that would need a
/// factor of 25 in a power-of-two denominator), so Rust's exact decimal
/// formatting gives the same digits.
pub fn round2_hundredths(x: f64) -> i64 {
    let s = format!("{x:.2}");
    let neg = s.starts_with('-');
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    let v: i64 = digits.parse().unwrap_or(0);
    if neg { -v } else { v }
}

/// The tie-break key: `round(ratio(norm(a), norm(b)), 2)` in hundredths.
pub fn title_similarity_hundredths(album_title: &str, candidate_title: &str) -> i64 {
    round2_hundredths(ratio(
        &norm_title(album_title),
        &norm_title(candidate_title),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Case- and punctuation-insensitive. NOT fully accent-insensitive:
    // NFKD splits "é" into "e" + U+0301 and the combining mark becomes a space,
    // so a mid-word accent splits the word ("Été" -> "e te"), exactly as the
    // Python reference does (checked: `_norm_title("  Été — Live!! ")` ==
    // 'e te live', `_norm_title("Café")` == 'cafe'). tests/difflib_golden.rs
    // checks this function against Python output for every golden pair.
    #[test]
    fn norm_title_matches_python_reference() {
        assert_eq!(norm_title("  Été — Live!! "), "e te live");
        assert_eq!(norm_title("Café"), "cafe");
        assert_eq!(norm_title(""), "");
        assert_eq!(norm_title("!!!"), "");
    }

    #[test]
    fn empty_sequences_ratio_is_one() {
        assert_eq!(ratio("", ""), 1.0);
        assert_eq!(ratio("a", ""), 0.0);
    }

    #[test]
    fn round2_examples() {
        assert_eq!(round2_hundredths(0.666_666), 67);
        assert_eq!(round2_hundredths(1.0), 100);
        assert_eq!(round2_hundredths(0.0), 0);
    }
}
