//! `thumbs.make_thumbnail`: Pillow's
//! `Image.open(...)`, `convert("RGB")` for RGBA / P / LA,
//! `img.thumbnail((384, 384))`, `save(format="JPEG", quality=82,
//! optimize=True, progressive=True)`, reproduced step by step so the
//! pixels match the Python app's within decoder rounding:
//!
//! * `thumbnail()` keeps the aspect ratio with Pillow's rounding
//!   (`preserve_aspect_ratio`), does nothing to the size when the image
//!   already fits, and uses BICUBIC with `reducing_gap=2.0`.
//! * `reducing_gap`: a JPEG source is `draft()`ed — decoded with libjpeg's
//!   DCT scaling by 1/2, 1/4 or 1/8 when both sides are at least
//!   2·384 px times that factor ([`crate::jpegdec::decode_scaled`]) — and
//!   any other source `reduce()`d by the integer factor `int(w / 384 / 2)`
//!   (box average, Reduce.c's rounding) before the resampling. Only for a
//!   JPEG that crate::jpegdec cannot read is the DCT scaling approximated
//!   by a box average of the same factor.
//! * Resampling is Pillow's `ImagingResample` (`Resample.c`): separable
//!   convolution with the a = -0.5 cubic, support scaled by the reduction
//!   factor, coefficients normalised then rounded to 22-bit fixed point,
//!   horizontal pass first, each pass rounded and clipped to 8 bits.
//! * Encoding follows libjpeg-turbo, which Pillow links: RGB -> YCbCr with
//!   jccolor.c's fixed-point tables (Y rounds with ONE_HALF, Cb / Cr with
//!   ONE_HALF - 1), 4:2:0 with h2v2 box-averaged chroma (Pillow's
//!   `subsampling=-1`), the IJG tables scaled for quality 82, islow DCT;
//!   progressive with optimised Huffman tables. `jpeg-encoder` quantises
//!   with a 15-bit reciprocal where libjpeg-turbo uses 16 + log2(divisor)
//!   bits, so a few coefficients round the other way (measured: 52-72 dB
//!   against the Python app's file on every fixture cover and test pattern,
//!   bound 45 dB), and its progressive script is spectral selection only,
//!   so files run ~10-15% larger than Pillow's. No ICC profile or EXIF is
//!   carried over (Pillow only writes what is passed to `save`), and EXIF
//!   orientation is not applied (the Python app does not call `exif_transpose`).
//! * A grayscale source stays grayscale (mode L); 16-bit grayscale (mode
//!   I;16) cannot be saved as JPEG, so no thumbnail; a truncated JPEG
//!   raises in Pillow's `load()`, so no thumbnail.
//!
//! * JPEG sources are decoded by [`crate::jpegdec`], which reproduces
//!   libjpeg-turbo's pixels (bit-identical to Pillow's on every fixture
//!   cover); other formats and the JPEG variants it does not read go
//!   through the `image` crate.
//!
//! Not reproduced: CMYK JPEG sources (Pillow keeps CMYK and writes a CMYK
//! thumbnail; here they decode through the `image` crate to RGB, written
//! as YCbCr).

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, ImageReader};
use jpeg_encoder::{
    ChromaSubsamplingMethod, ColorType as JpegColor, Encoder, ImageBuffer, JpegColorType,
    SamplingFactor,
};

/// `thumbs.THUMB_SIZE`: px on the long edge.
pub const THUMB_SIZE: u32 = 384;
/// `thumbs.THUMB_QUALITY`.
pub const THUMB_QUALITY: u8 = 82;

/// 8-bit pixels, 1 (L) or 3 (RGB) interleaved channels.
#[derive(Clone)]
struct Pixels {
    w: usize,
    h: usize,
    ch: usize,
    data: Vec<u8>,
}

/// `make_thumbnail(image_bytes)`: JPEG bytes, or `None` where the Python
/// app logs a failure and returns None. The failure is logged as the Python
/// app does (thumbs.py:98, WARNING on `musaic.thumbs`), with this build's
/// reason in place of Pillow's exception text.
pub fn make_thumbnail(image_bytes: &[u8]) -> Option<Vec<u8>> {
    match make_thumbnail_sized(image_bytes, THUMB_SIZE) {
        Ok(out) => Some(out),
        Err(why) => {
            musaic_log::warning(THUMBS_LOG, &format!("thumbnail generation failed: {why}"));
            None
        }
    }
}

