//! Log lines the Python app writes on failure paths that musaic-services ports
//! (the lower crates log through `musaic-log`):
//! `musaic.fingerprint` (fingerprint.py:191, 213), `musaic.services.apply`
//! (services/apply.py:141, 257), `musaic.thumbs` (thumbs.py:69, 98, 142),
//! `musaic.track_tags_sidecar` (track_tags_sidecar.py:64, 71, 74) and
//! `musaic.cache` (cache.py:1534). Each test provokes the failure locally
//! (fakes, temp dirs, SQLite TEMP triggers; no network) and checks level,
//! target and message: the Python app's format string filled with this build's
//! values.
//!
//! Deliberate differences, said once: where the Python app calls
//! `log.exception(msg)` (ERROR plus a Python traceback) this build logs
//! ERROR with `msg: <error text>` (the convention of musaic-fsops,
//! musaic-resolve and musaic-tags `tests/logging_seams.rs`); where the
//! Python app interpolates an exception (`%s` of `e`) the text is this build's
//! (io::Error, serde_json, SQLite, `NetError`, the thumbnailer's reason), so
//! only the fixed part is asserted.

// Unix permission bits make the failures these tests provoke; Windows builds this file out.
#![cfg(unix)]

use std::ops::ControlFlow;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use musaic_db::Db;
use musaic_events::Bus;
use musaic_log::{Level, Record, capture};
use musaic_net::{AcoustIdClient, Request, Response, Transport, TransportError};
use musaic_resolve::LookupResult;
use musaic_resolve::source::NetError;
use musaic_scan::{TrackOrder, py_resolve, scan_library};
use musaic_services::Json;
use musaic_services::apply::{ApplyEnv, ApplyRequest, NoCovers, apply_album_changes};
use musaic_services::fingerprint::{
    AcoustIdLookup, FingerprintEnv, Fingerprinter, fingerprint_album,
};

const NOW: &str = "2026-01-01T00:00:00Z";

/// Minimal FLAC (STREAMINFO + VORBIS_COMMENT + 1 KiB PADDING, no audio
/// frames), the recipe of musaic-tags' logging_seams.
fn flac_bytes(tags: &[(&str, &str)]) -> Vec<u8> {
    let mut si = Vec::new();
    si.extend_from_slice(&[0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 0]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let mut vc = Vec::new();
    vc.extend_from_slice(&4u32.to_le_bytes());
    vc.extend_from_slice(b"test");
    vc.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        let e = format!("{k}={v}");
        vc.extend_from_slice(&(e.len() as u32).to_le_bytes());
        vc.extend_from_slice(e.as_bytes());
    }
    let mut out = b"fLaC".to_vec();
    out.push(0);
    out.extend_from_slice(&(si.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&si);
    out.push(0x04);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    out.push(0x81);
    out.extend_from_slice(&1024u32.to_be_bytes()[1..]);
    out.extend_from_slice(&[0; 1024]);
    out
}

struct Lib {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    db: Db,
    lib_s: String,
    id: String,
    album: PathBuf,
}

/// One album `<tmp>/lib/Al` of `(filename, tracknumber)` FLACs, scanned
/// into a copy of the fixture v16 database.
fn library(files: &[(&str, &str)]) -> Lib {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let lib = base.join("lib");
    let album = lib.join("Al");
    std::fs::create_dir_all(&album).unwrap();
    for (name, n) in files {
        std::fs::write(
            album.join(name),
            flac_bytes(&[("TRACKNUMBER", n), ("TITLE", "A"), ("ALBUM", "X")]),
        )
        .unwrap();
    }
    let db_path = base.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db_path).unwrap();
    let (mut db, _) = Db::open(&db_path).unwrap();
    let lib_s = py_resolve(&lib).to_string_lossy().into_owned();
    let scanned = scan_library(&lib, TrackOrder::Filename);
    musaic_db::albums::save_snapshot(db.connection_mut(), &lib_s, NOW, NOW, &scanned).unwrap();
    let id = scanned[0].id.clone();
    Lib {
        _tmp: tmp,
        base,
        db,
        lib_s,
        id,
        album,
    }
}

/// The one line starting with `prefix`, at `level` on `target`.
fn has(lines: &[Record], level: Level, target: &str, prefix: &str) -> Record {
    let hits: Vec<&Record> = lines
        .iter()
        .filter(|r| r.message.starts_with(prefix))
        .collect();
    assert_eq!(hits.len(), 1, "{prefix}: {lines:?}");
    assert_eq!((hits[0].level, hits[0].target.as_str()), (level, target));
    hits[0].clone()
}

// ---------- musaic.fingerprint ----------

