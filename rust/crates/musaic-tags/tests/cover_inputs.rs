//! `normalize_to_jpeg` and `build_picture` on unusual source images.
//!
//! Every input is synthesised here (drawn pixels encoded by the `image` and
//! `jpeg-encoder` crates, or hand-edited marker segments); no image file is
//! read. The golden scenarios cover the common formats; these are the
//! shapes that tend to go wrong in a re-encode: wrong colour for CMYK, alpha
//! and 16-bit sources, metadata segments leaking into the output, dropped
//! EXIF orientation (applied to the pixels, since the output has no EXIF), extreme sizes, and damaged data.

use std::io::Cursor;

use image::{
    DynamicImage, ImageBuffer, ImageFormat, Luma, Rgb, RgbImage, Rgba, RgbaImage, codecs::gif,
};
use jpeg_encoder::{ColorType, Encoder};
use musaic_tags::{FlacFile, Picture, build_picture, normalize_to_jpeg};

const RED: [u8; 3] = [200, 30, 30];
const BLUE: [u8; 3] = [30, 30, 200];

/// Left half red, right half blue.
fn two_tone(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, _| Rgb(if x < w / 2 { RED } else { BLUE }))
}

fn encode(img: DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).expect("encode");
    out.into_inner()
}

fn baseline_jpeg(img: &RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    Encoder::new(&mut out, 92)
        .encode(
            img.as_raw(),
            img.width() as u16,
            img.height() as u16,
            ColorType::Rgb,
        )
        .expect("jpeg");
    out
}

fn decode(bytes: &[u8]) -> DynamicImage {
    image::load_from_memory_with_format(bytes, ImageFormat::Jpeg).expect("output is a JPEG")
}

/// Marker codes of the JPEG's segments up to and including the first SOS.
fn markers(jpeg: &[u8]) -> Vec<u8> {
    assert_eq!(&jpeg[..2], [0xFF, 0xD8], "SOI");
    let (mut pos, mut out) = (2, Vec::new());
    while pos + 4 <= jpeg.len() {
        assert_eq!(jpeg[pos], 0xFF, "marker at {pos}");
        let m = jpeg[pos + 1];
        out.push(m);
        if m == 0xDA {
            break;
        }
        pos += 2 + usize::from(u16::from_be_bytes([jpeg[pos + 2], jpeg[pos + 3]]));
    }
    out
}

/// What every normalised cover must be: a complete, progressive JPEG with
/// no comment or EXIF segment, of the given size.
fn assert_clean_jpeg(out: &[u8], w: u32, h: u32) -> DynamicImage {
    let m = markers(out);
    assert!(m.contains(&0xC2), "progressive (SOF2): {m:02x?}");
    assert!(!m.contains(&0xFE), "no COM segment: {m:02x?}");
    assert!(!m.contains(&0xE1), "no EXIF/XMP segment: {m:02x?}");
    assert_eq!(out[out.len() - 2..], [0xFF, 0xD9], "ends with EOI");
    let img = decode(out);
    assert_eq!((img.width(), img.height()), (w, h));
    img
}

fn near(got: [u8; 3], want: [u8; 3], tol: i32) -> bool {
    got.iter()
        .zip(want)
        .all(|(g, w)| (i32::from(*g) - i32::from(w)).abs() <= tol)
}

fn px(img: &DynamicImage, x: u32, y: u32) -> [u8; 3] {
    img.to_rgb8().get_pixel(x, y).0
}

fn assert_two_tone(img: &DynamicImage, what: &str) {
    let (w, h) = (img.width(), img.height());
    let (l, r) = (px(img, w / 4, h / 2), px(img, 3 * w / 4, h / 2));
    assert!(near(l, RED, 12), "{what}: left {l:?}, want {RED:?}");
    assert!(near(r, BLUE, 12), "{what}: right {r:?}, want {BLUE:?}");
}

// --- colour models and bit depths --------------------------------------------

#[test]
fn rgb_16_bit_png_keeps_its_colours() {
    let img: ImageBuffer<Rgb<u16>, Vec<u16>> = ImageBuffer::from_fn(32, 32, |x, _| {
        let c = if x < 16 { RED } else { BLUE };
        Rgb(c.map(|v| u16::from(v) * 257))
    });
    let png = encode(DynamicImage::ImageRgb16(img), ImageFormat::Png);
    let out = normalize_to_jpeg(&png, 95);
    assert_two_tone(&assert_clean_jpeg(&out, 32, 32), "16-bit RGB");
}