/// The Python app's logger for `thumbs.py` (`musaic.thumbs`).
pub const THUMBS_LOG: &str = "musaic.thumbs";

/// Pillow's `UnidentifiedImageError` text, minus its stream repr.
const UNIDENTIFIED: &str = "cannot identify image file";

fn make_thumbnail_sized(image_bytes: &[u8], max_size: u32) -> Result<Vec<u8>, String> {
    let max = max_size as usize;
    let reader = ImageReader::new(Cursor::new(image_bytes))
        .with_guessed_format()
        .map_err(|_| UNIDENTIFIED.to_string())?;
    let format = reader.format().ok_or_else(|| UNIDENTIFIED.to_string())?;
    let is_jpeg = format == ImageFormat::Jpeg;
    if is_jpeg && !jpeg_reaches_eoi(image_bytes) {
        // Pillow: "image file is truncated (N bytes not processed)".
        return Err("image file is truncated".into());
    }
    // libjpeg-exact decode (DCT-scaled as `draft()` asks) where the stream
    // is one crate::jpegdec reads; the `image` crate otherwise.
    let exact = if is_jpeg {
        crate::jpegdec::decode_scaled(image_bytes, |w, h| draft_scale(w, h, max))
    } else {
        None
    };
    let (px, w0, h0, s, scaled) = match exact {
        Some((d, w0, h0, s)) => {
            let px = Pixels {
                w: d.width,
                h: d.height,
                ch: d.channels,
                data: d.data,
            };
            (px, w0, h0, s, true)
        }
        None => {
            let img = reader.decode().map_err(|e| e.to_string())?;
            // Pillow's OSError for a mode it cannot save as JPEG.
            let px = pillow_pixels(img).ok_or("cannot write mode I;16 as JPEG")?;
            let (w0, h0) = (px.w, px.h);
            let s = if is_jpeg { draft_scale(w0, h0, max) } else { 1 };
            (px, w0, h0, s, false)
        }
    };
    let px = thumbnail(px, (w0, h0), s, scaled, max);
    encode(&px)
}

/// `JpegImageFile.draft(None, (2 * max, 2 * max))`'s DCT scale (only asked
/// for when the image does not fit, which a scale above 1 implies).
fn draft_scale(w: usize, h: usize, max: usize) -> usize {
    let req = max * 2;
    let scale = (w / req).min(h / req);
    [8usize, 4, 2, 1]
        .into_iter()
        .find(|s| scale >= *s)
        .unwrap_or(1)
}

/// The pixels Pillow resizes: L stays L, RGB stays RGB, RGBA / LA / P are
/// `convert("RGB")`ed (alpha dropped, palette expanded), 16-bit RGB(A)
/// opens as 8-bit RGB (high byte); 16-bit grayscale (I;16) has no JPEG
/// writer.
fn pillow_pixels(img: DynamicImage) -> Option<Pixels> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let (ch, data) = match img {
        DynamicImage::ImageLuma8(b) => (1, b.into_raw()),
        DynamicImage::ImageLuma16(_) => return None,
        DynamicImage::ImageRgb8(b) => (3, b.into_raw()),
        DynamicImage::ImageRgb16(b) => (3, b.into_raw().iter().map(|v| (v >> 8) as u8).collect()),
        DynamicImage::ImageRgba16(b) => (
            3,
            b.into_raw()
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .map(|v| (v >> 8) as u8)
                .collect(),
        ),
        DynamicImage::ImageLumaA16(b) => (
            3,
            b.into_raw()
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0]; 3])
                .map(|v| (v >> 8) as u8)
                .collect(),
        ),
        other => (3, other.into_rgb8().into_raw()),
    };
    Some(Pixels { w, h, ch, data })
}

