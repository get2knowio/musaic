//! A JPEG decoder that reproduces libjpeg-turbo's output with the settings
//! Pillow uses (`Image.open(...).load()`, after `draft()` when the
//! thumbnail asks for it: islow IDCT, or jidctred.c's 4x4 / 2x2 / 1x1 for
//! DCT scaling 1/2, 1/4, 1/8 with libjpeg-turbo's per-component scaled
//! sizes; fancy upsampling unless scaled to 1/8; YCbCr -> RGB with
//! jdcolor.c's tables), so a thumbnail
//! made from it starts from the same pixels as the Python app's.
//!
//! Supported: 8-bit baseline / extended Huffman (SOF0, SOF1) and
//! progressive (SOF2) frames with one (grayscale) or three components,
//! restart intervals, sampling ratios 1:1, 2:1, 1:2, 2:2 and other integer
//! ratios. Anything else (arithmetic coding, 12-bit, lossless, four
//! components, broken streams) answers `None` and the caller falls back to
//! the `image` crate.

/// Decoded pixels: `channels` is 1 (L) or 3 (RGB).
pub struct Decoded {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub data: Vec<u8>,
}

#[derive(Clone, Default)]
struct Huff {
    /// (code length, code) -> symbol, canonical tables.
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    vals: Vec<u8>,
}

impl Huff {
    fn new(counts: &[u8; 16], vals: Vec<u8>) -> Huff {
        let mut h = Huff {
            vals,
            ..Default::default()
        };
        let mut code = 0i32;
        let mut k = 0i32;
        for l in 1..=16 {
            let n = i32::from(counts[l - 1]);
            if n > 0 {
                h.valptr[l] = k;
                h.mincode[l] = code;
                code += n;
                k += n;
                h.maxcode[l] = code - 1;
            } else {
                h.maxcode[l] = -1;
            }
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        h
    }
}

struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks per line / column, padded to whole MCUs.
    bw: usize,
    bh: usize,
    coefs: Vec<[i32; 64]>,
    dc_tbl: usize,
    ac_tbl: usize,
    dc_pred: i32,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            pos: 0,
            acc: 0,
            n: 0,
        }
    }
    fn fill(&mut self) {
        while self.n <= 56 {
            // Past the end of the interval libjpeg inserts zeros.
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.acc |= u64::from(b) << (56 - self.n);
            self.n += 8;
        }
    }
    fn bit(&mut self) -> i32 {
        if self.n == 0 {
            self.fill();
        }
        let b = (self.acc >> 63) as i32;
        self.acc <<= 1;
        self.n -= 1;
        b
    }
    fn bits(&mut self, k: u32) -> i32 {
        if k == 0 {
            return 0;
        }
        if self.n < k {
            self.fill();
        }
        let v = (self.acc >> (64 - k)) as i32;
        self.acc <<= k;
        self.n -= k;
        v
    }
    fn decode(&mut self, h: &Huff) -> Option<u8> {
        let mut code = 0i32;
        for l in 1..=16 {
            code = (code << 1) | self.bit();
            if code <= h.maxcode[l] {
                let i = h.valptr[l] + code - h.mincode[l];
                return h.vals.get(usize::try_from(i).ok()?).copied();
            }
        }
        None
    }
    /// `HUFF_EXTEND(receive(s), s)`.
    fn receive_extend(&mut self, s: u32) -> i32 {
        if s == 0 {
            return 0;
        }
        let v = self.bits(s);
        if v < (1 << (s - 1)) {
            v - (1 << s) + 1
        } else {
            v
        }
    }
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    comps: Vec<Comp>,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
}

/// Decode at full size.
pub fn decode(b: &[u8]) -> Option<Decoded> {
    decode_scaled(b, |_, _| 1).map(|(d, ..)| d)
}

