//! Differential tests against the Python app's `musaic.tags` functions.
//!
//! Golden: testdata/golden/tags/apply-scenarios.json, recorded from the Python
//! app. Each scenario copies one album of
//! testdata/synthetic-library/library into a temp dir, replays the same raw setup ops, runs
//! `apply_album_files` with the scenario's payload, and compares every event
//! and every file of the album directory with what the Python app produced.
//!
//! Comparison rules:
//! - files without a re-encoded cover must be BYTE-identical to the Python app's
//!   (stronger than required; it holds because the writer copies mutagen's
//!   block layout);
//! - a re-encoded JPEG is compared by dimensions/depth (exact) and decoded
//!   pixels: PSNR against the Python app's own JPEG must be >= PSNR_MIN_DB;
//! - everything else (tags in order with key casing, vendor, picture
//!   metadata, block order, ID3 prefix, STREAMINFO MD5, audio bytes, sidecar
//!   names) must be equal.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use base64::Engine;
use musaic_tags::{CoverInput, Event, FlacFile, apply_album_files};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Minimum PSNR (dB) between our JPEG and the Python app's, both decoded by the
/// same decoder. See the report: measured values for this golden are far
/// above it, while a real defect (wrong channel order, no chroma averaging,
/// transposed or shifted image, wrong quality tables) lands well below it.
const PSNR_MIN_DB: f64 = 45.0;

fn load(name: &str) -> Value {
    let p = musaic_testdata::public("golden/tags").join(name);
    serde_json::from_str(&fs::read_to_string(&p).expect("read golden")).expect("parse golden")
}

/// Recorded on the volume `work/tags-golden` sits on (APFS, case-insensitive).
fn golden() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| load("apply-scenarios.json"))
}

/// The subset recorded on a case-sensitive APFS volume.
fn golden_case_sensitive() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| load("apply-scenarios-case-sensitive.json"))
}

fn is_case_insensitive(dir: &Path) -> bool {
    fs::write(dir.join("CaseProbe"), b"").unwrap();
    let r = dir.join("caseprobe").exists();
    fs::remove_file(dir.join("CaseProbe")).unwrap();
    r
}

/// Scenarios of the case-insensitive golden whose recorded result depends on
/// the volume folding case (a `cover.JPG` / `Cover.jpg` beside `cover.jpg`,
/// found by the same-file check). Every other scenario gives the same result
/// on a case-sensitive volume, so it runs there too.
const CASE_FOLDING_SCENARIOS: &[&str] = &["sidecar-variants", "sidecar-uppercase-existing"];

/// A scratch dir on a volume with the golden's case sensitivity: the system
/// temp dir if it matches, else a dir under $MUSAIC_TAGS_ALT_VOLUME (e.g. a
/// case-sensitive APFS image mounted at work/cs-mnt). When neither matches,
/// a scenario whose result does not depend on case folding still runs in
/// the temp dir; one that does is skipped with a printed reason (`None`), so
/// a case-sensitive CI runner reports it instead of comparing against the
/// wrong expectation.
fn scratch_for(g: &Value, id: &str) -> Option<tempfile::TempDir> {
    let want = g["case_insensitive_fs"].as_bool().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    if is_case_insensitive(tmp.path()) == want {
        return Some(tmp);
    }
    let kind = |insensitive: bool| {
        if insensitive {
            "insensitive"
        } else {
            "sensitive"
        }
    };
    let Some(alt) = std::env::var_os("MUSAIC_TAGS_ALT_VOLUME") else {
        let folding = want && CASE_FOLDING_SCENARIOS.contains(&id);
        if want && !folding {
            return Some(tmp);
        }
        println!(
            "skipped: {} ({id}): temp dir is case-{}; scenario recorded on a case-{} volume \
             (set MUSAIC_TAGS_ALT_VOLUME to a case-{} directory to run it)",
            musaic_testdata::test_name(),
            kind(!want),
            kind(want),
            kind(want),
        );
        return None;
    };
    let tmp = tempfile::tempdir_in(alt).unwrap();
    assert_eq!(
        is_case_insensitive(tmp.path()),
        want,
        "MUSAIC_TAGS_ALT_VOLUME has the wrong case sensitivity"
    );
    Some(tmp)
}

fn b64(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .expect("base64")
}

fn blob(name: &str) -> Vec<u8> {
    let g = golden();
    if let Some(img) = g["images"].get(name) {
        return b64(img["b64"].as_str().unwrap());
    }
    b64(g["blobs"][name]
        .as_str()
        .unwrap_or_else(|| panic!("no blob {name}")))
}

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