/// `Image.thumbnail((max, max))` with its defaults (BICUBIC,
/// `reducing_gap=2.0`), on pixels that `draft()` may already have scaled
/// by `s` from `(w0, h0)` (`scaled`; otherwise a box average of `s`
/// stands in for the DCT scaling).
fn thumbnail(px: Pixels, (w0, h0): (usize, usize), s: usize, scaled: bool, max: usize) -> Pixels {
    let Some((tw, th)) = preserve_aspect_ratio(w0, h0, max, max) else {
        return px;
    };
    let mut px = px;
    if s > 1 && !scaled {
        px = reduce(&px, s, s, [0, 0, px.w, px.h]);
    }
    // draft() hands resize() the box (0, 0, w0 / s, h0 / s) as floats.
    let bx = [
        0.0,
        0.0,
        (w0 as f64 / s as f64) as f32,
        (h0 as f64 / s as f64) as f32,
    ];
    if (px.w, px.h) == (tw, th) {
        return px;
    }
    resize(px, tw, th, bx, 2.0)
}

/// `thumbnail()`'s `preserve_aspect_ratio`: `None` when the image fits.
fn preserve_aspect_ratio(w: usize, h: usize, x: usize, y: usize) -> Option<(usize, usize)> {
    if x >= w && y >= h {
        return None;
    }
    let aspect = w as f64 / h as f64;
    let round_aspect = |number: f64, key: &dyn Fn(f64) -> f64| -> usize {
        let (lo, hi) = (number.floor(), number.ceil());
        // min(floor, ceil, key=key): the first on ties.
        let best = if key(hi) < key(lo) { hi } else { lo };
        (best as usize).max(1)
    };
    let (xf, yf) = (x as f64, y as f64);
    if xf / yf >= aspect {
        let nx = round_aspect(yf * aspect, &|n| (aspect - n / yf).abs());
        Some((nx, y))
    } else {
        let ny = round_aspect(xf / aspect, &|n| {
            if n == 0.0 {
                0.0
            } else {
                (aspect - xf / n).abs()
            }
        });
        Some((x, ny))
    }
}

/// `Image.resize(size, BICUBIC, box, reducing_gap)`.
fn resize(px: Pixels, tw: usize, th: usize, bx: [f32; 4], gap: f64) -> Pixels {
    let mut px = px;
    let mut bx = bx;
    let fx = (((bx[2] - bx[0]) as f64 / tw as f64 / gap) as usize).max(1);
    let fy = (((bx[3] - bx[1]) as f64 / th as f64 / gap) as usize).max(1);
    if fx > 1 || fy > 1 {
        // _get_safe_box: BICUBIC support 2.0 - 0.5.
        let support = 1.5f64;
        let sx = (bx[2] - bx[0]) as f64 / tw as f64;
        let sy = (bx[3] - bx[1]) as f64 / th as f64;
        let rb = [
            ((bx[0] as f64 - support * sx) as i64).max(0) as usize,
            ((bx[1] as f64 - support * sy) as i64).max(0) as usize,
            ((bx[2] as f64 + support * sx).ceil() as usize).min(px.w),
            ((bx[3] as f64 + support * sy).ceil() as usize).min(px.h),
        ];
        px = reduce(&px, fx, fy, [rb[0], rb[1], rb[2] - rb[0], rb[3] - rb[1]]);
        bx = [
            ((bx[0] as f64 - rb[0] as f64) / fx as f64) as f32,
            ((bx[1] as f64 - rb[1] as f64) / fy as f64) as f32,
            ((bx[2] as f64 - rb[0] as f64) / fx as f64) as f32,
            ((bx[3] as f64 - rb[1] as f64) / fy as f64) as f32,
        ];
    }
    resample(&px, tw, th, bx)
}

/// `ImagingReduce` over `bx = (x0, y0, width, height)`: the mean of each
/// `fx` x `fy` cell (partial cells at the right / bottom over the pixels
/// they have), rounded as Reduce.c does (`(sum + n/2) * mult >> 24`).
fn reduce(px: &Pixels, fx: usize, fy: usize, bx: [usize; 4]) -> Pixels {
    let ow = bx[2].div_ceil(fx);
    let oh = bx[3].div_ceil(fy);
    let mult = |n: usize| -> u64 { (4_294_967_296.0f32 / (256 * n) as u32 as f32) as u32 as u64 };
    let mut out = vec![0u8; ow * oh * px.ch];
    for oy in 0..oh {
        let y0 = bx[1] + oy * fy;
        let y1 = (y0 + fy).min(bx[1] + bx[3]);
        for ox in 0..ow {
            let x0 = bx[0] + ox * fx;
            let x1 = (x0 + fx).min(bx[0] + bx[2]);
            let n = (x1 - x0) * (y1 - y0);
            for c in 0..px.ch {
                let mut ss = (n / 2) as u64;
                for y in y0..y1 {
                    for x in x0..x1 {
                        ss += u64::from(px.data[(y * px.w + x) * px.ch + c]);
                    }
                }
                out[(oy * ow + ox) * px.ch + c] = ((ss * mult(n)) >> 24) as u8;
            }
        }
    }
    Pixels {
        w: ow,
        h: oh,
        ch: px.ch,
        data: out,
    }
}