#[test]
fn rgba_16_bit_png_drops_alpha_and_keeps_colours() {
    let img: ImageBuffer<Rgba<u16>, Vec<u16>> = ImageBuffer::from_fn(32, 32, |x, _| {
        let c = if x < 16 { RED } else { BLUE };
        let [r, g, b] = c.map(|v| u16::from(v) * 257);
        Rgba([r, g, b, 40_000])
    });
    let png = encode(DynamicImage::ImageRgba16(img), ImageFormat::Png);
    let out = normalize_to_jpeg(&png, 95);
    assert_two_tone(&assert_clean_jpeg(&out, 32, 32), "16-bit RGBA");
}

#[test]
fn grey_alpha_png_becomes_a_colour_jpeg_of_the_same_grey() {
    let img = ImageBuffer::from_fn(16, 16, |_, _| image::LumaA([100u8, 128]));
    let png = encode(DynamicImage::ImageLumaA8(img), ImageFormat::Png);
    let out = normalize_to_jpeg(&png, 95);
    let jpeg = assert_clean_jpeg(&out, 16, 16);
    assert!(near(px(&jpeg, 8, 8), [100; 3], 4));
}

#[test]
fn greyscale_jpeg_stays_one_component() {
    let img = ImageBuffer::from_fn(24, 24, |x, _| Luma([(x * 10) as u8]));
    let mut src = Vec::new();
    Encoder::new(&mut src, 90)
        .encode(img.as_raw(), 24, 24, ColorType::Luma)
        .unwrap();
    let out = normalize_to_jpeg(&src, 95);
    let m = markers(&out);
    assert!(m.contains(&0xC2) && m.last() == Some(&0xDA), "{m:02x?}");
    // One component: the SOF segment's component count byte is 1.
    let sof = out.windows(2).position(|w| w == [0xFF, 0xC2]).unwrap();
    assert_eq!(out[sof + 9], 1, "components");
    let back = decode(&out);
    assert_eq!((back.width(), back.height()), (24, 24));
    assert!(near(px(&back, 12, 12), [120; 3], 6));
}

#[test]
fn transparent_gif_keeps_opaque_pixels_and_decodes_to_rgb() {
    // Left half blue and opaque, right half fully transparent. What colour
    // the transparent half takes is whatever sits in the GIF's palette for
    // the transparent index (as with Pillow's `convert("RGB")`); only the
    // opaque half, the size and the format are pinned.
    let img = RgbaImage::from_fn(32, 32, |x, _| {
        if x < 16 {
            Rgba([BLUE[0], BLUE[1], BLUE[2], 255])
        } else {
            Rgba([255, 0, 0, 0])
        }
    });
    let mut gif_bytes = Vec::new();
    {
        let mut enc = gif::GifEncoder::new(&mut gif_bytes);
        enc.encode_frame(image::Frame::new(img)).expect("gif");
    }
    let out = normalize_to_jpeg(&gif_bytes, 95);
    let jpeg = assert_clean_jpeg(&out, 32, 32);
    assert!(near(px(&jpeg, 4, 16), BLUE, 12), "{:?}", px(&jpeg, 4, 16));
}

#[test]
fn cmyk_jpeg_re_encodes_with_its_colours_not_inverted() {
    // CMYK: cyan 0, magenta 255, yellow 255 (red in subtractive terms), no
    // black. A re-encode that mishandles the inverted storage of 4-component
    // JPEGs turns this cyan.
    let (w, h) = (16u16, 16u16);
    let data: Vec<u8> = (0..w * h).flat_map(|_| [0u8, 255, 255, 0]).collect();
    let mut src = Vec::new();
    Encoder::new(&mut src, 95)
        .encode(&data, w, h, ColorType::Cmyk)
        .expect("cmyk jpeg");
    let out = normalize_to_jpeg(&src, 95);
    assert_ne!(
        out, src,
        "CMYK should re-encode, not fall back to the input"
    );
    let jpeg = assert_clean_jpeg(&out, 16, 16);
    let p = px(&jpeg, 8, 8);
    assert!(
        p[0] > 180 && p[1] < 80 && p[2] < 80,
        "expected red, got {p:?}"
    );
}

