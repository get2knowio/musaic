//! Cover normalisation and the PICTURE block built from it.
//!
//! Port of `tags.normalize_to_jpeg` and `tags._build_picture`. The Python app
//! re-encodes with Pillow (`quality=95, optimize=True, progressive=True`,
//! default 4:2:0 chroma subsampling, no ICC/EXIF carried over); here the
//! image is decoded with the `image` crate and encoded with `jpeg-encoder` at
//! the same settings. The bytes differ from Pillow's (parsed equality is enough);
//! dimensions and colour depth match and pixels are compared by PSNR.

use std::io::Cursor;

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use jpeg_encoder::{ChromaSubsamplingMethod, ColorType as JpegColor, Encoder, SamplingFactor};

use crate::flac::Picture;

/// Header-level facts Pillow's lazy `Image.open` would know without decoding.
struct Header {
    width: u32,
    height: u32,
    /// `len(img.getbands())` for the Pillow mode this colour type opens as.
    bands: u32,
    format: ImageFormat,
}

fn open_header(bytes: &[u8]) -> Option<(Header, impl ImageDecoder)> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let format = reader.format()?;
    let decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    let bands = u32::from(decoder.color_type().channel_count());
    Some((
        Header {
            width,
            height,
            bands,
            format,
        },
        decoder,
    ))
}

/// Check a remotely proposed image before it can enter a review. Limit decoded
/// size before attempting the full decode, even for a tiny compressed input.
pub fn verify_image(bytes: &[u8]) -> Option<(u32, u32)> {
    let image = decode_review_image(bytes)?;
    Some((image.width(), image.height()))
}

/// Decode the actual proposed cover with the same resource limits at preview and apply.
pub fn review_thumbnail(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let image = decode_review_image(bytes)?;
    let mut jpeg = Cursor::new(Vec::new());
    image
        .thumbnail(256, 256)
        .to_rgb8()
        .write_to(&mut jpeg, ImageFormat::Jpeg)
        .ok()?;
    Some((image.width(), image.height(), jpeg.into_inner()))
}

fn decode_review_image(bytes: &[u8]) -> Option<DynamicImage> {
    let (header, mut decoder) = open_header(bytes)?;
    let (w, h) = (header.width, header.height);
    if w == 0 || h == 0 || w > 8192 || h > 8192 || u64::from(w) * u64::from(h) > 16_000_000 {
        return None;
    }
    if header.format == ImageFormat::Jpeg && !jpeg_reaches_eoi(bytes) {
        return None;
    }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128_000_000);
    decoder.set_limits(limits).ok()?;
    DynamicImage::from_decoder(decoder).ok()
}

/// Pixels Pillow would hand to its JPEG encoder, or `None` where Pillow's
/// `save(format="JPEG")` raises (16-bit grayscale opens as mode `I;16`).
fn pixels_for_jpeg(img: DynamicImage) -> Option<(Vec<u8>, u32, u32, JpegColor)> {
    let (w, h) = (img.width(), img.height());
    match img {
        // L stays L (grayscale JPEG).
        DynamicImage::ImageLuma8(b) => Some((b.into_raw(), w, h, JpegColor::Luma)),
        // I;16 cannot be written as JPEG.
        DynamicImage::ImageLuma16(_) => None,
        // RGB is saved as is.
        DynamicImage::ImageRgb8(b) => Some((b.into_raw(), w, h, JpegColor::Rgb)),
        // RGBA / LA / P are `convert("RGB")`ed: alpha dropped, palette expanded.
        DynamicImage::ImageRgba8(_) | DynamicImage::ImageLumaA8(_) => {
            Some((img.into_rgb8().into_raw(), w, h, JpegColor::Rgb))
        }
        // 16-bit RGB(A) / LA: assumed to follow Pillow's high-byte unpacking
        // (`RGB;16B` etc.). Not covered by the golden.
        DynamicImage::ImageRgb16(b) => Some((
            b.into_raw().iter().map(|v| (v >> 8) as u8).collect(),
            w,
            h,
            JpegColor::Rgb,
        )),
        DynamicImage::ImageRgba16(b) => {
            let rgb = b
                .into_raw()
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .map(|v| (v >> 8) as u8)
                .collect();
            Some((rgb, w, h, JpegColor::Rgb))
        }
        DynamicImage::ImageLumaA16(b) => {
            let rgb = b
                .into_raw()
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|p| [p[0]; 3])
                .map(|v| (v >> 8) as u8)
                .collect();
            Some((rgb, w, h, JpegColor::Rgb))
        }
        other => Some((other.into_rgb8().into_raw(), w, h, JpegColor::Rgb)),
    }
}