// ---------- raw FLAC surgery: mirrors split_flac / join_flac in the generator ----------

type Blocks = Vec<(u8, Vec<u8>)>;

fn try_split_flac(data: &[u8]) -> Option<(Vec<u8>, Blocks, Vec<u8>)> {
    let mut pos = 0usize;
    if data.get(..3)? == b"ID3" {
        let size = data
            .get(6..10)?
            .iter()
            .fold(0usize, |a, b| (a << 7) | (*b as usize & 0x7f));
        pos = 10 + size;
    }
    if data.get(pos..pos + 4)? != b"fLaC" {
        return None;
    }
    let prefix = data[..pos].to_vec();
    pos += 4;
    let mut blocks = Vec::new();
    loop {
        let h = data.get(pos..pos + 4)?;
        let n = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
        blocks.push((h[0] & 0x7f, data.get(pos + 4..pos + 4 + n)?.to_vec()));
        pos += 4 + n;
        if h[0] & 0x80 != 0 {
            break;
        }
    }
    Some((prefix, blocks, data[pos..].to_vec()))
}

fn split_flac(data: &[u8]) -> (Vec<u8>, Blocks, Vec<u8>) {
    try_split_flac(data).expect("a FLAC stream")
}

fn join_flac(prefix: &[u8], blocks: &Blocks, audio: &[u8]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    out.extend_from_slice(b"fLaC");
    for (i, (code, body)) in blocks.iter().enumerate() {
        out.push(code | if i == blocks.len() - 1 { 0x80 } else { 0 });
        out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(body);
    }
    out.extend_from_slice(audio);
    out
}

fn is_flac_name(name: &str) -> bool {
    name.to_lowercase().ends_with(".flac") && !name.starts_with("._")
}

fn sorted_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn apply_setup(album: &Path, ops: &[Value]) {
    for op in ops {
        match op["op"].as_str().unwrap() {
            "write_file" => fs::write(
                album.join(op["name"].as_str().unwrap()),
                blob(op["blob"].as_str().unwrap()),
            )
            .unwrap(),
            "rename_file" => fs::rename(
                album.join(op["from"].as_str().unwrap()),
                album.join(op["to"].as_str().unwrap()),
            )
            .unwrap(),
            kind => {
                let file = op["file"].as_str().unwrap();
                let targets: Vec<PathBuf> = if file == "*" {
                    sorted_names(album)
                        .into_iter()
                        .filter(|n| is_flac_name(n))
                        .map(|n| album.join(n))
                        .collect()
                } else {
                    vec![album.join(file)]
                };
                for p in targets {
                    let (mut prefix, mut blocks, audio) = split_flac(&fs::read(&p).unwrap());
                    match kind {
                        "prepend" => {
                            assert!(prefix.is_empty());
                            prefix = blob(op["blob"].as_str().unwrap());
                        }
                        "strip_blocks" => {
                            blocks.retain(|(c, _)| u64::from(*c) != op["code"].as_u64().unwrap())
                        }
                        "insert_block" => {
                            let body = match op.get("zeros") {
                                Some(n) => vec![0u8; n.as_u64().unwrap() as usize],
                                None => blob(op["blob"].as_str().unwrap()),
                            };
                            blocks.insert(
                                op["index"].as_u64().unwrap() as usize,
                                (op["code"].as_u64().unwrap() as u8, body),
                            );
                        }
                        other => panic!("unknown setup op {other}"),
                    }
                    fs::write(&p, join_flac(&prefix, &blocks, &audio)).unwrap();
                }
            }
        }
    }
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        fs::copy(e.path(), dst.join(e.file_name())).unwrap();
    }
}

fn brief(dir: &Path) -> Vec<Value> {
    sorted_names(dir)
        .into_iter()
        .map(|n| {
            let b = fs::read(dir.join(&n)).unwrap();
            json!({"name": n, "size": b.len(), "sha256": sha(&b)})
        })
        .collect()
}

// ---------- events -> golden JSON shape ----------