// --- metadata segments ------------------------------------------------------

/// `jpeg` with `segment` (marker + payload, length added here) spliced
/// directly after SOI.
fn with_segment(jpeg: &[u8], marker: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8, 0xFF, marker];
    out.extend(((payload.len() + 2) as u16).to_be_bytes());
    out.extend(payload);
    out.extend(&jpeg[2..]);
    out
}

#[test]
fn comment_segments_are_not_copied_to_the_output() {
    let src = with_segment(
        &baseline_jpeg(&two_tone(32, 32)),
        0xFE,
        b"made with a tool; private note",
    );
    assert!(markers(&src).contains(&0xFE));
    let out = normalize_to_jpeg(&src, 95);
    let jpeg = assert_clean_jpeg(&out, 32, 32);
    assert_two_tone(&jpeg, "comment JPEG");
    assert!(
        !out.windows(7).any(|w| w == b"private"),
        "comment text leaked into the output"
    );
}

/// An EXIF APP1 segment holding one IFD entry: Orientation = `orientation`.
fn exif_orientation(orientation: u16) -> Vec<u8> {
    let mut p = b"Exif\0\0MM\0\x2A\0\0\0\x08".to_vec();
    p.extend([0, 1]); // one entry
    p.extend([0x01, 0x12, 0, 3, 0, 0, 0, 1]); // tag 0x0112, SHORT, count 1
    p.extend(orientation.to_be_bytes());
    p.extend([0, 0]);
    p.extend([0, 0, 0, 0]); // no next IFD
    p
}

/// A 48x32 two-tone (red left, blue right) JPEG tagged with `orientation`.
fn oriented(orientation: u16) -> Vec<u8> {
    with_segment(
        &baseline_jpeg(&two_tone(48, 32)),
        0xE1,
        &exif_orientation(orientation),
    )
}

#[test]
fn exif_orientation_is_applied_to_the_pixels_and_the_tag_dropped() {
    // The output has no EXIF, so a rotation the source asked for has to be
    // in the pixels. Orientation 6 (rotate 90 degrees clockwise to display)
    // turns a 48x32 image into 32x48, and the left half (red) ends on top.
    let out = normalize_to_jpeg(&oriented(6), 95);
    let img = assert_clean_jpeg(&out, 32, 48);
    assert!(near(px(&img, 16, 12), RED, 12), "{:?}", px(&img, 16, 12));
    assert!(near(px(&img, 16, 36), BLUE, 12), "{:?}", px(&img, 16, 36));
}

#[test]
fn exif_orientations_that_keep_the_size_or_swap_it() {
    // 1: as stored; 3: 180 degrees (red ends on the right); 8: 90 degrees
    // counter-clockwise (red ends at the bottom); 2: mirrored (red right).
    let out = normalize_to_jpeg(&oriented(1), 95);
    assert_two_tone(&assert_clean_jpeg(&out, 48, 32), "orientation 1");
    for o in [2, 3] {
        let img = assert_clean_jpeg(&normalize_to_jpeg(&oriented(o), 95), 48, 32);
        assert!(near(px(&img, 12, 16), BLUE, 12), "orientation {o}");
        assert!(near(px(&img, 36, 16), RED, 12), "orientation {o}");
    }
    let img = assert_clean_jpeg(&normalize_to_jpeg(&oriented(8), 95), 32, 48);
    assert!(near(px(&img, 16, 12), BLUE, 12));
    assert!(near(px(&img, 16, 36), RED, 12));
}

#[test]
fn an_unreadable_or_out_of_range_orientation_leaves_the_pixels_alone() {
    for o in [0u16, 9, 255] {
        let out = normalize_to_jpeg(&oriented(o), 95);
        assert_two_tone(&assert_clean_jpeg(&out, 48, 32), "bad orientation");
    }
}

// --- damaged JPEGs ----------------------------------------------------------

#[test]
fn complete_scan_without_eoi_is_kept_as_it_came() {
    // The heuristic treats a JPEG with no EOI after its first SOS as
    // truncated and returns the input unchanged, however complete the scan.
    let full = baseline_jpeg(&two_tone(32, 32));
    assert_eq!(full[full.len() - 2..], [0xFF, 0xD9]);
    let no_eoi = &full[..full.len() - 2];
    assert_eq!(normalize_to_jpeg(no_eoi, 95), no_eoi);
}

