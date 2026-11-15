//! Cover thumbnails against the Python app's.
//!
//! Golden: testdata/golden/thumbs/thumbs.json (the real-audio set's rows in
//! its private half), written by the Python generator, which runs the
//! Python app's own `thumbs.get_source_image` + `thumbs.make_thumbnail`
//! (Pillow 13). For the library albums the golden holds hashes and shapes
//! only; the Python app's thumbnails themselves are private test data,
//! `thumbs-golden/<library>/` (cover art is never committed), so the two
//! library tests are Tier 2. The generated test patterns carry their input
//! and the Python app's output.
//!
//! Bound: PSNR >= 45 dB of the decoded pixels (the cache format is kept,
//! the pixels need not be bit-identical), dimensions and mode equal, and our file no more than 25%
//! larger than the Python app's (earlier builds wrote 1.3-1.7x; now ~1.1x,
//! jpeg-encoder's progressive script being spectral selection only).
//! Measured at this commit: 52-72 dB (the decode and the resample are
//! bit-identical to Pillow's; the residual is jpeg-encoder's quantiser
//! rounding).

use std::path::Path;

use base64::Engine;
use image::DynamicImage;
use musaic_api::thumbs::make_thumbnail;
use serde_json::Value;
use sha2::{Digest, Sha256};

const PSNR_MIN_DB: f64 = 45.0;

fn golden() -> Value {
    musaic_testdata::golden("thumbs/thumbs.json")
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

fn decode(b: &[u8]) -> DynamicImage {
    image::load_from_memory(b).expect("decode jpeg")
}

fn mode(img: &DynamicImage) -> &'static str {
    match img {
        DynamicImage::ImageLuma8(_) => "L",
        DynamicImage::ImageRgb8(_) => "RGB",
        _ => "other",
    }
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let (a, b) = (decode(a), decode(b));
    assert_eq!((a.width(), a.height()), (b.width(), b.height()));
    let (a, b) = (a.to_rgb8().into_raw(), b.to_rgb8().into_raw());
    let mse: f64 = a
        .iter()
        .zip(&b)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
        .sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// Our thumbnail of `src` against the Python app's `expected` description and
/// bytes.
fn check(ctx: &str, src: &[u8], desc: &Value, python: &[u8]) -> f64 {
    let ours = make_thumbnail(src).unwrap_or_else(|| panic!("{ctx}: no thumbnail"));
    let img = decode(&ours);
    assert_eq!(
        (img.width() as u64, img.height() as u64, mode(&img)),
        (
            desc["width"].as_u64().unwrap(),
            desc["height"].as_u64().unwrap(),
            desc["mode"].as_str().unwrap()
        ),
        "{ctx}: shape"
    );
    assert_eq!(
        sha(python),
        desc["sha256"].as_str().unwrap(),
        "{ctx}: python thumb changed"
    );
    let db = psnr(&ours, python);
    eprintln!(
        "{ctx}: PSNR {db:.2} dB, sizes ours {} python {}",
        ours.len(),
        python.len()
    );
    assert!(db >= PSNR_MIN_DB, "{ctx}: PSNR {db:.2} dB < {PSNR_MIN_DB}");
    assert!(
        ours.len() as f64 <= python.len() as f64 * 1.25,
        "{ctx}: {} bytes vs the Python app's {}",
        ours.len(),
        python.len()
    );
    db
}

/// The album's source image as `thumbs.get_source_image` picked it.
fn source(album_dir: &Path, how: &str) -> Vec<u8> {
    match how.strip_prefix("embedded:") {
        Some(f) => {
            musaic_api::files::embedded_cover(&album_dir.join(f))
                .expect("embedded")
                .0
        }
        None => std::fs::read(album_dir.join(how)).expect("cover file"),
    }
}

fn run_library(name: &str, root: &Path) {
    let g = golden();
    let rows = g["libraries"][name]
        .as_object()
        .unwrap_or_else(|| panic!("golden has no {name} library"));
    assert!(!rows.is_empty());
    let mut worst = f64::INFINITY;
    for (dir, row) in rows {
        let ctx = format!("{name}/{dir}");
        let album_dir = root.join(dir);
        let src = source(&album_dir, row["source"].as_str().unwrap());
        assert_eq!(
            sha(&src),
            row["source_image"]["sha256"].as_str().unwrap(),
            "{ctx}: the fixture cover differs from the one the golden saw"
        );
        let python_path = musaic_testdata::private_required(format!("thumbs-golden/{name}"))
            .join(format!("{dir}.jpg"));
        let python = std::fs::read(&python_path).unwrap_or_else(|_| {
            panic!(
                "{}: missing; regenerate the thumbnails in the private test data",
                python_path.display()
            )
        });
        worst = worst.min(check(&ctx, &src, &row["thumb"], &python));
    }
    eprintln!(
        "{name}: worst PSNR {worst:.2} dB over {} albums",
        rows.len()
    );
}

/// Every synthetic-library cover (browse and cover upload differed by
/// 40-44 dB before this fix).
#[test]
fn synthetic_library_thumbs_match_python() {
    musaic_testdata::require_private!("thumbs-golden/synthetic");
    run_library("synthetic", &musaic_testdata::synthetic_library());
}

/// The real-audio set's covers (34-38 dB before this
/// fix). Needs the local copy of the real-audio set (private, gitignored) and the
/// generator's output for it.
#[test]
fn real_library_thumbs_match_python() {
    musaic_testdata::require_private!(
        "thumbs-golden/real",
        "real-library/library",
        "golden/thumbs/thumbs.json"
    );
    run_library(
        "real",
        &musaic_testdata::private_required("real-library/library"),
    );
}

/// The generated patterns — JPEG draft (DCT scaling,
/// approximated), reduce() before the resample, grayscale stays L, RGBA /
/// palette PNGs converted to RGB, an image that already fits (re-encoded,
/// same size), and a truncated JPEG (no thumbnail).
#[test]
fn pattern_thumbs_match_python() {
    let g = golden();
    let b64 = base64::engine::general_purpose::STANDARD;
    for (name, row) in g["patterns"].as_object().unwrap() {
        let input = b64.decode(row["input_b64"].as_str().unwrap()).unwrap();
        match row["thumb_b64"].as_str() {
            None => assert!(make_thumbnail(&input).is_none(), "{name}: python made none"),
            Some(t) => {
                let python = b64.decode(t).unwrap();
                check(name, &input, &row["thumb"], &python);
            }
        }
    }
}