/// `<ALBUM>/name` for a path inside the album; the golden spells it with `/`
/// on every platform.
fn rel(p: &Path, album: &Path) -> String {
    match p.strip_prefix(album) {
        Ok(rest) if rest.as_os_str().is_empty() => "<ALBUM>".into(),
        Ok(rest) => format!("<ALBUM>/{}", musaic_testdata::slash_path(rest)),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

fn pairs(v: &[(String, String)]) -> Value {
    Value::Array(v.iter().map(|(k, x)| json!([k, x])).collect())
}

fn event_json(e: &Event, album: &Path) -> Value {
    let body = match e {
        Event::CoverDownloaded {
            source_bytes,
            source_mime,
            bytes,
            mime,
        } => {
            json!({"source_bytes": source_bytes, "source_mime": source_mime, "bytes": bytes, "mime": mime})
        }
        Event::Start {
            album_path,
            dry_run,
            file_count,
            suggestions_applied,
            has_cover_change,
            track_change_count,
        } => json!({
            "album_path": rel(album_path, album), "dry_run": dry_run, "file_count": file_count,
            "suggestions_applied": pairs(suggestions_applied), "has_cover_change": has_cover_change,
            "track_change_count": track_change_count}),
        Event::FileStarted {
            index,
            filename,
            path,
        } => json!({"index": index, "filename": filename, "path": rel(path, album)}),
        Event::FileDone(c) => {
            let mut v = json!({
                "path": rel(&c.path, album), "filename": c.filename,
                "diffs": Value::Array(c.diffs.iter().map(|(k, d)| json!([k, {"old": d.old, "new": d.new}])).collect()),
                "art_changed": c.art_changed, "saved": c.saved,
                "error": c.error.as_ref().map(|_| "<ERROR>")});
            if let Some(r) = &c.renamed_to {
                v["renamed_to"] = json!(r);
            }
            v
        }
        Event::FileRenamed {
            old_path,
            new_path,
            old_filename,
            new_filename,
        } => json!({
            "old_path": rel(old_path, album), "new_path": rel(new_path, album),
            "old_filename": old_filename, "new_filename": new_filename}),
        Event::Done {
            album_path,
            dry_run,
            saved_count,
            art_changed_count,
            renamed_count,
            errors,
            suggestions_applied,
        } => json!({
            "album_path": rel(album_path, album), "dry_run": dry_run, "saved_count": saved_count,
            "art_changed_count": art_changed_count, "renamed_count": renamed_count, "errors": errors,
            "suggestions_applied": pairs(suggestions_applied)}),
        Event::CoverSaved { path, filename } => {
            json!({"path": rel(path, album), "filename": filename})
        }
    };
    json!([e.name(), body])
}

fn strip_error_text(mut v: Value) -> Value {
    if let Some(o) = v[1].as_object_mut() {
        o.remove("error_text");
    }
    v
}

// ---------- payload ----------

fn py_str(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()), // str(int)
        other => panic!("unsupported suggestion value {other}"),
    }
}

fn suggestions(v: &Value) -> musaic_tags::Suggestions {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|p| (p[0].as_str().unwrap().to_string(), py_str(&p[1])))
                .collect()
        })
        .unwrap_or_default()
}

// ---------- JPEG comparison ----------

/// PSNR of two images decoded by the same decoder; grayscale stays grayscale.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let a = image::load_from_memory(a).expect("decode ours");
    let b = image::load_from_memory(b).expect("decode the Python app's");
    assert_eq!(
        (a.width(), a.height()),
        (b.width(), b.height()),
        "dimensions"
    );
    let gray =
        |i: &image::DynamicImage| matches!(i.color(), image::ColorType::L8 | image::ColorType::L16);
    assert_eq!(gray(&a), gray(&b), "grayscale vs colour");
    let (pa, pb) = if gray(&a) {
        (a.into_luma8().into_raw(), b.into_luma8().into_raw())
    } else {
        (a.into_rgb8().into_raw(), b.into_rgb8().into_raw())
    };
    let mse = pa
        .iter()
        .zip(&pb)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
        .sum::<f64>()
        / pa.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// Vendor string the Python app (mutagen 1.48.1) gives a comment block it creates.
const PYTHON_NEW_BLOCK_VENDOR: &str = "Mutagen 1.48.1";

// ---------- the runner ----------