#[test]
fn trailing_bytes_after_eoi_do_not_stop_the_re_encode() {
    let mut src = baseline_jpeg(&two_tone(32, 32));
    src.extend(b"trailing junk written by a careless tool");
    let out = normalize_to_jpeg(&src, 95);
    assert_two_tone(&assert_clean_jpeg(&out, 32, 32), "trailing junk");
}

#[test]
fn half_a_jpeg_is_kept_as_it_came() {
    let full = baseline_jpeg(&two_tone(64, 64));
    let half = &full[..full.len() / 2];
    assert_eq!(normalize_to_jpeg(half, 95), half);
}

#[test]
fn cut_scan_with_a_forged_eoi_re_encodes_without_panicking() {
    // Entropy data cut short, then FF D9 appended: the marker check passes
    // and the decoder pads the missing rows. Whatever it pads with, the
    // result is a well-formed JPEG of the original size.
    let full = baseline_jpeg(&two_tone(64, 64));
    let mut cut = full[..full.len() * 2 / 3].to_vec();
    cut.extend([0xFF, 0xD9]);
    let out = normalize_to_jpeg(&cut, 95);
    if out != cut {
        assert_clean_jpeg(&out, 64, 64);
    }
}

#[test]
fn non_image_data_is_kept_as_it_came() {
    for junk in [
        &b""[..],
        b"\x00",
        b"not an image at all",
        b"\xFF\xD8",
        b"\xFF\xD8\xFF\xD9",
        b"\x89PNG\r\n\x1a\n",
        b"GIF89a",
        &[0u8; 4096],
    ] {
        assert_eq!(normalize_to_jpeg(junk, 95), junk, "{junk:02x?}");
    }
}

#[test]
fn png_cut_in_the_pixel_data_is_kept_as_it_came() {
    let png = encode(DynamicImage::ImageRgb8(two_tone(64, 64)), ImageFormat::Png);
    let cut = &png[..png.len() - 40];
    assert_eq!(normalize_to_jpeg(cut, 95), cut);
}

// --- sizes ------------------------------------------------------------------

#[test]
fn one_pixel_images_re_encode() {
    for img in [
        DynamicImage::ImageRgb8(RgbImage::from_pixel(1, 1, Rgb(RED))),
        DynamicImage::ImageLuma8(ImageBuffer::from_pixel(1, 1, Luma([77u8]))),
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 0]))),
    ] {
        let out = normalize_to_jpeg(&encode(img, ImageFormat::Png), 95);
        assert_clean_jpeg(&out, 1, 1);
    }
}

#[test]
fn odd_sizes_that_do_not_fill_a_chroma_block_re_encode() {
    for (w, h) in [(3, 5), (17, 9), (33, 31), (1, 40), (40, 1)] {
        let out = normalize_to_jpeg(
            &encode(DynamicImage::ImageRgb8(two_tone(w, h)), ImageFormat::Png),
            95,
        );
        assert_clean_jpeg(&out, w, h);
    }
}

#[test]
fn extremely_wide_and_tall_images_up_to_the_jpeg_limit_re_encode() {
    // JPEG dimensions are 16-bit: 65535 is the largest edge.
    for (w, h) in [(65_535, 1), (1, 65_535)] {
        let png = encode(DynamicImage::ImageRgb8(two_tone(w, h)), ImageFormat::Png);
        let out = normalize_to_jpeg(&png, 95);
        assert_clean_jpeg(&out, w, h);
    }
}

#[test]
fn images_past_the_jpeg_limit_are_kept_as_they_came() {
    let png = encode(
        DynamicImage::ImageRgb8(two_tone(65_536, 1)),
        ImageFormat::Png,
    );
    assert_eq!(normalize_to_jpeg(&png, 95), png);
}

#[test]
fn a_large_image_re_encodes_to_its_own_size() {
    let png = encode(
        DynamicImage::ImageRgb8(two_tone(1500, 1000)),
        ImageFormat::Png,
    );
    assert_clean_jpeg(&normalize_to_jpeg(&png, 95), 1500, 1000);
}

#[test]
fn normalising_twice_is_stable_in_size_and_colour() {
    let once = normalize_to_jpeg(
        &encode(DynamicImage::ImageRgb8(two_tone(40, 30)), ImageFormat::Png),
        95,
    );
    let twice = normalize_to_jpeg(&once, 95);
    let jpeg = assert_clean_jpeg(&twice, 40, 30);
    assert_two_tone(&jpeg, "second pass");
}