const FP: &str = "musaic.fingerprint";

/// Fingerprints every file except those whose name contains "bad".
struct FakePrints;

impl Fingerprinter for FakePrints {
    fn fingerprint(&self, path: &Path) -> Result<(String, u64), String> {
        if path.to_string_lossy().contains("bad") {
            Err("not audio (test)".into())
        } else {
            Ok(("AQAAfake".into(), 1))
        }
    }
    fn implementation(&self) -> &str {
        musaic_db::FINGERPRINT_IMPL_RUST
    }
}

struct FailingLookup;

impl AcoustIdLookup for FailingLookup {
    fn lookup(&self, _: &str, _: u64) -> Result<Vec<LookupResult>, NetError> {
        Err(NetError::Network("timed out (test)".into()))
    }
}

// Python app: musaic/fingerprint.py:191
// `log.warning("Chromaprint failed for %s: %s", flac_path.name, e)`.
#[test]
fn chromaprint_failure_is_logged() {
    let l = library(&[("01 a.flac", "1"), ("02 bad.flac", "2")]);
    let bus = Bus::new();
    let env = FingerprintEnv {
        conn: l.db.connection(),
        fingerprinter: &FakePrints,
        acoustid: None,
        bus: &bus,
        now_iso: NOW,
        cancel: None,
    };
    let album = l.album.to_string_lossy().into_owned();
    let (out, lines) = capture(|| fingerprint_album(&env, &l.id, &album, false));
    assert_eq!(out.unwrap().tracks.len(), 1);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let r = has(&lines, Level::Warning, FP, "Chromaprint failed for ");
    assert_eq!(
        r.message,
        "Chromaprint failed for 02 bad.flac: not audio (test)"
    );
}

// Python app: musaic/fingerprint.py:213
// `log.warning("AcoustID lookup failed for %s: %s", flac_path.name, e)`.
// Logged for the lookup after fingerprinting, and again
// for the retry of the stored fingerprint (the Python app had no such retry).
// Deliberate change: the Python app's per-track WARNING is now DEBUG, and each
// album gets one WARNING summary (offline, every track fails the same way;
// one line per track filled the log).
#[test]
fn acoustid_lookup_failure_is_logged() {
    let l = library(&[("01 a.flac", "1"), ("02 b.flac", "2")]);
    let bus = Bus::new();
    let env = FingerprintEnv {
        conn: l.db.connection(),
        fingerprinter: &FakePrints,
        acoustid: Some(&FailingLookup),
        bus: &bus,
        now_iso: NOW,
        cancel: None,
    };
    let album = l.album.to_string_lossy().into_owned();
    let folder = l.album.file_name().unwrap().to_string_lossy().into_owned();
    let summary = format!(
        "AcoustID lookup failed for 2 of 2 track(s) in {folder}: network error: timed out (test)"
    );
    for pass in ["computed", "stored (lookup retried)"] {
        let (out, lines) = capture(|| fingerprint_album(&env, &l.id, &album, false));
        assert_eq!(out.unwrap().tracks.len(), 2, "{pass}");
        assert_eq!(lines.len(), 3, "{pass}: {lines:?}");
        let warnings: Vec<&Record> = lines.iter().filter(|r| r.level == Level::Warning).collect();
        assert_eq!(
            warnings.len(),
            1,
            "{pass}: one summary per album: {lines:?}"
        );
        assert_eq!(warnings[0].target, FP);
        assert_eq!(warnings[0].message, summary, "{pass}");
        for (name, r) in ["01 a.flac", "02 b.flac"].iter().zip(&lines[..2]) {
            assert_eq!(r.level, Level::Debug, "{pass}");
            assert_eq!(
                r.message,
                format!("AcoustID lookup failed for {name}: network error: timed out (test)"),
                "{pass}"
            );
        }
    }
}

const KEY: &str = "sEcReTkEy123";

/// Answers every request with `answer`, or fails it.
struct Canned(Option<Response>);

impl Transport for Canned {
    fn send(&self, r: &Request) -> Result<Response, TransportError> {
        self.0.clone().ok_or_else(|| TransportError::Unobserved {
            key: String::new(),
            method: "POST",
            url: r.redacted_url(),
        })
    }
}