const PRECISION_BITS: u32 = 32 - 8 - 2;

fn bicubic(x: f64) -> f64 {
    let a = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((a + 2.0) * x - (a + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * a
    } else {
        0.0
    }
}

/// `precompute_coeffs` + `normalize_coeffs_8bpc`: per output pixel the
/// first input index, the count, and the fixed-point weights.
fn coeffs(
    in_size: usize,
    in0: f32,
    in1: f32,
    out_size: usize,
) -> (usize, Vec<(usize, usize)>, Vec<i32>) {
    let scale = f64::from(in1 - in0) / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = 2.0 * filterscale;
    let ksize = support.ceil() as usize * 2 + 1;
    let mut bounds = Vec::with_capacity(out_size);
    let mut kk = vec![0i32; out_size * ksize];
    for xx in 0..out_size {
        let center = f64::from(in0) + (xx as f64 + 0.5) * scale;
        let ss = 1.0 / filterscale;
        let xmin = ((center - support + 0.5) as i64).max(0) as usize;
        let xmax = ((center + support + 0.5) as i64).min(in_size as i64) as usize;
        let n = xmax.saturating_sub(xmin);
        let mut k = vec![0f64; n];
        let mut ww = 0.0;
        for (x, kx) in k.iter_mut().enumerate() {
            let w = bicubic(((x + xmin) as f64 - center + 0.5) * ss);
            *kx = w;
            ww += w;
        }
        for (x, kx) in k.iter().enumerate() {
            let v = if ww != 0.0 { kx / ww } else { *kx };
            let f = v * f64::from(1u32 << PRECISION_BITS);
            kk[xx * ksize + x] = if v < 0.0 {
                (-0.5 + f) as i32
            } else {
                (0.5 + f) as i32
            };
        }
        bounds.push((xmin, n));
    }
    (ksize, bounds, kk)
}

fn clip8(v: i64) -> u8 {
    (v >> PRECISION_BITS).clamp(0, 255) as u8
}

/// `ImagingResample(im, (tw, th), BICUBIC, box)`.
fn resample(px: &Pixels, tw: usize, th: usize, bx: [f32; 4]) -> Pixels {
    let need_h = tw != px.w || bx[0] != 0.0 || bx[2] != tw as f32;
    let need_v = th != px.h || bx[1] != 0.0 || bx[3] != th as f32;
    let (kh, bh, wh) = coeffs(px.w, bx[0], bx[2], tw);
    let (kv, mut bv, wv) = coeffs(px.h, bx[1], bx[3], th);
    let ch = px.ch;
    let half = 1i64 << (PRECISION_BITS - 1);
    let mut cur = px.clone();
    if need_h {
        // Only the rows the vertical pass reads.
        let first = bv[0].0;
        let last = bv[th - 1].0 + bv[th - 1].1;
        for b in &mut bv {
            b.0 -= first;
        }
        let rows = last - first;
        let mut out = vec![0u8; tw * rows * ch];
        for yy in 0..rows {
            let line = &px.data[(yy + first) * px.w * ch..(yy + first + 1) * px.w * ch];
            for (xx, &(xmin, n)) in bh.iter().enumerate() {
                let k = &wh[xx * kh..xx * kh + n];
                for c in 0..ch {
                    let mut ss = half;
                    for (x, kx) in k.iter().enumerate() {
                        ss += i64::from(line[(x + xmin) * ch + c]) * i64::from(*kx);
                    }
                    out[(yy * tw + xx) * ch + c] = clip8(ss);
                }
            }
        }
        cur = Pixels {
            w: tw,
            h: rows,
            ch,
            data: out,
        };
    }
    if need_v {
        let mut out = vec![0u8; cur.w * th * ch];
        for (yy, &(ymin, n)) in bv.iter().enumerate() {
            let k = &wv[yy * kv..yy * kv + n];
            for xx in 0..cur.w {
                for c in 0..ch {
                    let mut ss = half;
                    for (y, ky) in k.iter().enumerate() {
                        ss += i64::from(cur.data[((y + ymin) * cur.w + xx) * ch + c])
                            * i64::from(*ky);
                    }
                    out[(yy * cur.w + xx) * ch + c] = clip8(ss);
                }
            }
        }
        cur = Pixels {
            w: cur.w,
            h: th,
            ch,
            data: out,
        };
    }
    cur
}

/// RGB rows converted with libjpeg's `rgb_ycc_convert` arithmetic.
struct LibjpegYcc<'a> {
    data: &'a [u8],
    w: u16,
    h: u16,
}