/// Decode with libjpeg's DCT scaling (`scale_denom` 1, 2, 4 or 8, chosen
/// by `choose(width, height)` from the frame size, as Pillow's `draft()`
/// does): the pixels, the frame's own size and the scale used.
pub fn decode_scaled(
    b: &[u8],
    choose: impl Fn(usize, usize) -> usize,
) -> Option<(Decoded, usize, usize, usize)> {
    if b.get(0..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2usize;
    let mut qt = [[0i32; 64]; 4];
    let mut dc: [Option<Huff>; 4] = Default::default();
    let mut ac: [Option<Huff>; 4] = Default::default();
    let mut frame: Option<Frame> = None;
    let mut restart = 0usize;
    let mut jfif = false;
    let mut adobe: Option<u8> = None;
    let mut eob_run = 0u32;
    loop {
        // Next marker (fill bytes allowed).
        while *b.get(i)? != 0xFF {
            i += 1;
        }
        while *b.get(i)? == 0xFF {
            i += 1;
        }
        let m = *b.get(i)?;
        i += 1;
        match m {
            0xD9 => break,
            0xD0..=0xD7 | 0x01 => continue,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]));
        let seg = b.get(i + 2..i + len)?;
        i += len;
        match m {
            0xE0 if seg.starts_with(b"JFIF\0") => jfif = true,
            0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => adobe = Some(seg[11]),
            0xDB => {
                let mut p = 0;
                while p < seg.len() {
                    let pq = seg[p] >> 4;
                    let tq = usize::from(seg[p] & 15);
                    p += 1;
                    let t = qt.get_mut(tq)?;
                    for k in 0..64 {
                        let v = if pq == 0 {
                            p += 1;
                            i32::from(*seg.get(p - 1)?)
                        } else {
                            p += 2;
                            i32::from(u16::from_be_bytes([*seg.get(p - 2)?, *seg.get(p - 1)?]))
                        };
                        t[ZIGZAG[k]] = v;
                    }
                }
            }
            0xC4 => {
                let mut p = 0;
                while p < seg.len() {
                    let tc = seg[p] >> 4;
                    let th = usize::from(seg[p] & 15);
                    let counts: [u8; 16] = seg.get(p + 1..p + 17)?.try_into().ok()?;
                    let n: usize = counts.iter().map(|c| usize::from(*c)).sum();
                    let vals = seg.get(p + 17..p + 17 + n)?.to_vec();
                    p += 17 + n;
                    let h = Some(Huff::new(&counts, vals));
                    match tc {
                        0 => *dc.get_mut(th)? = h,
                        1 => *ac.get_mut(th)? = h,
                        _ => return None,
                    }
                }
            }
            0xDD => restart = usize::from(u16::from_be_bytes([*seg.first()?, *seg.get(1)?])),
            0xC0..=0xC2 => {
                if frame.is_some() || *seg.first()? != 8 {
                    return None;
                }
                let hd = seg.get(1..6)?;
                let height = usize::from(u16::from_be_bytes([hd[0], hd[1]]));
                let width = usize::from(u16::from_be_bytes([hd[2], hd[3]]));
                let nc = usize::from(hd[4]);
                // Very large frames go to the `image` crate (its limits).
                if width == 0 || height == 0 || width * height > MAX_PIXELS || !(nc == 1 || nc == 3)
                {
                    return None;
                }
                let mut comps = Vec::new();
                for c in 0..nc {
                    let s = seg.get(6 + 3 * c..9 + 3 * c)?;
                    let (h, v) = (usize::from(s[1] >> 4), usize::from(s[1] & 15));
                    if !(1..=4).contains(&h) || !(1..=4).contains(&v) || s[2] > 3 {
                        return None;
                    }
                    comps.push(Comp {
                        id: s[0],
                        h,
                        v,
                        tq: usize::from(s[2]),
                        bw: 0,
                        bh: 0,
                        coefs: Vec::new(),
                        dc_tbl: 0,
                        ac_tbl: 0,
                        dc_pred: 0,
                    });
                }
                let hmax = comps.iter().map(|c| c.h).max()?;
                let vmax = comps.iter().map(|c| c.v).max()?;
                let mcux = width.div_ceil(8 * hmax);
                let mcuy = height.div_ceil(8 * vmax);
                for c in &mut comps {
                    c.bw = mcux * c.h;
                    c.bh = mcuy * c.v;
                    c.coefs = vec![[0i32; 64]; c.bw * c.bh];
                }
                frame = Some(Frame {
                    width,
                    height,
                    progressive: m == 0xC2,
                    comps,
                    hmax,
                    vmax,
                    mcux,
                    mcuy,
                });
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            0xDA => {
                let f = frame.as_mut()?;
                let ns = usize::from(*seg.first()?);
                let mut sel = Vec::new();
                for k in 0..ns {
                    let id = *seg.get(1 + 2 * k)?;
                    let t = *seg.get(2 + 2 * k)?;
                    let ci = f.comps.iter().position(|c| c.id == id)?;
                    f.comps[ci].dc_tbl = usize::from(t >> 4);
                    f.comps[ci].ac_tbl = usize::from(t & 15);
                    sel.push(ci);
                }
                let p = 1 + 2 * ns;
                let (ss, se) = (usize::from(*seg.get(p)?), usize::from(*seg.get(p + 1)?));
                let ah = u32::from(*seg.get(p + 2)? >> 4);
                let al = u32::from(*seg.get(p + 2)? & 15);
                // The entropy-coded data: up to the next marker other than
                // RSTn, split into restart intervals, unstuffed.
                let start = i;
                let mut j = i;
                loop {
                    if *b.get(j)? == 0xFF {
                        let n = *b.get(j + 1)?;
                        if n != 0 && !(0xD0..=0xD7).contains(&n) && n != 0xFF {
                            break;
                        }
                    }
                    j += 1;
                }
                let intervals = split_intervals(&b[start..j]);
                i = j;
                scan(
                    f,
                    &sel,
                    (ss, se, ah, al),
                    &dc,
                    &ac,
                    restart,
                    &intervals,
                    &mut eob_run,
                )?;
            }
            _ => {}
        }
    }
    let f = frame?;
    let s = choose(f.width, f.height);
    if ![1, 2, 4, 8].contains(&s) {
        return None;
    }
    let d = output(&f, &qt, jfif, adobe, s)?;
    Some((d, f.width, f.height, s))
}