fn flac_record(path: &Path) -> Value {
    let data = fs::read(path).unwrap();
    let (prefix, blocks, audio) = split_flac(&data);
    let f = FlacFile::open(path).expect("our reader parses the result");
    // Independent cross-check of STREAMINFO through musaic-scan's reader.
    let meta = musaic_scan::flacmeta::read(path).expect("flacmeta parses the result");
    json!({
        "id3_prefix_len": prefix.len(),
        "blocks": blocks.iter().map(|(c, b)| json!([c, b.len()])).collect::<Vec<_>>(),
        "audio_offset": data.len() - audio.len(),
        "audio_sha256": sha(&audio),
        "streaminfo_md5": meta.info.md5.iter().map(|x| format!("{x:02x}")).collect::<String>(),
        "vendor": f.tags().map(|t| t.vendor.clone()),
        "tags": f.tags().map(|t| t.comments.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>()),
        "pictures": f.pictures().iter().map(|p| json!({
            "type": p.kind, "mime": p.mime, "desc": p.desc, "width": p.width, "height": p.height,
            "depth": p.depth, "colors": p.colors, "data_len": p.data.len(), "sha256": sha(&p.data)})).collect::<Vec<_>>(),
    })
}

fn run(id: &str) {
    run_in(golden(), id);
}