/// `normalize_to_jpeg(image_bytes, quality=quality)`: re-encode any decodable
/// image as progressive, Huffman-optimised JPEG; on any failure return the
/// input bytes unchanged (as the Python app does).
///
/// The failure is logged as the Python app logs it (tags.py:425,
/// `logger.warning("cover JPEG normalization failed: %s — keeping original
/// bytes", e)` on `musaic.tags`); the reason text is this build's, not
/// Pillow's exception message.
pub fn normalize_to_jpeg(image_bytes: &[u8], quality: u8) -> Vec<u8> {
    match encode(image_bytes, quality) {
        Ok(out) => out,
        Err(why) => {
            musaic_log::warning(
                "musaic.tags",
                &format!("cover JPEG normalization failed: {why} \u{2014} keeping original bytes"),
            );
            image_bytes.to_vec()
        }
    }
}

fn encode(image_bytes: &[u8], quality: u8) -> Result<Vec<u8>, String> {
    let (header, mut decoder) =
        open_header(image_bytes).ok_or_else(|| "cannot identify image file".to_owned())?;
    // Pillow raises "image file is truncated" when the data ends before the
    // decoder is done; zune-jpeg instead pads the image with grey. Entropy-
    // coded data cannot contain FF D9, so a stream that reached its EOI
    // marker has one after its first SOS (FF DA).
    if header.format == ImageFormat::Jpeg && !jpeg_reaches_eoi(image_bytes) {
        return Err("image file is truncated".to_owned());
    }
    // The output carries no EXIF, so an orientation the source asked a viewer
    // to apply is applied to the pixels here; otherwise the cover would show
    // sideways once the tag is gone. (The earlier implementation ignored it.)
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let (data, w, h, color) =
        pixels_for_jpeg(img).ok_or_else(|| "cannot write mode I;16 as JPEG".to_owned())?;
    let too_large = |_| format!("image too large for JPEG ({w}x{h})");
    let (w, h) = (
        u16::try_from(w).map_err(too_large)?,
        u16::try_from(h).map_err(too_large)?,
    );
    let mut out = Vec::new();
    let mut enc = Encoder::new(&mut out, quality);
    enc.set_progressive(true);
    enc.set_optimized_huffman_tables(true);
    // libjpeg's default for colour JPEGs (Pillow subsampling=-1) is 4:2:0
    // with box-averaged chroma (h2v2_downsample).
    enc.set_sampling_factor(SamplingFactor::F_2_2);
    enc.set_chroma_subsampling_method(ChromaSubsamplingMethod::Average);
    enc.encode(&data, w, h, color).map_err(|e| e.to_string())?;
    Ok(out)
}

/// `_build_picture(data, mime)`: a type-3 "Front Cover" picture whose width,
/// height and depth come from the image header when it can be read (else 0).
pub fn build_picture(data: Vec<u8>, mime: &str) -> Picture {
    let (width, height, depth) = match open_header(&data) {
        Some((h, _)) => (h.width, h.height, 8 * h.bands),
        None => (0, 0, 0),
    };
    Picture {
        kind: 3,
        mime: mime.to_string(),
        desc: "Front Cover".to_string(),
        width,
        height,
        depth,
        colors: 0,
        data,
    }
}

fn jpeg_reaches_eoi(b: &[u8]) -> bool {
    let Some(sos) = b.windows(2).position(|w| w == [0xFF, 0xDA]) else {
        return false;
    };
    b[sos..].windows(2).any(|w| w == [0xFF, 0xD9])
}