/// Frames above this many pixels are left to the `image` crate.
const MAX_PIXELS: usize = 64 * 1024 * 1024;

/// libjpeg keeps coefficients as `JCOEF` (16-bit).
fn jcoef(v: i32) -> i32 {
    i32::from(v as i16)
}

/// A DC magnitude category (at most 16 bits in any valid stream).
fn dc_size(sym: u8) -> Option<u32> {
    (sym <= 16).then_some(u32::from(sym))
}

/// The scan's bytes between RSTn markers, with `FF 00` unstuffed.
fn split_intervals(d: &[u8]) -> Vec<Vec<u8>> {
    let mut out = vec![Vec::new()];
    let mut k = 0;
    while k < d.len() {
        let x = d[k];
        if x == 0xFF {
            match d.get(k + 1) {
                Some(0) => {
                    out.last_mut().unwrap().push(0xFF);
                    k += 2;
                    continue;
                }
                Some(n) if (0xD0..=0xD7).contains(n) => {
                    out.push(Vec::new());
                    k += 2;
                    continue;
                }
                Some(0xFF) => {
                    k += 1;
                    continue;
                }
                _ => {}
            }
        }
        out.last_mut().unwrap().push(x);
        k += 1;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn scan(
    f: &mut Frame,
    sel: &[usize],
    (ss, se, ah, al): (usize, usize, u32, u32),
    dc: &[Option<Huff>; 4],
    ac: &[Option<Huff>; 4],
    restart: usize,
    intervals: &[Vec<u8>],
    eob_run: &mut u32,
) -> Option<()> {
    if !f.progressive && (ss != 0 || se != 63 || ah != 0 || al != 0) {
        return None;
    }
    if f.progressive && (se > 63 || ss > se || (ss == 0 && se != 0) || (ss > 0 && sel.len() != 1)) {
        return None;
    }
    // The scan's units: MCUs for an interleaved scan, single blocks of
    // the component's real area otherwise.
    let mut units: Vec<Vec<(usize, usize)>> = Vec::new();
    if sel.len() == 1 {
        let c = &f.comps[sel[0]];
        let cw = (f.width * c.h).div_ceil(f.hmax);
        let ch = (f.height * c.v).div_ceil(f.vmax);
        for by in 0..ch.div_ceil(8) {
            for bx in 0..cw.div_ceil(8) {
                units.push(vec![(sel[0], by * c.bw + bx)]);
            }
        }
    } else {
        for my in 0..f.mcuy {
            for mx in 0..f.mcux {
                let mut u = Vec::new();
                for &ci in sel {
                    let c = &f.comps[ci];
                    for y in 0..c.v {
                        for x in 0..c.h {
                            u.push((ci, (my * c.v + y) * c.bw + mx * c.h + x));
                        }
                    }
                }
                units.push(u);
            }
        }
    }
    let per = if restart == 0 { usize::MAX } else { restart };
    let empty = Vec::new();
    for (n, chunk) in units.chunks(per.min(units.len().max(1))).enumerate() {
        let data = intervals.get(n).unwrap_or(&empty);
        let mut bits = Bits::new(data);
        for c in &mut f.comps {
            c.dc_pred = 0;
        }
        *eob_run = 0;
        for u in chunk {
            for &(ci, blk) in u {
                let c = &mut f.comps[ci];
                let dct = dc.get(c.dc_tbl)?.as_ref();
                let act = ac.get(c.ac_tbl)?.as_ref();
                let coef = &mut c.coefs[blk];
                if !f.progressive {
                    let s = dc_size(bits.decode(dct?)?)?;
                    c.dc_pred = c.dc_pred.wrapping_add(bits.receive_extend(s));
                    coef[0] = jcoef(c.dc_pred);
                    let act = act?;
                    let mut k = 1;
                    while k < 64 {
                        let rs = bits.decode(act)?;
                        let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
                        if s == 0 {
                            if r != 15 {
                                break;
                            }
                            k += 16;
                            continue;
                        }
                        k += r;
                        if k > 63 {
                            break;
                        }
                        coef[ZIGZAG[k]] = bits.receive_extend(s);
                        k += 1;
                    }
                } else if ss == 0 {
                    if ah == 0 {
                        let s = dc_size(bits.decode(dct?)?)?;
                        c.dc_pred = c.dc_pred.wrapping_add(bits.receive_extend(s));
                        coef[0] = jcoef(c.dc_pred.wrapping_shl(al));
                    } else if bits.bit() != 0 {
                        coef[0] |= 1 << al;
                    }
                } else if ah == 0 {
                    ac_first(&mut bits, act?, coef, ss, se, al, eob_run)?;
                } else {
                    ac_refine(&mut bits, act?, coef, ss, se, al, eob_run)?;
                }
            }
        }
    }
    Some(())
}

fn ac_first(
    bits: &mut Bits,
    h: &Huff,
    coef: &mut [i32; 64],
    ss: usize,
    se: usize,
    al: u32,
    eob_run: &mut u32,
) -> Option<()> {
    if *eob_run > 0 {
        *eob_run -= 1;
        return Some(());
    }
    let mut k = ss;
    while k <= se {
        let rs = bits.decode(h)?;
        let (r, s) = (u32::from(rs >> 4), u32::from(rs & 15));
        if s == 0 {
            if r < 15 {
                *eob_run = (1 << r) - 1;
                if r > 0 {
                    *eob_run += bits.bits(r) as u32;
                }
                break;
            }
            k += 16;
            continue;
        }
        k += r as usize;
        if k > 63 {
            return None;
        }
        coef[ZIGZAG[k]] = jcoef(bits.receive_extend(s).wrapping_shl(al));
        k += 1;
    }
    Some(())
}

/// jdphuff.c `decode_mcu_AC_refine`.
fn ac_refine(
    bits: &mut Bits,
    h: &Huff,
    coef: &mut [i32; 64],
    ss: usize,
    se: usize,
    al: u32,
    eob_run: &mut u32,
) -> Option<()> {
    let p1 = 1i32 << al;
    let m1 = -1i32 << al;
    let mut k = ss;
    if *eob_run == 0 {
        while k <= se {
            let rs = bits.decode(h)?;
            let mut r = i32::from(rs >> 4);
            let s = rs & 15;
            let mut val = 0;
            if s != 0 {
                if s != 1 {
                    return None;
                }
                val = if bits.bit() != 0 { p1 } else { m1 };
            } else if r != 15 {
                *eob_run = 1 << r;
                if r > 0 {
                    *eob_run += bits.bits(r as u32) as u32;
                }
                break;
            }
            while k <= se {
                let z = ZIGZAG[k];
                if coef[z] != 0 {
                    if bits.bit() != 0 && (coef[z] & p1) == 0 {
                        coef[z] += if coef[z] >= 0 { p1 } else { m1 };
                    }
                } else {
                    if r == 0 {
                        if val != 0 {
                            coef[z] = val;
                        }
                        k += 1;
                        break;
                    }
                    r -= 1;
                }
                k += 1;
            }
        }
    }
    if *eob_run > 0 {
        while k <= se {
            let z = ZIGZAG[k];
            if coef[z] != 0 && bits.bit() != 0 && (coef[z] & p1) == 0 {
                coef[z] += if coef[z] >= 0 { p1 } else { m1 };
            }
            k += 1;
        }
        *eob_run -= 1;
    }
    Some(())
}

/// Natural-order index of zigzag position k.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// jidctint.c `jpeg_idct_islow`, `range_limit` included.
fn idct_islow(coef: &[i32; 64], q: &[i32; 64], out: &mut [u8], stride: usize) {
    const CB: i32 = 13;
    const P1: i32 = 2;
    const F0298: i64 = 2446;
    const F0390: i64 = 3196;
    const F0541: i64 = 4433;
    const F0765: i64 = 6270;
    const F0899: i64 = 7373;
    const F1175: i64 = 9633;
    const F1501: i64 = 12299;
    const F1847: i64 = 15137;
    const F1961: i64 = 16069;
    const F2053: i64 = 16819;
    const F2562: i64 = 20995;
    const F3072: i64 = 25172;
    let descale = |x: i64, n: i32| (x + (1i64 << (n - 1))) >> n;
    let range = |x: i64| -> u8 {
        let v = ((x + 512) & 1023) - 512;
        (v + 128).clamp(0, 255) as u8
    };
    // (tmp10..13, tmp0..3) -> 8 outputs
    #[allow(clippy::too_many_arguments)]
    fn butterfly(i: [i64; 8]) -> [i64; 8] {
        let [x0, x1, x2, x3, x4, x5, x6, x7] = i;
        let z2 = x2;
        let z3 = x6;
        let z1 = (z2 + z3) * F0541;
        let tmp2 = z1 + z3 * -F1847;
        let tmp3 = z1 + z2 * F0765;
        let tmp0 = (x0 + x4) << CB;
        let tmp1 = (x0 - x4) << CB;
        let tmp10 = tmp0 + tmp3;
        let tmp13 = tmp0 - tmp3;
        let tmp11 = tmp1 + tmp2;
        let tmp12 = tmp1 - tmp2;
        let (mut t0, mut t1, mut t2, mut t3) = (x7, x5, x3, x1);
        let z1 = t0 + t3;
        let z2 = t1 + t2;
        let z3 = t0 + t2;
        let z4 = t1 + t3;
        let z5 = (z3 + z4) * F1175;
        t0 *= F0298;
        t1 *= F2053;
        t2 *= F3072;
        t3 *= F1501;
        let z1 = z1 * -F0899;
        let z2 = z2 * -F2562;
        let z3 = z3 * -F1961 + z5;
        let z4 = z4 * -F0390 + z5;
        t0 += z1 + z3;
        t1 += z2 + z4;
        t2 += z2 + z3;
        t3 += z1 + z4;
        [
            tmp10 + t3,
            tmp11 + t2,
            tmp12 + t1,
            tmp13 + t0,
            tmp13 - t0,
            tmp12 - t1,
            tmp11 - t2,
            tmp10 - t3,
        ]
    }
    let mut ws = [0i64; 64];
    for col in 0..8 {
        let x: [i64; 8] =
            std::array::from_fn(|r| i64::from(coef[r * 8 + col]) * i64::from(q[r * 8 + col]));
        if x[1..].iter().all(|v| *v == 0) {
            for r in 0..8 {
                ws[r * 8 + col] = x[0] << P1;
            }
            continue;
        }
        let o = butterfly(x);
        for r in 0..8 {
            ws[r * 8 + col] = descale(o[r], CB - P1);
        }
    }
    for row in 0..8 {
        let w: [i64; 8] = std::array::from_fn(|c| ws[row * 8 + c]);
        let line = &mut out[row * stride..row * stride + 8];
        if w[1..].iter().all(|v| *v == 0) {
            line.fill(range(descale(w[0], P1 + 3)));
            continue;
        }
        let o = butterfly(w);
        for c in 0..8 {
            line[c] = range(descale(o[c], CB + P1 + 3));
        }
    }
}

const C13: i32 = 13;
const P2: i32 = 2;

fn descale(x: i64, n: i32) -> i64 {
    (x + (1i64 << (n - 1))) >> n
}

/// `range_limit[x & RANGE_MASK]` of the IDCT output.
fn range(x: i64) -> u8 {
    let v = ((x + 512) & 1023) - 512;
    (v + 128).clamp(0, 255) as u8
}

/// jidctred.c `jpeg_idct_4x4` (DCT scaling 1/2).
fn idct_4x4(coef: &[i32; 64], q: &[i32; 64], out: &mut [u8], stride: usize) {
    const F0211: i64 = 1730;
    const F0509: i64 = 4176;
    const F0601: i64 = 4926;
    const F0765: i64 = 6270;
    const F0899: i64 = 7373;
    const F1061: i64 = 8697;
    const F1451: i64 = 11893;
    const F1847: i64 = 15137;
    const F2172: i64 = 17799;
    const F2562: i64 = 20995;
    let odd = |z1: i64, z2: i64, z3: i64, z4: i64| {
        (
            z1 * -F0211 + z2 * F1451 + z3 * -F2172 + z4 * F1061,
            z1 * -F0509 + z2 * -F0601 + z3 * F0899 + z4 * F2562,
        )
    };
    let mut ws = [0i64; 64];
    for col in 0..8 {
        if col == 4 {
            continue;
        }
        let x: [i64; 8] =
            std::array::from_fn(|r| i64::from(coef[r * 8 + col]) * i64::from(q[r * 8 + col]));
        if [1, 2, 3, 5, 6, 7].iter().all(|r| x[*r] == 0) {
            for r in 0..4 {
                ws[r * 8 + col] = x[0] << P2;
            }
            continue;
        }
        let t0 = x[0] << (C13 + 1);
        let t2 = x[2] * F1847 + x[6] * -F0765;
        let (t10, t12) = (t0 + t2, t0 - t2);
        let (o0, o2) = odd(x[7], x[5], x[3], x[1]);
        let n = C13 - P2 + 1;
        ws[col] = descale(t10 + o2, n);
        ws[3 * 8 + col] = descale(t10 - o2, n);
        ws[8 + col] = descale(t12 + o0, n);
        ws[2 * 8 + col] = descale(t12 - o0, n);
    }
    for row in 0..4 {
        let w = &ws[row * 8..row * 8 + 8];
        let line = &mut out[row * stride..row * stride + 4];
        if [1, 2, 3, 5, 6, 7].iter().all(|c| w[*c] == 0) {
            line.fill(range(descale(w[0], P2 + 3)));
            continue;
        }
        let t0 = w[0] << (C13 + 1);
        let t2 = w[2] * F1847 + w[6] * -F0765;
        let (t10, t12) = (t0 + t2, t0 - t2);
        let (o0, o2) = odd(w[7], w[5], w[3], w[1]);
        let n = C13 + P2 + 3 + 1;
        line[0] = range(descale(t10 + o2, n));
        line[3] = range(descale(t10 - o2, n));
        line[1] = range(descale(t12 + o0, n));
        line[2] = range(descale(t12 - o0, n));
    }
}

/// jidctred.c `jpeg_idct_2x2` (DCT scaling 1/4).
fn idct_2x2(coef: &[i32; 64], q: &[i32; 64], out: &mut [u8], stride: usize) {
    const F0720: i64 = 5906;
    const F0850: i64 = 6967;
    const F1272: i64 = 10426;
    const F3624: i64 = 29692;
    let odd =
        |z7: i64, z5: i64, z3: i64, z1: i64| z7 * -F0720 + z5 * F0850 + z3 * -F1272 + z1 * F3624;
    let mut ws = [0i64; 64];
    for col in [0usize, 1, 3, 5, 7] {
        let x: [i64; 8] =
            std::array::from_fn(|r| i64::from(coef[r * 8 + col]) * i64::from(q[r * 8 + col]));
        if [1, 3, 5, 7].iter().all(|r| x[*r] == 0) {
            ws[col] = x[0] << P2;
            ws[8 + col] = x[0] << P2;
            continue;
        }
        let t10 = x[0] << (C13 + 2);
        let t0 = odd(x[7], x[5], x[3], x[1]);
        let n = C13 - P2 + 2;
        ws[col] = descale(t10 + t0, n);
        ws[8 + col] = descale(t10 - t0, n);
    }
    for row in 0..2 {
        let w = &ws[row * 8..row * 8 + 8];
        let line = &mut out[row * stride..row * stride + 2];
        if [1, 3, 5, 7].iter().all(|c| w[*c] == 0) {
            line.fill(range(descale(w[0], P2 + 3)));
            continue;
        }
        let t10 = w[0] << (C13 + 2);
        let t0 = odd(w[7], w[5], w[3], w[1]);
        let n = C13 + P2 + 3 + 2;
        line[0] = range(descale(t10 + t0, n));
        line[1] = range(descale(t10 - t0, n));
    }
}

/// jidctred.c `jpeg_idct_1x1` (DCT scaling 1/8).
fn idct_1x1(coef: &[i32; 64], q: &[i32; 64], out: &mut [u8]) {
    let dc = i64::from(coef[0]) * i64::from(q[0]);
    out[0] = range(descale(dc, 3));
}

fn output(
    f: &Frame,
    qt: &[[i32; 64]; 4],
    jfif: bool,
    adobe: Option<u8>,
    s: usize,
) -> Option<Decoded> {
    // jdmaster.c jpeg_calc_output_dimensions.
    let (w, h) = (f.width.div_ceil(s), f.height.div_ceil(s));
    let min = 8 / s;
    // Each component, IDCT'd at its scaled size, then upsampled to the
    // output size.
    let mut planes: Vec<Vec<u8>> = Vec::new();
    for c in &f.comps {
        // A component that is subsampled gets a larger IDCT where that
        // saves upsampling (libjpeg-turbo's _DCT_scaled_size loop).
        let mut k = min;
        while k < 8
            && (f.hmax * min).is_multiple_of(c.h * k * 2)
            && (f.vmax * min).is_multiple_of(c.v * k * 2)
        {
            k *= 2;
        }
        let pw = c.bw * k;
        let ph = c.bh * k;
        let mut plane = vec![0u8; pw * ph];
        let q = &qt[c.tq];
        for by in 0..c.bh {
            for bx in 0..c.bw {
                let off = by * k * pw + bx * k;
                let coef = &c.coefs[by * c.bw + bx];
                let out = &mut plane[off..];
                match k {
                    8 => idct_islow(coef, q, out, pw),
                    4 => idct_4x4(coef, q, out, pw),
                    2 => idct_2x2(coef, q, out, pw),
                    _ => idct_1x1(coef, q, out),
                }
            }
        }
        let dw = (f.width * c.h * k).div_ceil(f.hmax * 8);
        let dh = (f.height * c.v * k).div_ceil(f.vmax * 8);
        let (hin, vin) = (c.h * k / min, c.v * k / min);
        if hin == 0 || vin == 0 || !f.hmax.is_multiple_of(hin) || !f.vmax.is_multiple_of(vin) {
            return None;
        }
        let fancy = min > 1;
        planes.push(upsample(
            &plane,
            pw,
            dw,
            dh,
            f.hmax / hin,
            f.vmax / vin,
            w,
            h,
            fancy,
        )?);
    }
    if planes.len() == 1 {
        return Some(Decoded {
            width: w,
            height: h,
            channels: 1,
            data: planes.pop()?,
        });
    }
    // jdapimin.c default_decompress_parms for three components.
    let ids: Vec<u8> = f.comps.iter().map(|c| c.id).collect();
    let ycc = if jfif {
        true
    } else if let Some(t) = adobe {
        t != 0
    } else {
        ids != b"RGB"
    };
    let mut data = vec![0u8; w * h * 3];
    let n = w * h;
    for p in 0..n {
        let (y, cb, cr) = (planes[0][p], planes[1][p], planes[2][p]);
        let rgb = if ycc { ycc_rgb(y, cb, cr) } else { [y, cb, cr] };
        data[p * 3..p * 3 + 3].copy_from_slice(&rgb);
    }
    Some(Decoded {
        width: w,
        height: h,
        channels: 3,
        data,
    })
}

/// jdcolor.c `ycc_rgb_convert` (build_ycc_rgb_table).
fn ycc_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    const ONE_HALF: i64 = 1 << 15;
    let (y, cb, cr) = (i64::from(y), i64::from(cb) - 128, i64::from(cr) - 128);
    let r = y + ((91881 * cr + ONE_HALF) >> 16);
    let g = y + ((-22554 * cb + ONE_HALF + -46802 * cr) >> 16);
    let b = y + ((116130 * cb + ONE_HALF) >> 16);
    [
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    ]
}

/// jdsample.c: fancy h2v1 / h1v2 / h2v2 (libjpeg-turbo's rounding),
/// otherwise box replication; then cropped to the image. Rows above the
/// first and below the last real row repeat the edge row (jdmainct.c's
/// context pointers).
#[allow(clippy::too_many_arguments)]
fn upsample(
    plane: &[u8],
    pw: usize,
    dw: usize,
    dh: usize,
    fx: usize,
    fy: usize,
    w: usize,
    h: usize,
    fancy: bool,
) -> Option<Vec<u8>> {
    if dw == 0 || dh == 0 {
        return None;
    }
    let at = |x: usize, y: usize| i32::from(plane[y.min(dh - 1) * pw + x]);
    let ow = dw * fx;
    let oh = dh * fy;
    let mut full = vec![0u8; ow * oh];
    match (fx, fy) {
        (1, 1) => {
            for y in 0..dh {
                full[y * ow..y * ow + dw].copy_from_slice(&plane[y * pw..y * pw + dw]);
            }
        }
        (2, 1) if fancy && dw > 2 => {
            for y in 0..dh {
                let o = &mut full[y * ow..(y + 1) * ow];
                let i = |x: usize| at(x, y);
                o[0] = i(0) as u8;
                o[1] = ((i(0) * 3 + i(1) + 2) >> 2) as u8;
                for x in 1..dw - 1 {
                    let v = i(x) * 3;
                    o[2 * x] = ((v + i(x - 1) + 1) >> 2) as u8;
                    o[2 * x + 1] = ((v + i(x + 1) + 2) >> 2) as u8;
                }
                let v = i(dw - 1);
                o[2 * dw - 2] = ((v * 3 + i(dw - 2) + 1) >> 2) as u8;
                o[2 * dw - 1] = v as u8;
            }
        }
        (1, 2) if fancy => {
            for y in 0..dh {
                for v in 0..2 {
                    let near = if v == 0 { y.saturating_sub(1) } else { y + 1 };
                    let bias = if v == 0 { 1 } else { 2 };
                    let o = &mut full[(2 * y + v) * ow..(2 * y + v + 1) * ow];
                    for (x, ox) in o.iter_mut().enumerate().take(dw) {
                        *ox = ((at(x, y) * 3 + at(x, near) + bias) >> 2) as u8;
                    }
                }
            }
        }
        (2, 2) if fancy && dw > 2 => {
            for y in 0..dh {
                for v in 0..2 {
                    let near = if v == 0 { y.saturating_sub(1) } else { y + 1 };
                    let col = |x: usize| at(x, y) * 3 + at(x, near);
                    let o = &mut full[(2 * y + v) * ow..(2 * y + v + 1) * ow];
                    let mut this = col(0);
                    let mut next = col(1);
                    o[0] = ((this * 4 + 8) >> 4) as u8;
                    o[1] = ((this * 3 + next + 7) >> 4) as u8;
                    let mut last = this;
                    this = next;
                    for x in 2..dw {
                        next = col(x);
                        o[2 * x - 2] = ((this * 3 + last + 8) >> 4) as u8;
                        o[2 * x - 1] = ((this * 3 + next + 7) >> 4) as u8;
                        last = this;
                        this = next;
                    }
                    o[2 * dw - 2] = ((this * 3 + last + 8) >> 4) as u8;
                    o[2 * dw - 1] = ((this * 4 + 7) >> 4) as u8;
                }
            }
        }
        _ => {
            for y in 0..oh {
                for x in 0..ow {
                    full[y * ow + x] = at(x / fx, y / fy) as u8;
                }
            }
        }
    }
    if ow < w || oh < h {
        return None;
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        out[y * w..(y + 1) * w].copy_from_slice(&full[y * ow..y * ow + w]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(progressive: bool, sampling: jpeg_encoder::SamplingFactor) -> Vec<u8> {
        let (w, h) = (37u16, 29u16);
        let px: Vec<u8> = (0..usize::from(w) * usize::from(h) * 3)
            .map(|i| ((i * 7919) % 251) as u8)
            .collect();
        let mut out = Vec::new();
        let mut enc = jpeg_encoder::Encoder::new(&mut out, 75);
        enc.set_progressive(progressive);
        enc.set_sampling_factor(sampling);
        enc.set_restart_interval(3);
        enc.encode(&px, w, h, jpeg_encoder::ColorType::Rgb).unwrap();
        out
    }

    /// Covers are untrusted input; a truncated or corrupted
    /// stream must answer `None` or some pixels, never panic.
    #[test]
    fn corrupt_streams_never_panic() {
        use jpeg_encoder::SamplingFactor::*;
        for (prog, sf) in [(false, F_2_2), (true, F_2_2), (false, F_1_1), (true, F_2_1)] {
            let good = sample(prog, sf);
            let d = decode(&good).expect("decodes its own stream");
            assert_eq!((d.width, d.height, d.channels), (37, 29, 3));
            for cut in (0..good.len()).step_by(7) {
                let _ = decode(&good[..cut]);
            }
            for pos in (2..good.len()).step_by(3) {
                for x in [0x00u8, 0xFF, 0x5A, 0xC4] {
                    let mut bad = good.clone();
                    bad[pos] ^= x;
                    let _ = decode(&bad);
                    let _ = decode_scaled(&bad, |_, _| 2);
                }
            }
        }
    }
}