fn run_in(g: &Value, id: &str) {
    let sc = g["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no scenario {id}"));
    let Some(tmp) = scratch_for(g, id) else {
        return;
    };
    let album = tmp.path().join(id).join(sc["album"].as_str().unwrap());
    copy_dir(
        &musaic_testdata::synthetic_library().join(sc["album_src"].as_str().unwrap()),
        &album,
    );
    assert_eq!(
        Value::Array(brief(&album)),
        sc["before_fixture"],
        "{id}: fixture album differs from the one the golden saw"
    );
    apply_setup(
        &album,
        sc["setup"].as_array().map(Vec::as_slice).unwrap_or(&[]),
    );
    assert_eq!(
        Value::Array(brief(&album)),
        sc["after_setup"],
        "{id}: setup did not reproduce the Python app's input bytes"
    );

    // Audio frames of every FLAC before the apply, keyed by content so a
    // renamed file is still found.
    let audio_before: BTreeMap<String, (String, Vec<u8>)> = sorted_names(&album)
        .into_iter()
        .filter(|n| is_flac_name(n))
        .filter_map(|n| {
            let (_, _, a) = try_split_flac(&fs::read(album.join(&n)).ok()?)?;
            Some((n, (sha(&a), a)))
        })
        .collect();

    let cover_name = sc["cover"].as_str();
    let cover_bytes = cover_name.map(blob);
    let cover_mime = cover_name.map(|c| g["images"][c]["mime"].as_str().unwrap().to_string());
    let reencoded = cover_name.is_some_and(|c| g["cover_psnr"][c]["reencoded"].as_bool().unwrap());
    let sugg = suggestions(&sc["suggestions"]);
    let track: musaic_tags::TrackSuggestions = sc["track_suggestions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| (p[0].as_str().unwrap().to_string(), suggestions(&p[1])))
                .collect()
        })
        .unwrap_or_default();
    let dry_run = sc["dry_run"].as_bool().unwrap_or(false);

    for (round_no, round) in sc["rounds"].as_array().unwrap().iter().enumerate() {
        let ctx = format!("{id} round {round_no}");
        let mut events = Vec::new();
        let cover = cover_bytes.as_deref().map(|bytes| CoverInput {
            bytes,
            mime: cover_mime.as_deref(),
        });
        apply_album_files(&album, &sugg, &track, cover, dry_run, &mut |e| {
            events.push(e)
        })
        .unwrap();

        // Events, in order.
        let ours: Vec<Value> = events.iter().map(|e| event_json(e, &album)).collect();
        let theirs: Vec<Value> = round["events"]
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(strip_error_text)
            .collect();
        let mask = |mut v: Vec<Value>| {
            for e in &mut v {
                if e[0] == "cover_downloaded" && reencoded {
                    e[1]["bytes"] = json!("<JPEG LEN: compared by PSNR>");
                }
            }
            v
        };
        assert_eq!(mask(ours), mask(theirs), "{ctx}: events");

        // Directory listing: the same names.
        let names = sorted_names(&album);
        let golden_files = round["files"].as_array().unwrap();
        let golden_names: Vec<&str> = golden_files
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, golden_names, "{ctx}: directory listing");

        let python_jpeg = round["normalized_cover_sha256"]
            .as_str()
            .map(|s| b64(g["normalized_outputs"][s].as_str().unwrap()));
        let our_jpeg = cover_bytes
            .as_deref()
            .map(|b| musaic_tags::normalize_to_jpeg(b, 95));
        let reencoded_sha = python_jpeg.as_deref().filter(|_| reencoded).map(sha);

        for gf in golden_files {
            let name = gf["name"].as_str().unwrap();
            let path = album.join(name);
            let bytes = fs::read(&path).unwrap();
            let ours_is_cover_jpeg = reencoded_sha.is_some()
                && Some(gf["sha256"].as_str().unwrap()) == reencoded_sha.as_deref();
            match gf.get("flac") {
                Some(Value::Object(_)) => {
                    let mut ours = flac_record(&path);
                    let mut theirs = gf["flac"].clone();
                    // Audio frames: identical to the Python app's AND to our own input.
                    assert_eq!(
                        ours["audio_sha256"], theirs["audio_sha256"],
                        "{ctx}: {name}: audio frames"
                    );
                    let (_, _, audio_after) = split_flac(&bytes);
                    assert!(
                        audio_before.values().any(|(_, a)| *a == audio_after),
                        "{ctx}: {name}: audio byte range differs from every input file"
                    );
                    let pictures_reencoded = reencoded
                        && theirs["pictures"].as_array().unwrap().iter().any(|p| {
                            Some(p["sha256"].as_str().unwrap()) == reencoded_sha.as_deref()
                        });
                    // Deliberate change: a comment block we create
                    // carries our vendor string where the Python app's carries
                    // mutagen's. Everything else must still match.
                    let own_vendor = ours["vendor"] == json!(musaic_tags::flac::NEW_BLOCK_VENDOR)
                        && theirs["vendor"] == json!(PYTHON_NEW_BLOCK_VENDOR);
                    if own_vendor {
                        theirs["vendor"] = ours["vendor"].clone();
                        let delta = PYTHON_NEW_BLOCK_VENDOR.len() as i64
                            - musaic_tags::flac::NEW_BLOCK_VENDOR.len() as i64;
                        let len_of = |rec: &Value| {
                            rec["blocks"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|b| b[0] == 4)
                                .map(|b| b[1].as_i64().unwrap())
                        };
                        assert_eq!(
                            len_of(&ours).map(|n| n + delta),
                            len_of(&theirs),
                            "{ctx}: {name}: comment block differs by more than the vendor string"
                        );
                        for rec in [&mut ours, &mut theirs] {
                            for b in rec["blocks"].as_array_mut().unwrap() {
                                if b[0] == 4 || b[0] == 1 {
                                    b[1] = json!("<SIZE>");
                                }
                            }
                            rec["audio_offset"] = json!("<OFFSET>");
                        }
                    }
                    if pictures_reencoded {
                        let ours_jpeg = our_jpeg.as_deref().unwrap();
                        for p in ours["pictures"].as_array_mut().unwrap() {
                            if p["sha256"] == json!(sha(ours_jpeg)) {
                                p["sha256"] = json!("<REENCODED>");
                                p["data_len"] = json!("<REENCODED>");
                            }
                        }
                        for p in theirs["pictures"].as_array_mut().unwrap() {
                            if Some(p["sha256"].as_str().unwrap()) == reencoded_sha.as_deref() {
                                p["sha256"] = json!("<REENCODED>");
                                p["data_len"] = json!("<REENCODED>");
                            }
                        }
                        // Block order must match; PICTURE and PADDING lengths
                        // follow from the JPEG size, and audio_offset with them.
                        for rec in [&mut ours, &mut theirs] {
                            for b in rec["blocks"].as_array_mut().unwrap() {
                                if b[0] == 6 || b[0] == 1 {
                                    b[1] = json!("<SIZE>");
                                }
                            }
                            rec["audio_offset"] = json!("<OFFSET>");
                        }
                    } else if !own_vendor {
                        assert_eq!(
                            sha(&bytes),
                            gf["sha256"].as_str().unwrap(),
                            "{ctx}: {name}: not byte-identical to the Python app's file"
                        );
                    }
                    assert_eq!(ours, theirs, "{ctx}: {name}: parsed FLAC");
                }
                _ if ours_is_cover_jpeg => {
                    // The sidecar holds exactly the bytes we embedded.
                    assert_eq!(
                        Some(bytes.as_slice()),
                        our_jpeg.as_deref(),
                        "{ctx}: {name}: sidecar != normalized cover"
                    );
                }
                _ => assert_eq!(
                    sha(&bytes),
                    gf["sha256"].as_str().unwrap(),
                    "{ctx}: {name}: bytes"
                ),
            }
        }

        if let (true, Some(python), Some(ours)) =
            (reencoded, python_jpeg.as_deref(), our_jpeg.as_deref())
        {
            let db = psnr(ours, python);
            let src = cover_bytes.as_deref().unwrap();
            let ours_vs_src = psnr(ours, src);
            let python_vs_src = psnr(python, src);
            eprintln!(
                "PSNR {ctx} [{}]: ours vs Python app {db:.2} dB; vs source: ours {ours_vs_src:.2} dB, Python app {python_vs_src:.2} dB; sizes ours {} Python app {}",
                cover_name.unwrap(),
                ours.len(),
                python.len()
            );
            assert!(
                db >= PSNR_MIN_DB,
                "{ctx}: PSNR {db:.2} dB < {PSNR_MIN_DB} dB"
            );
            // Our encoder must not be meaningfully worse than Pillow's against the source.
            assert!(
                ours_vs_src >= python_vs_src - 1.0,
                "{ctx}: ours {ours_vs_src:.2} dB vs Python app {python_vs_src:.2} dB against the source"
            );
        }
    }
}