/// The line goes through the real AcoustID client, whose
/// error texts redact the key; an answer that echoes the key leaves no
/// trace of it in the log line, for every failure class.
#[test]
fn acoustid_failure_lines_carry_no_key() {
    let echo = format!(
        r#"{{"status": "error", "error": {{"code": 4, "message": "invalid API key {KEY}"}}}}"#
    );
    let answers = [
        None,
        Some(Response {
            status: 400,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: echo.clone().into_bytes(),
        }),
        Some(Response {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: echo.clone().into_bytes(),
        }),
        Some(Response {
            status: 200,
            headers: vec![],
            body: format!("<html>{KEY}</html>").into_bytes(),
        }),
    ];
    for answer in answers {
        let l = library(&[("01 a.flac", "1")]);
        let bus = Bus::new();
        let t = Canned(answer.clone());
        let client = AcoustIdClient::new(&t, KEY);
        let env = FingerprintEnv {
            conn: l.db.connection(),
            fingerprinter: &FakePrints,
            acoustid: Some(&client as &dyn AcoustIdLookup),
            bus: &bus,
            now_iso: NOW,
            cancel: None,
        };
        let album = l.album.to_string_lossy().into_owned();
        let (_, lines) = capture(|| fingerprint_album(&env, &l.id, &album, false));
        let r = has(
            &lines,
            Level::Debug,
            FP,
            "AcoustID lookup failed for 01 a.flac: ",
        );
        assert!(!r.message.contains(KEY), "{answer:?}: {}", r.message);
        let w = has(
            &lines,
            Level::Warning,
            FP,
            "AcoustID lookup failed for 1 of 1 ",
        );
        assert!(!w.message.contains(KEY), "{answer:?}: {}", w.message);
    }
}

// ---------- musaic.services.apply ----------

const APPLY: &str = "musaic.services.apply";

fn apply(l: &mut Lib, req: &ApplyRequest) -> (Vec<(String, Json)>, Vec<Record>) {
    let bus = Bus::new();
    let now = || NOW.to_string();
    let mut env = ApplyEnv {
        conn: l.db.connection_mut(),
        bus: &bus,
        covers: &NoCovers,
        now: &now,
        track_order: TrackOrder::Filename,
    };
    let mut items = Vec::new();
    let ((), lines) = capture(|| {
        apply_album_changes(&mut env, req, &mut |it| {
            items.push((it.event.to_string(), it.data));
            ControlFlow::Continue(())
        });
    });
    (items, lines)
}

fn request(l: &Lib) -> ApplyRequest {
    ApplyRequest {
        album_id: l.id.clone(),
        album_path: l.album.to_string_lossy().into_owned(),
        library_dir: Some(l.lib_s.clone()),
        ..Default::default()
    }
}

// Python app: musaic/services/apply.py:141
// `_log.exception("Failed to migrate track tags %s → %s on album %s", ...)`.
// Tag rows follow a rename. Driven by a TEMP trigger that makes the track_tags move fail; the
// apply carries on to `updated_album`, as in the Python app.
#[test]
fn track_tag_migration_failure_is_logged() {
    let mut l = library(&[("01 a.flac", "1")]);
    l.db.connection()
        .execute(
            "INSERT INTO track_tags (album_id, filename, tag, created_at) VALUES (?1, '01 a.flac', 'x', ?2)",
            [l.id.as_str(), NOW],
        )
        .unwrap();
    l.db.connection()
        .execute_batch(
            "CREATE TEMP TRIGGER no_move BEFORE DELETE ON track_tags \
             BEGIN SELECT RAISE(ABORT, 'track_tags locked (test)'); END;",
        )
        .unwrap();
    let mut req = request(&l);
    req.track_suggestions = vec![(
        "01 a.flac".into(),
        vec![("tracknumber".into(), Some("2".into()))],
    )];
    let (items, lines) = apply(&mut l, &req);
    assert!(l.album.join("02 a.flac").is_file());
    assert_eq!(items.last().unwrap().0, "updated_album", "{items:?}");
    let r = has(&lines, Level::Error, APPLY, "Failed to migrate track tags ");
    assert_eq!(
        r.message,
        format!(
            "Failed to migrate track tags 01 a.flac \u{2192} 02 a.flac on album {}: track_tags locked (test)",
            l.id
        )
    );
    assert_eq!(lines.len(), 1, "{lines:?}");
}