// --- build_picture ----------------------------------------------------------

#[test]
fn build_picture_reads_size_and_depth_from_the_header() {
    let colour = build_picture(baseline_jpeg(&two_tone(30, 20)), "image/jpeg");
    assert_eq!(
        (colour.kind, colour.width, colour.height, colour.depth),
        (3, 30, 20, 24)
    );
    assert_eq!(colour.desc, "Front Cover");
    assert_eq!(colour.colors, 0);

    let grey = encode(
        DynamicImage::ImageLuma8(ImageBuffer::from_pixel(7, 9, Luma([1u8]))),
        ImageFormat::Png,
    );
    let grey = build_picture(grey, "image/png");
    assert_eq!((grey.width, grey.height, grey.depth), (7, 9, 8));
}

#[test]
fn build_picture_of_data_that_is_not_an_image_has_zero_geometry() {
    let p = build_picture(b"no image here".to_vec(), "image/jpeg");
    assert_eq!((p.width, p.height, p.depth), (0, 0, 0));
    assert_eq!(p.data, b"no image here");
}

// --- PICTURE blocks with a wrong declared size --------------------------------

/// Minimal FLAC (STREAMINFO, then a PICTURE block with `body`, last).
fn flac_with_picture_block(body: &[u8]) -> Vec<u8> {
    let mut si = vec![0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0];
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
    si.extend(packed.to_be_bytes());
    si.extend([0; 16]);
    let mut out = b"fLaC".to_vec();
    out.push(0);
    out.extend(&(si.len() as u32).to_be_bytes()[1..]);
    out.extend(&si);
    out.push(0x80 | 6);
    out.extend(&(body.len() as u32).to_be_bytes()[1..]);
    out.extend(body);
    out
}

/// A PICTURE block body whose data-length field says `declared` while
/// `data` is what follows it.
fn picture_body(declared: u32, data: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(3u32.to_be_bytes());
    b.extend(10u32.to_be_bytes());
    b.extend(b"image/jpeg");
    b.extend(0u32.to_be_bytes());
    for v in [8u32, 8, 24, 0] {
        b.extend(v.to_be_bytes());
    }
    b.extend(declared.to_be_bytes());
    b.extend(data);
    b
}

fn open(bytes: &[u8]) -> std::io::Result<FlacFile> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.flac");
    std::fs::write(&path, bytes).unwrap();
    FlacFile::open(&path)
}

#[test]
fn picture_declaring_more_data_than_the_block_holds_is_an_error() {
    let data = baseline_jpeg(&two_tone(8, 8));
    for declared in [data.len() as u32 + 1, data.len() as u32 + 1000, u32::MAX] {
        let body = picture_body(declared, &data);
        let err = open(&flac_with_picture_block(&body))
            .map(|_| ())
            .expect_err("error");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidData,
            "declared {declared}"
        );
    }
}

#[test]
fn picture_declaring_less_data_than_the_block_holds_keeps_the_declared_part() {
    let data = baseline_jpeg(&two_tone(8, 8));
    let body = picture_body(data.len() as u32 - 10, &data);
    let f = open(&flac_with_picture_block(&body)).expect("opens");
    let pics: Vec<&Picture> = f.pictures();
    assert_eq!(pics.len(), 1);
    assert_eq!(pics[0].data, data[..data.len() - 10]);
}

#[test]
fn picture_with_a_string_length_past_the_block_is_an_error() {
    let mut body = picture_body(0, b"");
    body[4..8].copy_from_slice(&u32::MAX.to_be_bytes()); // MIME length
    let err = open(&flac_with_picture_block(&body))
        .map(|_| ())
        .expect_err("error");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn picture_block_shorter_than_its_fixed_fields_is_an_error() {
    for len in [0usize, 3, 7, 20, 30] {
        let body = &picture_body(0, b"")[..len];
        assert!(open(&flac_with_picture_block(body)).is_err(), "{len} bytes");
    }
}

#[test]
fn picture_with_zero_declared_data_opens_with_no_image() {
    let f = open(&flac_with_picture_block(&picture_body(0, b""))).expect("opens");
    assert_eq!(f.pictures()[0].data, b"");
}