macro_rules! scenarios {
    ($($fn_name:ident => $id:literal, $what:literal;)*) => {
        $(
            #[doc = concat!("Covers ", $what, " (golden scenario `", $id, "`).")]
            #[test]
            fn $fn_name() {
                run($id);
            }
        )*

        #[test]
        fn every_golden_scenario_has_a_test() {
            let ours = [$($id),*];
            let golden: Vec<&str> = golden()["scenarios"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
            assert_eq!(golden, ours);
        }
    };
}

// Each test names what it covers next to it.
scenarios! {
    single_field => "single-field", "Vorbis comment writes";
    multi_field => "multi-field", "Vorbis comment writes";
    noop_same_values => "noop-same-values", "Vorbis comment writes (idempotency: no-op leaves files byte-identical)";
    case_only_change => "case-only-change", "Vorbis comment writes";
    noop_empty_payload => "noop-empty-payload", "Vorbis comment writes";
    clear_empty_and_none => "clear-empty-and-none", "Vorbis comment writes (empty values skipped)";
    whitespace_only_value => "whitespace-only-value", "Vorbis comment writes (malformed: whitespace-only written as empty)";
    genre_multi_value_first_match => "genre-multi-value-first-match", "Vorbis comment writes (multi-value)";
    genre_multi_value_replace => "genre-multi-value-replace", "Vorbis comment writes (multi-value)";
    non_ascii => "non-ascii", "Vorbis comment writes";
    per_track => "per-track", "Vorbis comment writes (per-track fields + rename plan)";
    per_track_swap_renames => "per-track-swap-renames", "Vorbis comment writes (two-pass rename)";
    per_track_no_prefix => "per-track-no-prefix", "Vorbis comment writes";
    per_track_prefix_forms => "per-track-prefix-forms", "track-number filename prefix ('1-05 - ' / '1.06_' prefixes, numbers >= 100, non-integer)";
    per_track_int_forms => "per-track-int-forms", "track-number filename prefix (malformed: Python int() forms '5/12', '1_0', '+6', Arabic-Indic digit)";
    dry_run => "dry-run", "Vorbis comment writes, cover embed, cover.jpg sidecar (dry run writes nothing)";
    idempotent_twice => "idempotent-twice", "Vorbis comment writes (idempotency)";
    cover_png_on_no_cover => "cover-png-on-no-cover", "cover embed, cover.jpg sidecar";
    cover_jpeg_replace_embedded => "cover-jpeg-replace-embedded", "Vorbis comment writes, cover embed, cover.jpg sidecar";
    cover_both_existing => "cover-both-existing", "cover embed, cover.jpg sidecar";
    cover_noise_png => "cover-noise-png", "cover embed";
    cover_rgba_png => "cover-rgba-png", "cover embed (alpha dropped), cover.jpg sidecar (folder.jpg kept)";
    cover_palette_png => "cover-palette-png", "cover embed, cover.jpg sidecar (AppleDouble junk album)";
    cover_gray_png => "cover-gray-png", "cover embed";
    cover_gray_jpeg => "cover-gray-jpeg", "cover embed";
    cover_gif => "cover-gif", "cover embed";
    cover_webp => "cover-webp", "cover embed";
    cover_gray16_png_unsavable => "cover-gray16-png-unsavable", "cover embed (malformed: original bytes kept)";
    cover_truncated_jpeg => "cover-truncated-jpeg", "cover embed (malformed: original bytes kept)";
    cover_garbage => "cover-garbage", "cover embed (malformed: original bytes kept)";
    multi_picture_replace => "multi-picture-replace", "cover embed (all pictures replaced)";
    multi_picture_tags_only => "multi-picture-tags-only", "Vorbis comment writes (pictures untouched)";
    id3_prefix => "id3-prefix", "Vorbis comment writes";
    id3_prefix_cover => "id3-prefix-cover", "cover embed";
    no_vorbis_comment => "no-vorbis-comment", "Vorbis comment writes";
    no_padding => "no-padding", "Vorbis comment writes";
    no_padding_noop => "no-padding-noop", "Vorbis comment writes (idempotency)";
    padding_overflow_shrinks => "padding-overflow-shrinks", "Vorbis comment writes";
    malformed_vorbis_comment => "malformed-vorbis-comment", "Vorbis comment writes (malformed)";
    broken_flac_in_album => "broken-flac-in-album", "Vorbis comment writes (malformed)";
    sidecar_variants => "sidecar-variants", "cover.jpg sidecar (case-insensitive volume)";
    sidecar_uppercase_existing => "sidecar-uppercase-existing", "cover.jpg sidecar (same-file check on a case-insensitive volume)";
}

// On a case-sensitive volume: `cover.JPG` is a different
// file there and is deleted; `Cover.Png` is not in the Python app's list and stays.
// These run where the system temp dir is case-sensitive (Linux); elsewhere
// they print a skip line unless MUSAIC_TAGS_ALT_VOLUME names a case-sensitive
// directory (e.g. a case-sensitive APFS disk image).
#[test]
fn case_sensitive_cover_png_on_no_cover() {
    run_in(golden_case_sensitive(), "cover-png-on-no-cover");
}

#[test]
fn case_sensitive_sidecar_variants() {
    run_in(golden_case_sensitive(), "sidecar-variants");
}

#[test]
fn case_sensitive_sidecar_uppercase_existing() {
    run_in(golden_case_sensitive(), "sidecar-uppercase-existing");
}

/// Crash-safety of the writer: a write that fits the old metadata
/// region is done in place, like mutagen (same inode); one that changes the
/// region's size goes through a fsynced temp file + rename (new inode, same
/// permissions, no temp left behind) where mutagen shifts the audio inside
/// the file. Either way the bytes equal the Python app's (golden scenarios
/// `single-field` and `no-padding`).
#[cfg(unix)]
#[test]
fn save_is_in_place_when_it_fits_and_replaces_otherwise() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let g = golden();
    for (id, expect_in_place) in [("single-field", true), ("no-padding", false)] {
        let sc = g["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap();
        let tmp = scratch_for(g, id).expect("not a case-folding scenario");
        let album = tmp.path().join(sc["album"].as_str().unwrap());
        copy_dir(
            &musaic_testdata::synthetic_library().join(sc["album_src"].as_str().unwrap()),
            &album,
        );
        apply_setup(
            &album,
            sc["setup"].as_array().map(Vec::as_slice).unwrap_or(&[]),
        );
        let names: Vec<String> = sorted_names(&album)
            .into_iter()
            .filter(|n| is_flac_name(n))
            .collect();
        for n in &names {
            fs::set_permissions(album.join(n), fs::Permissions::from_mode(0o640)).unwrap();
        }
        let inodes: Vec<u64> = names
            .iter()
            .map(|n| fs::metadata(album.join(n)).unwrap().ino())
            .collect();
        let mut modes = Vec::new();
        for n in &names {
            let mut f = FlacFile::open(&album.join(n)).unwrap();
            for p in sc["suggestions"].as_array().unwrap() {
                f.tags_mut().set(
                    match p[0].as_str().unwrap() {
                        "genre" => "genre",
                        other => panic!("{other}"),
                    },
                    p[1].as_str().unwrap(),
                );
            }
            modes.push(f.save().unwrap());
        }
        let golden_files = sc["rounds"][0]["files"].as_array().unwrap();
        for (n, ino) in names.iter().zip(&inodes) {
            let meta = fs::metadata(album.join(n)).unwrap();
            assert_eq!(meta.ino() == *ino, expect_in_place, "{id}: {n}: inode");
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o640,
                "{id}: {n}: permissions"
            );
            let gf = golden_files
                .iter()
                .find(|f| f["name"] == n.as_str())
                .unwrap();
            assert_eq!(
                sha(&fs::read(album.join(n)).unwrap()),
                gf["sha256"].as_str().unwrap(),
                "{id}: {n}"
            );
        }
        let want = if expect_in_place {
            musaic_tags::SaveMode::InPlace
        } else {
            musaic_tags::SaveMode::Replaced
        };
        assert!(modes.iter().all(|m| *m == want), "{id}: {modes:?}");
        assert!(
            !sorted_names(&album).iter().any(|n| n.ends_with(".tmp")),
            "{id}: temp file left behind"
        );
    }
}