// Python app: musaic/services/apply.py:257
// `_log.exception("apply_album_changes_iter failed for album %s", album_id)`.
// The post-apply upsert. Driven by a TEMP trigger that makes the post-apply upsert fail:
// the stream ends with `error {detail}` carrying the same text.
#[test]
fn apply_failure_is_logged() {
    let mut l = library(&[("01 a.flac", "1")]);
    l.db.connection()
        .execute_batch(
            "CREATE TEMP TRIGGER no_upd BEFORE UPDATE ON albums \
             BEGIN SELECT RAISE(ABORT, 'albums locked (test)'); END; \
             CREATE TEMP TRIGGER no_ins BEFORE INSERT ON albums \
             BEGIN SELECT RAISE(ABORT, 'albums locked (test)'); END;",
        )
        .unwrap();
    let mut req = request(&l);
    req.suggestions = vec![("genre".into(), Some("Drone".into()))];
    let (items, lines) = apply(&mut l, &req);
    let (event, data) = items.last().unwrap();
    assert_eq!(event, "error", "{items:?}");
    let detail = data
        .get("detail")
        .and_then(Json::as_str)
        .unwrap()
        .to_string();
    assert!(detail.contains("albums locked (test)"), "{detail}");
    let r = has(
        &lines,
        Level::Error,
        APPLY,
        "apply_album_changes_iter failed ",
    );
    assert_eq!(
        r.message,
        format!(
            "apply_album_changes_iter failed for album {}: {detail}",
            l.id
        )
    );
    assert_eq!(lines.len(), 1, "{lines:?}");
}

// The Python app's own `yield "error", ...` for a failed cover download is not
// an exception: `error {detail}`, no line from services.apply.
#[test]
fn failed_cover_download_is_not_an_apply_failure() {
    let mut l = library(&[("01 a.flac", "1")]);
    let mut req = request(&l);
    req.cover_url = Some("https://covers.invalid/x.jpg".into());
    let (items, lines) = apply(&mut l, &req);
    assert_eq!(items.last().unwrap().0, "error");
    assert!(lines.iter().all(|r| r.target != APPLY), "{lines:?}");
}

// ---------- musaic.thumbs ----------

const THUMBS: &str = "musaic.thumbs";

fn thumbs_golden() -> serde_json::Value {
    musaic_testdata::golden("thumbs/thumbs.json")
}

// Python app: musaic/thumbs.py:98 `log.warning("thumbnail generation failed: %s", e)`.
// The Python app made no thumbnail for the golden's truncated JPEG
// (Pillow: "image file is truncated (N bytes not processed)") nor for bytes
// that are no image (Pillow: "cannot identify image file <...>").
#[test]
fn thumbnail_failure_is_logged() {
    use base64::Engine as _;
    let g = thumbs_golden();
    let row = &g["patterns"]["jpeg-truncated"];
    assert!(row["thumb_b64"].is_null(), "the Python app made none");
    let truncated = base64::engine::general_purpose::STANDARD
        .decode(row["input_b64"].as_str().unwrap())
        .unwrap();
    for (input, want) in [
        (
            truncated,
            "thumbnail generation failed: image file is truncated",
        ),
        (
            b"not an image".to_vec(),
            "thumbnail generation failed: cannot identify image file",
        ),
    ] {
        let (out, lines) = capture(|| musaic_services::thumbs::make_thumbnail(&input));
        assert!(out.is_none());
        assert_eq!(lines.len(), 1, "{lines:?}");
        let r = has(
            &lines,
            Level::Warning,
            THUMBS,
            "thumbnail generation failed: ",
        );
        assert_eq!(r.message, want);
    }
}