fn rgb_ycc(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    const ONE_HALF: i32 = 1 << 15;
    const CBCR_OFFSET: i32 = 128 << 16;
    let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
    let y = (19595 * r + 38470 * g + 7471 * b + ONE_HALF) >> 16;
    let cb = (-11059 * r - 21709 * g + 32768 * b + CBCR_OFFSET + ONE_HALF - 1) >> 16;
    let cr = (32768 * r - 27439 * g - 5329 * b + CBCR_OFFSET + ONE_HALF - 1) >> 16;
    (y as u8, cb as u8, cr as u8)
}

impl ImageBuffer for LibjpegYcc<'_> {
    fn get_jpeg_color_type(&self) -> JpegColorType {
        JpegColorType::Ycbcr
    }
    fn width(&self) -> u16 {
        self.w
    }
    fn height(&self) -> u16 {
        self.h
    }
    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        let w = usize::from(self.w);
        let line = &self.data[usize::from(y) * w * 3..(usize::from(y) + 1) * w * 3];
        for p in line.as_chunks::<3>().0 {
            let (yy, cb, cr) = rgb_ycc(p[0], p[1], p[2]);
            buffers[0].push(yy);
            buffers[1].push(cb);
            buffers[2].push(cr);
        }
    }
}

fn encode(px: &Pixels) -> Result<Vec<u8>, String> {
    let too_big = |_| format!("{}x{} is too large for JPEG", px.w, px.h);
    let (w, h) = (
        u16::try_from(px.w).map_err(too_big)?,
        u16::try_from(px.h).map_err(too_big)?,
    );
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, THUMB_QUALITY);
    enc.set_progressive(true);
    enc.set_optimized_huffman_tables(true);
    enc.set_sampling_factor(SamplingFactor::F_2_2);
    enc.set_chroma_subsampling_method(ChromaSubsamplingMethod::Average);
    if px.ch == 1 {
        enc.encode(&px.data, w, h, JpegColor::Luma)
            .map_err(|e| e.to_string())?;
    } else {
        enc.encode_image(LibjpegYcc {
            data: &px.data,
            w,
            h,
        })
        .map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Pillow raises "image file is truncated" when a JPEG ends before its
/// EOI marker; zune-jpeg pads with grey instead.
fn jpeg_reaches_eoi(b: &[u8]) -> bool {
    let Some(sos) = b.windows(2).position(|w| w == [0xFF, 0xDA]) else {
        return false;
    };
    b[sos..].windows(2).any(|w| w == [0xFF, 0xD9])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `preserve_aspect_ratio` on the fixture cover shapes: 600x581 ->
    /// 384x372, 591x600 -> 378x384, 500x490 -> 384x376 (the sizes the
    /// Python app's thumbnails of those covers have).
    #[test]
    fn thumbnail_sizes_match_the_python_app() {
        assert_eq!(preserve_aspect_ratio(600, 581, 384, 384), Some((384, 372)));
        assert_eq!(preserve_aspect_ratio(591, 600, 384, 384), Some((378, 384)));
        assert_eq!(preserve_aspect_ratio(500, 490, 384, 384), Some((384, 376)));
        assert_eq!(preserve_aspect_ratio(600, 600, 384, 384), Some((384, 384)));
        assert_eq!(preserve_aspect_ratio(320, 240, 384, 384), None);
    }
}