/// A save first opens the
/// file for writing, so a read-only FLAC is refused whether or not the new
/// metadata fits; the refusal is the same error (kind and OS error) in both
/// cases, the Python app's (mutagen's `open(path, "rb+")`; services golden
/// `one-file-fails` / `one-file-fails-grows`). Directory permissions, for
/// the record: a writable file in a read-only directory is saved in place
/// when the metadata fits (bytes equal the Python app's), and refused untouched
/// when it does not (the temp file the writer needs cannot be created; mutagen
/// would have shifted the audio in place and succeeded).
#[cfg(unix)]
#[test]
fn read_only_file_and_directory_combinations() {
    use std::io;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let g = golden();
    let prepare = |id: &str| {
        let sc = g["scenarios"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == id)
            .unwrap()
            .clone();
        let tmp = scratch_for(g, id).expect("not a case-folding scenario");
        let album = tmp.path().join(sc["album"].as_str().unwrap());
        copy_dir(
            &musaic_testdata::synthetic_library().join(sc["album_src"].as_str().unwrap()),
            &album,
        );
        apply_setup(
            &album,
            sc["setup"].as_array().map(Vec::as_slice).unwrap_or(&[]),
        );
        let name = sorted_names(&album)
            .into_iter()
            .find(|n| is_flac_name(n))
            .unwrap();
        (tmp, sc, album.clone(), album.join(name))
    };
    let set_mode =
        |p: &Path, m: u32| fs::set_permissions(p, fs::Permissions::from_mode(m)).unwrap();
    // (scenario, file read-only, dir read-only) -> expected outcome.
    let mut errors: Vec<(String, io::ErrorKind, Option<i32>)> = Vec::new();
    for (id, file_ro, dir_ro) in [
        ("single-field", true, false),
        ("no-padding", true, false),
        ("single-field", true, true),
        ("no-padding", true, true),
        ("single-field", false, true),
        ("no-padding", false, true),
    ] {
        let label = format!("{id} file_ro={file_ro} dir_ro={dir_ro}");
        let (_tmp, sc, album, path) = prepare(id);
        let before = fs::read(&path).unwrap();
        let ino = fs::metadata(&path).unwrap().ino();
        let mut f = FlacFile::open(&path).unwrap();
        for p in sc["suggestions"].as_array().unwrap() {
            assert_eq!(p[0], "genre");
            f.tags_mut().set("genre", p[1].as_str().unwrap());
        }
        if file_ro {
            set_mode(&path, 0o444);
        }
        if dir_ro {
            set_mode(&album, 0o555);
        }
        let res = f.save();
        set_mode(&album, 0o755);
        set_mode(&path, 0o644);
        let fits = id == "single-field";
        match res {
            Ok(mode) => {
                assert!(!file_ro && fits, "{label}: saved");
                assert_eq!(mode, musaic_tags::SaveMode::InPlace, "{label}");
                let gf = sc["rounds"][0]["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|x| path.file_name().unwrap().to_str() == x["name"].as_str())
                    .unwrap();
                assert_eq!(
                    sha(&fs::read(&path).unwrap()),
                    gf["sha256"].as_str().unwrap(),
                    "{label}: bytes"
                );
            }
            Err(e) => {
                assert!(file_ro || !fits, "{label}: refused: {e}");
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied, "{label}: {e}");
                assert_eq!(fs::read(&path).unwrap(), before, "{label}: untouched");
                assert_eq!(fs::metadata(&path).unwrap().ino(), ino, "{label}: inode");
                errors.push((label.clone(), e.kind(), e.raw_os_error()));
            }
        }
        assert!(
            !sorted_names(&album).iter().any(|n| n.ends_with(".tmp")),
            "{label}: temp file left behind"
        );
    }
    // A read-only file gives one and the same error whether it fits or not.
    let ro: Vec<_> = errors
        .iter()
        .filter(|(l, ..)| l.contains("file_ro=true"))
        .map(|(_, k, os)| (*k, *os))
        .collect();
    assert_eq!(ro.len(), 4);
    assert!(ro.iter().all(|x| *x == ro[0]), "{ro:?}");
}