// Python app: musaic/thumbs.py:69 `log.warning("failed to read on-disk cover %s: %s", disk, e)`.
// An unreadable cover.jpg; no embedded cover, so no thumbnail.
#[test]
fn unreadable_disk_cover_is_logged() {
    let l = library(&[("01 a.flac", "1")]);
    let cover = l.album.join("cover.jpg");
    std::fs::write(&cover, b"x").unwrap();
    std::fs::set_permissions(&cover, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&cover).is_ok() {
        eprintln!("running as a user that reads mode-000 files; nothing to check");
        return;
    }
    let album = Json::obj([
        ("cover_file", Json::from("cover.jpg")),
        ("has_embedded_cover", Json::from(false)),
    ]);
    let thumbs = l.base.join("thumbs");
    let path = l.album.to_string_lossy().into_owned();
    let (out, lines) =
        capture(|| musaic_services::covers::ensure_thumb(&thumbs, &l.id, &path, &album, false));
    std::fs::set_permissions(&cover, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(out.is_none());
    assert_eq!(lines.len(), 1, "{lines:?}");
    has(
        &lines,
        Level::Warning,
        THUMBS,
        &format!("failed to read on-disk cover {}: ", cover.display()),
    );
}

// Python app: musaic/thumbs.py:142 `log.warning("delete_thumb(%s) failed: %s", album_id, e)`.
// A missing thumb is not a failure (FileNotFoundError: False, no line); a
// path that cannot be unlinked (a directory) is.
#[test]
fn delete_thumb_failure_is_logged() {
    use musaic_services::covers::delete_thumb;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (gone, lines) = capture(|| delete_thumb(dir, "missing"));
    assert!(!gone);
    assert!(lines.is_empty(), "{lines:?}");
    std::fs::write(dir.join("ok.jpg"), b"x").unwrap();
    assert!(delete_thumb(dir, "ok"));
    std::fs::create_dir(dir.join("dir.jpg")).unwrap();
    let (gone, lines) = capture(|| delete_thumb(dir, "dir"));
    assert!(!gone);
    assert_eq!(lines.len(), 1, "{lines:?}");
    has(&lines, Level::Warning, THUMBS, "delete_thumb(dir) failed: ");
}

// ---------- musaic.track_tags_sidecar / musaic.cache ----------

const SIDECAR: &str = "musaic.track_tags_sidecar";

fn sidecar_dir(content: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    std::fs::write(musaic_fsops::sidecar::sidecar_path(&dir), content).unwrap();
    (tmp, dir)
}

// Python app: musaic/track_tags_sidecar.py:71 `log.warning("Malformed tag sidecar %s: %s", p, e)`
// and :74 `log.warning("Tag sidecar %s has wrong root shape (expected object)", p)`.
#[test]
fn malformed_and_misshapen_sidecars_are_logged() {
    use musaic_services::sidecar_import::read_sidecar;
    let (_t, dir) = sidecar_dir(b"{not json");
    let p = musaic_fsops::sidecar::sidecar_path(&dir);
    let (map, lines) = capture(|| read_sidecar(&dir));
    assert!(map.unwrap().is_empty());
    assert_eq!(lines.len(), 1, "{lines:?}");
    has(
        &lines,
        Level::Warning,
        SIDECAR,
        &format!("Malformed tag sidecar {}: ", p.display()),
    );

    let (_t, dir) = sidecar_dir(b"[\"a.flac\"]");
    let p = musaic_fsops::sidecar::sidecar_path(&dir);
    let (map, lines) = capture(|| read_sidecar(&dir));
    assert!(map.unwrap().is_empty());
    assert_eq!(lines.len(), 1, "{lines:?}");
    let r = has(&lines, Level::Warning, SIDECAR, "Tag sidecar ");
    assert_eq!(
        r.message,
        format!(
            "Tag sidecar {} has wrong root shape (expected object)",
            p.display()
        )
    );

    // Empty and well-formed files log nothing.
    for ok in [b"  \n".as_slice(), br#"{"a.flac": ["x"]}"#.as_slice()] {
        let (_t, dir) = sidecar_dir(ok);
        let (_, lines) = capture(|| read_sidecar(&dir));
        assert!(lines.is_empty(), "{lines:?}");
    }
}

// Python app: musaic/track_tags_sidecar.py:64 `log.warning("Could not read tag sidecar %s: %s", p, e)`.
#[test]
fn unreadable_sidecar_is_logged() {
    use musaic_services::sidecar_import::read_sidecar;
    let (_t, dir) = sidecar_dir(br#"{"a.flac": ["x"]}"#);
    let p = musaic_fsops::sidecar::sidecar_path(&dir);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&p).is_ok() {
        eprintln!("running as a user that reads mode-000 files; nothing to check");
        return;
    }
    let (map, lines) = capture(|| read_sidecar(&dir));
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(map.unwrap().is_empty());
    assert_eq!(lines.len(), 1, "{lines:?}");
    has(
        &lines,
        Level::Warning,
        SIDECAR,
        &format!("Could not read tag sidecar {}: ", p.display()),
    );
}

// Python app: musaic/cache.py:1534
// `log.exception("Failed to import tag sidecar for album %s", album_id)`.
// A sidecar that is not UTF-8: the Python app's `read_text` raised
// UnicodeDecodeError, which read_sidecar's `except OSError` does not catch,
// so the importer's per-album handler logged it and went on.
#[test]
fn sidecar_import_failure_is_logged() {
    let mut l = library(&[("01 a.flac", "1")]);
    std::fs::write(
        musaic_fsops::sidecar::sidecar_path(&l.album),
        b"{\"01 a.flac\": [\"\xff\xfe\"]}",
    )
    .unwrap();
    let path = l.album.to_string_lossy().into_owned();
    let id = l.id.clone();
    let (n, lines) = capture(|| {
        musaic_services::sidecar_import::import_track_tag_sidecars(
            l.db.connection_mut(),
            [(id.as_str(), path.as_str())],
            NOW,
        )
    });
    assert_eq!(n, 0);
    assert_eq!(lines.len(), 1, "{lines:?}");
    has(
        &lines,
        Level::Error,
        "musaic.cache",
        &format!("Failed to import tag sidecar for album {id}: "),
    );
}
