//! Replays the scenarios the Python app ran
//! (testdata/golden/fsops/scenarios.json) and compares every step: the
//! returned value, the AlbumRenamed event, the library tree, the trash tree,
//! the sidecar and the database rows.
//!
//! Covers plans, renames, the same-file dir rename, refusals, AlbumRenamed,
//! the trash, cover discovery, the deliberate NFC change, and track_tags
//! following renames with the sidecar rewrite.
//!
//! Several outcomes depend on the volume (case- and normalisation-
//! insensitive APFS when the golden was recorded); the test refuses to run on
//! a volume that behaves differently instead of passing vacuously.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use musaic_core::album_id;
use musaic_db::{Db, albums, renames, settings};
use musaic_fsops::plan::{Plan, derive_plan};
use musaic_fsops::rename::{PlanEntry, RenameEnv, RenameOutcome, execute_renames};
use musaic_fsops::{cover, sidecar, trash};
use musaic_scan::{TrackOrder, py_resolve, scan_album, scan_library};
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde_json::{Map, Value as J, json};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

const NOW: &str = "2026-01-01T00:00:00Z";

fn sha(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// Mirror of the generator's `flac_bytes`: STREAMINFO + VORBIS_COMMENT, no audio.
fn flac_bytes(tags: &[(String, String)], seconds: u64) -> Vec<u8> {
    let total = 44100 * seconds;
    let mut si = Vec::new();
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&[0; 6]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | total;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let vendor = b"fsops-golden";
    let mut vc = Vec::new();
    vc.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    vc.extend_from_slice(vendor);
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
    out.push(0x84);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    out
}

fn s(j: &J) -> &str {
    j.as_str().unwrap()
}

struct Ctx {
    lib: PathBuf,
    lib_s: String,
    trash: PathBuf,
    trash_s: String,
    ids: HashMap<String, String>,
}

impl Ctx {
    fn learn(&mut self) {
        fn walk(dir: &Path, root: &Path, ids: &mut HashMap<String, String>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if e.file_type().unwrap().is_dir() {
                    let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                    ids.insert(album_id(&py_resolve(&p).to_string_lossy()), rel);
                    walk(&p, root, ids);
                }
            }
        }
        let (lib, mut ids) = (self.lib.clone(), std::mem::take(&mut self.ids));
        walk(&lib, &lib, &mut ids);
        self.ids = ids;
    }

    fn norm(&self, v: J) -> J {
        match v {
            J::String(x) => match self.ids.get(&x) {
                Some(rel) => json!(format!("<ID:{rel}>")),
                None => json!(
                    x.replace(&self.trash_s, "<TRASH>")
                        .replace(&self.lib_s, "<LIB>")
                ),
            },
            J::Array(a) => J::Array(a.into_iter().map(|x| self.norm(x)).collect()),
            J::Object(o) => J::Object(o.into_iter().map(|(k, x)| (k, self.norm(x))).collect()),
            other => other,
        }
    }

    fn rows(&self, conn: &Connection, sql: &str) -> Vec<J> {
        let mut stmt = conn.prepare(sql).unwrap();
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        stmt.query_map([], |r| {
            let mut o = Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = match r.get_ref(i)? {
                    ValueRef::Null => J::Null,
                    ValueRef::Integer(x) => json!(x),
                    ValueRef::Real(x) => json!(x),
                    ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                    ValueRef::Blob(_) => unreachable!(),
                };
                o.insert(n.clone(), v);
            }
            Ok(self.norm(J::Object(o)))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    fn dump(&self, conn: &Connection, album_cols: &str) -> J {
        let key = |r: &J, ks: &[&str]| -> Vec<String> {
            ks.iter()
                .map(|k| match &r[*k] {
                    J::String(x) => x.clone(),
                    other => other.to_string(), // str(None) ordering is never exercised
                })
                .collect()
        };
        let sorted = |mut v: Vec<J>, ks: &[&str]| {
            v.sort_by_key(|r| key(r, ks));
            J::Array(v)
        };
        json!({
            "albums": sorted(self.rows(conn, &format!("SELECT {album_cols} FROM albums")), &["path"]),
            "track_tags": sorted(self.rows(conn, "SELECT album_id, filename, tag, created_at FROM track_tags"), &["album_id", "filename", "tag"]),
            "track_fingerprints": sorted(self.rows(conn, "SELECT album_id, filename, fingerprint FROM track_fingerprints"), &["album_id", "filename"]),
            "check_findings": sorted(self.rows(conn, "SELECT check_id, album_id, summary FROM check_findings"), &["check_id", "album_id", "summary"]),
            "ai_jobs": sorted(self.rows(conn, "SELECT id, album_id FROM ai_jobs"), &["id"]),
            "blocklist": sorted(self.rows(conn, "SELECT album_id, release_group_mbid FROM album_blocked_mb_release_groups"), &["album_id", "release_group_mbid"]),
        })
    }
}

/// `os.walk`-style recursive listing: dirs and files, rel path, size, sha256.
fn listing(root: &Path, with_text: bool) -> J {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<J>, with_text: bool) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_str().unwrap().to_string();
            if e.file_type().unwrap().is_dir() {
                out.push(json!({"path": rel, "kind": "dir"}));
                walk(&p, root, out, with_text);
            } else {
                let data = std::fs::read(&p).unwrap();
                let mut o =
                    json!({"path": rel, "kind": "file", "size": data.len(), "sha256": sha(&data)});
                if with_text {
                    o["text"] = json!(String::from_utf8_lossy(&data));
                }
                out.push(o);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out, with_text);
    out.sort_by(|a, b| s(&a["path"]).cmp(s(&b["path"])));
    J::Array(out)
}

fn plan_json(p: &Option<Plan>) -> J {
    let Some(p) = p else { return J::Null };
    let mut o = json!({
        "dir_name": p.dir_name, "is_compilation": p.is_compilation, "template": p.template,
        "dir_template": p.dir_template, "headline": p.headline,
        "renames": p.renames.iter().map(|r| json!({
            "filename": r.filename, "expected": r.expected, "tracknumber": r.tracknumber,
            "title": r.title, "artist": r.artist, "collides_with": r.collides_with})).collect::<Vec<_>>(),
        "dir_rename": p.dir_rename.as_ref().map(|d| json!({"old": d.old, "new": d.new})),
    });
    if p.album_missing {
        o["album_missing"] = json!(true);
    }
    if let Some(e) = &p.error {
        o["error"] = json!(e);
    }
    o
}

fn outcome_json(o: &RenameOutcome) -> J {
    let mut j = json!({"ok": o.ok, "message": o.message, "renamed": o.renamed});
    if o.already_renamed {
        j["already_renamed"] = json!(true);
    }
    if let Some(d) = &o.detail {
        j["errors"] = json!(d.errors);
        j["dir_renamed"] = json!(d.dir_renamed);
        j["new_album_id"] = json!(d.new_album_id);
        j["dir_migration"] = match d.dir_migration {
            Some(m) => json!({"albums": m.albums, "track_fingerprints": m.track_fingerprints,
                              "track_tags": m.track_tags, "check_findings": m.check_findings, "ai_jobs": m.ai_jobs,
                              "album_blocked_mb_release_groups": m.album_blocked_mb_release_groups}),
            None => json!({}),
        };
    }
    j
}

/// The Python app's plan minus what differs only by NFC/NFD form.
fn without_nfc_only(mut want: J) -> (J, usize) {
    let nfc = |x: &J| s(x).nfc().collect::<String>();
    let mut dropped = 0;
    if want.is_null() {
        return (want, 0);
    }
    if let Some(d) = want.get("dir_rename").filter(|d| !d.is_null()).cloned()
        && nfc(&d["old"]) == nfc(&d["new"])
    {
        want["dir_rename"] = J::Null;
        dropped += 1;
    }
    let renames = want["renames"].as_array().cloned().unwrap_or_default();
    let kept: Vec<J> = renames
        .into_iter()
        .filter(|r| {
            let keep = nfc(&r["filename"]) != nfc(&r["expected"]);
            dropped += usize::from(!keep);
            keep
        })
        .collect();
    want["renames"] = J::Array(kept);
    (want, dropped)
}

fn probe_case_insensitive(d: &Path) -> bool {
    let p = d.join("CaseProbe");
    std::fs::write(&p, b"").unwrap();
    let r = d.join("caseprobe").exists();
    std::fs::remove_file(p).unwrap();
    r
}

fn probe_normalization_insensitive(d: &Path) -> bool {
    let p = d.join("\u{e9}");
    std::fs::write(&p, b"").unwrap();
    let r = d.join("e\u{301}").exists();
    std::fs::remove_file(p).unwrap();
    r
}

fn run_scenario(sc: &J, album_cols: &str, delta: Delta) -> usize {
    let id = s(&sc["id"]);
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let lib = base.join("lib");
    std::fs::create_dir(&lib).unwrap();
    let trash_root = trash::trash_dir(&base.join("config"));
    let mut cx = Ctx {
        lib_s: lib.to_string_lossy().into_owned(),
        trash_s: trash_root.to_string_lossy().into_owned(),
        lib: lib.clone(),
        trash: trash_root.clone(),
        ids: HashMap::new(),
    };

    // Library from the recipe; every generated FLAC must be byte-identical.
    for a in sc["albums"].as_array().unwrap() {
        let d = lib.join(s(&a["dir"]));
        std::fs::create_dir_all(&d).unwrap();
        for f in a["files"].as_array().unwrap() {
            let p = d.join(s(&f["name"]));
            if f.get("subdir").is_some() {
                std::fs::create_dir(&p).unwrap();
            } else if let Some(t) = f.get("text") {
                std::fs::write(&p, s(t)).unwrap();
            } else {
                let tags: Vec<(String, String)> = f["tags"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|kv| (s(&kv[0]).to_string(), s(&kv[1]).to_string()))
                    .collect();
                let data = flac_bytes(&tags, f["seconds"].as_u64().unwrap());
                assert_eq!(
                    sha(&data),
                    s(&f["sha256"]),
                    "{id}: FLAC recipe drifted for {}",
                    s(&f["name"])
                );
                std::fs::write(&p, data).unwrap();
            }
        }
    }
    cx.learn();

    // Database: the Python app's empty v16 db (copied), settings, a scan, seeded rows.
    let db_path = base.join("library.db");
    std::fs::copy(musaic_testdata::public("db/v16-empty.db"), &db_path).unwrap();
    let (mut db, _) = Db::open(&db_path).unwrap();
    let conn = db.connection_mut();
    if let Some(st) = sc.get("settings").and_then(J::as_object) {
        for (k, v) in st {
            settings::set_raw(conn, k, s(v)).unwrap();
        }
    }
    let scanned = scan_library(&lib, TrackOrder::Filename);
    albums::save_snapshot(conn, &cx.lib_s, NOW, NOW, &scanned).unwrap();
    let mut cur: HashMap<String, PathBuf> = sc["albums"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (s(&a["dir"]).to_string(), lib.join(s(&a["dir"]))))
        .collect();
    let aid =
        |cur: &HashMap<String, PathBuf>, k: &str| album_id(&py_resolve(&cur[k]).to_string_lossy());
    let seed = &sc["seed"];
    let list = |k: &str| {
        seed.get(k)
            .and_then(J::as_array)
            .cloned()
            .unwrap_or_default()
    };
    for r in list("track_tags") {
        conn.execute(
            "INSERT INTO track_tags(album_id, filename, tag, created_at) VALUES (?1,?2,?3,?4)",
            [aid(&cur, s(&r[0])).as_str(), s(&r[1]), s(&r[2]), s(&r[3])],
        )
        .unwrap();
    }
    for r in list("track_fingerprints") {
        conn.execute(
            "INSERT INTO track_fingerprints(album_id, filename, fingerprint, duration_s, last_computed_at) VALUES (?1,?2,?3,3.0,?4)",
            [aid(&cur, s(&r[0])).as_str(), s(&r[1]), r.get(2).map_or("AQAA", s), NOW],
        )
        .unwrap();
    }
    for r in list("findings") {
        conn.execute(
            "INSERT INTO check_findings(check_id, album_id, severity, summary, first_seen_at, last_seen_at) VALUES (?1,?2,'info',?3,'t','t')",
            [s(&r[1]), aid(&cur, s(&r[0])).as_str(), s(&r[2])],
        )
        .unwrap();
    }
    for r in list("ai_jobs") {
        conn.execute(
            "INSERT INTO ai_jobs(id, album_id, album_path, job_type, status, created_at) VALUES (?1,?2,?3,'metadata','queued','t')",
            [s(&r[1]), aid(&cur, s(&r[0])).as_str(), &cur[s(&r[0])].to_string_lossy()],
        )
        .unwrap();
    }
    for r in list("blocklist") {
        conn.execute(
            "INSERT INTO album_blocked_mb_release_groups(album_id, release_group_mbid, blocked_at) VALUES (?1,?2,'t')",
            [aid(&cur, s(&r[0])).as_str(), s(&r[1])],
        )
        .unwrap();
    }

    assert_eq!(
        listing(&lib, false),
        sc["initial"]["files"],
        "{id}: initial files"
    );
    assert_eq!(
        cx.dump(conn, album_cols),
        sc["initial"]["db"],
        "{id}: initial db"
    );

    let lib_s = cx.lib_s.clone();
    let env = RenameEnv {
        library_dir: Some(lib_s.as_str()),
        now: NOW,
        track_order: TrackOrder::Filename,
    };
    let mut nfc_dropped = 0;
    for (i, st) in sc["steps"].as_array().unwrap().iter().enumerate() {
        // The Python app's record of this step, with the scenario's stated
        // deliberate-change delta applied (identity for most scenarios).
        let want_st = {
            let mut w = st.clone();
            delta(sc, i, &mut w);
            sort_snapshot(&mut w);
            w
        };
        let st = &want_st;
        let step = &st["step"];
        let op = s(&step["op"]);
        let ctx = format!("{id} step {i} ({op})");
        let mut renamed_event = None;
        let result: J = match op {
            "derive_plan" => {
                let p = derive_plan(conn, &aid(&cur, s(&step["album"]))).unwrap();
                let (want, dropped) = without_nfc_only(st["result"].clone());
                nfc_dropped += dropped;
                assert_eq!(cx.norm(plan_json(&p)), want, "{ctx}: plan");
                st["result"].clone()
            }
            "execute" | "execute_unknown_id" => {
                let (album, input) = if op == "execute" {
                    (aid(&cur, s(&step["album"])), st["result"]["input"].clone())
                } else {
                    (
                        "0123456789abcdef".to_string(),
                        json!({"plan": [], "dir_rename": null}),
                    )
                };
                let entry = |e: &J| PlanEntry {
                    old: s(&e["old"]).to_string(),
                    new: s(&e["new"]).to_string(),
                };
                let plan: Vec<PlanEntry> = input["plan"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(entry)
                    .collect();
                let dr = (!input["dir_rename"].is_null()).then(|| entry(&input["dir_rename"]));
                let out = execute_renames(conn, &env, &album, &plan, dr.as_ref());
                renamed_event = out.album_renamed.clone();
                if let Some(dr) = &dr {
                    let old = cur[s(&step["album"])].clone();
                    let names: Vec<String> = std::fs::read_dir(old.parent().unwrap())
                        .unwrap()
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect();
                    let new_name = musaic_core::pycompat::strip(&dr.new).to_string();
                    let old_name = old.file_name().unwrap().to_string_lossy().into_owned();
                    if !names.contains(&old_name) && names.contains(&new_name) {
                        cur.insert(
                            s(&step["album"]).to_string(),
                            old.parent().unwrap().join(new_name),
                        );
                    }
                }
                cx.learn();
                let got = cx.norm(outcome_json(&out));
                let want = if op == "execute" {
                    &st["result"]["output"]
                } else {
                    &st["result"]
                };
                assert_eq!(&got, want, "{ctx}: outcome");
                st["result"].clone()
            }
            "mark_missing" => {
                albums::mark_album_missing(
                    conn,
                    &aid(&cur, s(&step["album"])),
                    "2026-02-01T00:00:00Z",
                )
                .unwrap();
                J::Null
            }
            "remove_dir" => {
                std::fs::remove_dir_all(&cur[s(&step["album"])]).unwrap();
                J::Null
            }
            "set_setting" => {
                settings::set_raw(conn, s(&step["key"]), s(&step["value"])).unwrap();
                J::Null
            }
            "rename_track_filename" => {
                let n = renames::rename_track_filename(
                    conn,
                    &aid(&cur, s(&step["album"])),
                    s(&step["old"]),
                    s(&step["new"]),
                )
                .unwrap();
                json!(n)
            }
            "clear_track_tags" => {
                conn.execute(
                    "DELETE FROM track_tags WHERE album_id = ?1",
                    [aid(&cur, s(&step["album"]))],
                )
                .unwrap();
                J::Null
            }
            "write_sidecar" => {
                let a = aid(&cur, s(&step["album"]));
                let map = renames::album_track_tags(conn, &a).unwrap();
                let wrote =
                    sidecar::resync_after_renames(conn, &a, &cur[s(&step["album"])]).unwrap();
                let got = cx.norm(json!({"map": map, "wrote": wrote}));
                let sp = sidecar::sidecar_path(&cur[s(&step["album"])]);
                let text = sp.is_file().then(|| std::fs::read_to_string(&sp).unwrap());
                assert_eq!(json!(text), st["sidecar"], "{ctx}: sidecar bytes");
                got
            }
            "write_file" => {
                std::fs::write(lib.join(s(&step["file"])), s(&step["text"])).unwrap();
                J::Null
            }
            "trash" => {
                let stamp = s(&step["stamp"]).replace(':', "-");
                let r = trash::send_to_trash(
                    &trash_root,
                    &lib.join(s(&step["file"])),
                    step["album_dir_name"].as_str(),
                    || stamp,
                );
                let got = cx.norm(match r {
                    Ok(p) => json!({"ok": p.to_string_lossy()}),
                    Err(e) => json!({"error": e.to_string()}),
                });
                assert_eq!(listing(&cx.trash, true), st["trash"], "{ctx}: trash tree");
                got
            }
            "cover" => {
                let d = &cur[s(&step["album"])];
                let found = scan_album(d, TrackOrder::Filename).and_then(|a| a.cover_file);
                let disk = cover::disk_cover_path(&d.to_string_lossy(), found.as_deref());
                let (mime, digest) = match &disk {
                    Some(p) => (
                        Some(cover::disk_cover_mime(p)),
                        Some(sha(&std::fs::read(p).unwrap())),
                    ),
                    None => (None, None),
                };
                cx.norm(json!({"cover_file": found, "disk_path": disk.map(|p| p.to_string_lossy().into_owned()),
                               "mime": mime, "sha256": digest}))
            }
            other => panic!("unknown op {other}"),
        };
        assert_eq!(result, st["result"], "{ctx}: result");
        let want_events: Vec<&J> = st["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["type"] == "AlbumRenamed")
            .collect();
        let got_event = renamed_event.map(|e| {
            cx.norm(json!({"type": "AlbumRenamed", "old_album_id": e.old_album_id, "new_album_id": e.new_album_id}))
        });
        assert_eq!(
            got_event.as_ref(),
            want_events.first().copied(),
            "{ctx}: AlbumRenamed"
        );
        assert!(want_events.len() <= 1, "{ctx}: more than one AlbumRenamed");
        assert_eq!(listing(&lib, false), st["files"], "{ctx}: files");
        assert_eq!(cx.dump(conn, album_cols), st["db"], "{ctx}: db");
    }
    nfc_dropped
}

fn golden() -> J {
    serde_json::from_str(
        &std::fs::read_to_string(musaic_testdata::public("golden/fsops/scenarios.json")).unwrap(),
    )
    .unwrap()
}

/// Whether the temp dir's case / normalisation sensitivity is the one the
/// golden was recorded on (APFS: case- and normalisation-insensitive). On
/// another kind of volume (Linux ext4, a case-sensitive APFS image) the
/// recorded file trees cannot match, so the scenario is skipped with a
/// printed reason instead of compared against the wrong expectation.
fn same_volume_kind(g: &J) -> bool {
    let probe = tempfile::tempdir().unwrap();
    let have = (
        probe_case_insensitive(probe.path()),
        probe_normalization_insensitive(probe.path()),
    );
    let want = (
        g["case_insensitive_fs"].as_bool().unwrap(),
        g["normalization_insensitive_fs"].as_bool().unwrap(),
    );
    if have != want {
        println!(
            "skipped: {}: temp dir is (case-insensitive, normalisation-insensitive) = {have:?}; \
             the golden was recorded on {want:?}",
            musaic_testdata::test_name()
        );
    }
    have == want
}

fn run(id: &str, delta: Delta) {
    let g = golden();
    if !same_volume_kind(&g) {
        return;
    }
    let cols: Vec<&str> = g["album_columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(s)
        .collect();
    let sc = g["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|sc| sc["id"] == id)
        .unwrap_or_else(|| panic!("scenario {id} missing from the golden"));
    let dropped = run_scenario(sc, &cols.join(", "), delta);
    // Only nfc-nfd has normalisation-only differences: the Python app
    // proposed a dir rename and a file rename, then the file rename again.
    let want = if id == "nfc-nfd" { 3 } else { 0 };
    assert_eq!(dropped, want, "{id}: normalisation-only plan entries");
}

#[test]
fn every_scenario_in_the_golden_is_replayed() {
    let ids: Vec<String> = golden()["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|sc| s(&sc["id"]).to_string())
        .collect();
    assert_eq!(ids, SCENARIOS);
}

macro_rules! scenarios {
    ($($name:ident => $id:literal, $delta:expr, $why:literal;)*) => {
        const SCENARIOS: &[&str] = &[$($id),*];
        $(
            #[doc = $why]
            #[test]
            fn $name() {
                run($id, $delta);
            }
        )*
    };
}

scenarios! {
    every_token => "every-token", d_every_token, "Every template token; dir + file rename; all child tables follow. Deltas: fingerprints follow renames, blocklist repointed, sidecar rewritten.";
    artist_cascade => "artist-cascade", same, "ARTIST-only / dir_artist cascade; untitled / unnumbered tracks skipped.";
    sanitise => "sanitise", same, "Unsafe and control characters, separators, dots.";
    nfc_nfd => "nfc-nfd", d_nfc_nfd, "Same-file dir rename onto the NFC spelling. Deltas: blocklist repointed, case/normalisation-only renames work, sidecar rewritten.";
    case_only_dir => "case-only-dir", d_case_only_dir, "Case-only dir rename passes the same-file check and gets a new id. Delta: blocklist repointed.";
    case_only_file => "case-only-file", d_case_only_file, "Case-only file rename. Delta: works instead of being refused.";
    targets_exist => "targets-exist", d_targets_exist, "Existing dir / unrelated file refused; two-pass swap. Delta: sidecar rewritten.";
    multi_disc => "multi-disc", d_multi_disc, "'(disc N of M)' insertion and {disc}. Delta: blocklist repointed.";
    unnumbered => "unnumbered", same, "Unnumbered tracks; compilation templates; already-renamed resubmit.";
    noop => "noop", same, "An empty plan still yields AlbumRenamed(id, id).";
    refusals => "refusals", same, "Separators, empty target, unknown / Missing / vanished album.";
    dir_then_file_refused => "dir-then-file-refused", d_dir_then_file_refused, "Dir rename, then a refused file rename. Delta: refused before the dir rename.";
    blocklist_fk => "blocklist-fk", d_blocklist_fk, "Album with a blocked release group. Delta: blocklist repointed, rename succeeds.";
    duplicate_targets => "duplicate-targets", d_duplicate_targets, "Two files onto one name. Delta: refused, nothing renamed.";
    track_tags_migration => "track-tags-migration", same, "track_tags union-move and sidecar rewrite.";
    trash => "trash", d_trash, "Trash layout and collision naming. Delta: no trashed copy is overwritten.";
    cover_discovery => "cover-discovery", same, "Cover discovery order and case fallback.";
    duplicate_targets_folded => "duplicate-targets-folded", d_duplicate_targets_folded, "Targets equal by case / normalisation refused.";
    trash_burst => "trash-burst", d_trash_burst, "Trash burst: counter after the stamp, nothing replaced.";
    fingerprint_at_target => "fingerprint-at-target", d_fingerprint_at_target, "Fingerprint rows follow renames.";
}

// ---------------------------------------------------------------------------
// Deliberate-change deltas.
//
// Each function receives the Python app's record of step `i` and turns it into
// the expected post-fix record by applying ONLY the stated differences;
// everything else must still equal the Python app. State carries over, so a
// delta stated "from step k" applies to every later snapshot too.
// ---------------------------------------------------------------------------

type Delta = fn(&J, usize, &mut J);

fn same(_: &J, _: usize, _: &mut J) {}

const SIDECAR: &str = ".media-manager-tags.json";
const BLOCKLIST_KEY: &str = "album_blocked_mb_release_groups";

fn id_of(rel: &str) -> String {
    format!("<ID:{rel}>")
}

/// Re-sort a snapshot the way the generator's dump and listing do.
fn sort_snapshot(st: &mut J) {
    let keys: &[(&str, &[&str])] = &[
        ("albums", &["path"]),
        ("track_tags", &["album_id", "filename", "tag"]),
        ("track_fingerprints", &["album_id", "filename"]),
        ("check_findings", &["check_id", "album_id", "summary"]),
        ("ai_jobs", &["id"]),
        ("blocklist", &["album_id", "release_group_mbid"]),
    ];
    for (t, ks) in keys {
        if let Some(rows) = st["db"].get_mut(*t).and_then(J::as_array_mut) {
            rows.sort_by_key(|r| {
                ks.iter()
                    .map(|k| r[*k].as_str().unwrap_or("").to_string())
                    .collect::<Vec<_>>()
            });
        }
    }
    for k in ["files", "trash"] {
        if let Some(v) = st.get_mut(k).and_then(J::as_array_mut) {
            v.sort_by(|a, b| s(&a["path"]).cmp(s(&b["path"])));
        }
    }
}

/// Every dir_migration in an execute output gains the blocklist count.
fn blocklist_key(st: &mut J, n: usize) {
    if let Some(m) = st["result"]["output"]
        .get_mut("dir_migration")
        .and_then(J::as_object_mut)
    {
        m.insert(BLOCKLIST_KEY.into(), json!(n));
    }
}

/// Tree: rename an entry (and everything under it).
fn tree_rename(st: &mut J, from: &str, to: &str) {
    for e in st["files"].as_array_mut().unwrap() {
        let p = s(&e["path"]).to_string();
        if p == from {
            e["path"] = json!(to);
        } else if let Some(rest) = p.strip_prefix(&format!("{from}/")) {
            e["path"] = json!(format!("{to}/{rest}"));
        }
    }
}

/// DB: rename a file of album `rel` in the given per-file tables, and the
/// album row's sample_track when it named that file.
fn db_rename_file(st: &mut J, rel: &str, old: &str, new: &str, tables: &[&str]) {
    for t in tables {
        for r in st["db"][*t].as_array_mut().unwrap() {
            if r["album_id"] == id_of(rel) && r["filename"] == old {
                r["filename"] = json!(new);
            }
        }
    }
    for r in st["db"]["albums"].as_array_mut().unwrap() {
        if r["sample_track"] == format!("<LIB>/{rel}/{old}") {
            r["sample_track"] = json!(format!("<LIB>/{rel}/{new}"));
        }
    }
}

/// Album `rel` (whatever the Python app did to it, incl. a rename to `also`) is
/// back to the scenario's initial tree and rows.
fn restore_album(sc: &J, st: &mut J, rel: &str, also: Option<&str>) {
    let init = &sc["initial"];
    let mine = |p: &str, r: &str| p == r || p.starts_with(&format!("{r}/"));
    let rels: Vec<&str> = std::iter::once(rel).chain(also).collect();
    let files = st["files"].as_array_mut().unwrap();
    files.retain(|e| !rels.iter().any(|r| mine(s(&e["path"]), r)));
    files.extend(
        init["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| mine(s(&e["path"]), rel))
            .cloned(),
    );
    let ids: Vec<String> = rels.iter().map(|r| id_of(r)).collect();
    for (t, col) in [
        ("albums", "id"),
        ("track_tags", "album_id"),
        ("track_fingerprints", "album_id"),
        ("check_findings", "album_id"),
        ("ai_jobs", "album_id"),
        ("blocklist", "album_id"),
    ] {
        let rows = st["db"][t].as_array_mut().unwrap();
        rows.retain(|r| !ids.iter().any(|i| r[col] == *i));
        rows.extend(
            init["db"][t]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r[col] == id_of(rel))
                .cloned(),
        );
    }
}

/// Snapshot rename of album dir `from` -> `to` (tree + every row: ids, paths,
/// dir_name).
fn move_dir(st: &mut J, from: &str, to: &str) {
    tree_rename(st, from, to);
    fn walk(v: &mut J, from: &str, to: &str) {
        match v {
            J::String(x) => {
                if *x == id_of(from) {
                    *x = id_of(to);
                } else if *x == format!("<LIB>/{from}") {
                    *x = format!("<LIB>/{to}");
                } else if let Some(rest) = x.strip_prefix(&format!("<LIB>/{from}/")) {
                    *x = format!("<LIB>/{to}/{rest}");
                }
            }
            J::Array(a) => a.iter_mut().for_each(|x| walk(x, from, to)),
            J::Object(o) => o.values_mut().for_each(|x| walk(x, from, to)),
            _ => {}
        }
    }
    walk(&mut st["db"], from, to);
    for r in st["db"]["albums"].as_array_mut().unwrap() {
        if r["dir_name"] == from {
            r["dir_name"] = json!(to);
        }
    }
}

fn file_entry(path: &str, data: &[u8], text: bool) -> J {
    let mut e = json!({"path": path, "kind": "file", "size": data.len(), "sha256": sha(data)});
    if text {
        e["text"] = json!(String::from_utf8_lossy(data));
    }
    e
}

/// The sidecar of album `rel`, as the expected snapshot's own
/// track_tags rows serialise (format pinned by track-tags-migration).
fn add_sidecar(st: &mut J, rel: &str) {
    let mut map: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for r in st["db"]["track_tags"].as_array().unwrap() {
        if r["album_id"] == id_of(rel) {
            map.entry(s(&r["filename"]).to_string())
                .or_default()
                .push(s(&r["tag"]).to_string());
        }
    }
    assert!(!map.is_empty(), "sidecar delta on an album without tags");
    let text = sidecar::render_sidecar(&map);
    let files = st["files"].as_array_mut().unwrap();
    files.retain(|e| e["path"] != format!("{rel}/{SIDECAR}"));
    files.push(file_entry(
        &format!("{rel}/{SIDECAR}"),
        text.as_bytes(),
        false,
    ));
}

/// Execute output of a run where every file of the plan was renamed.
fn success(st: &mut J, renamed: usize, dir: Option<J>, new_id: &str) {
    let msg = match (&dir, renamed) {
        (Some(_), 0) => "Renamed album directory".to_string(),
        (d, n) => format!(
            "{}{n} file{}",
            if d.is_some() {
                "Renamed album directory + "
            } else {
                ""
            },
            if n == 1 { "" } else { "s" }
        ),
    };
    st["result"]["output"] = json!({"ok": true, "message": msg, "renamed": renamed, "errors": [],
        "dir_renamed": dir.is_some(), "new_album_id": new_id, "dir_migration": dir.unwrap_or(json!({}))});
}

fn refused(st: &mut J, message: &str) {
    st["result"]["output"] = json!({"ok": false, "message": message, "renamed": 0});
    st["events"] = json!([]);
}

fn renamed_event(old: &str, new: &str) -> J {
    json!([{"type": "AlbumRenamed", "old_album_id": old, "new_album_id": new}])
}

fn migration(albums: usize, fps: usize, tags: usize, blocklist: usize) -> J {
    json!({"albums": albums, "track_fingerprints": fps, "track_tags": tags,
           "check_findings": 0, "ai_jobs": 0, BLOCKLIST_KEY: blocklist})
}

fn d_every_token(_: &J, i: usize, st: &mut J) {
    const DIR: &str = "Aurora Fields - First Light (2019) First Light Aurora Fields [FLAC]";
    if i == 1 {
        blocklist_key(st, 0); // blocklist repointed
    }
    if i >= 1 {
        // The fingerprint row follows its file.
        db_rename_file(
            st,
            DIR,
            "01 - Dawn Chorus.flac",
            "01 Aurora Fields _ Aurora Fields _ First Light _ 2019 _ Dawn Chorus.flac",
            &["track_fingerprints"],
        );
        add_sidecar(st, DIR); // sidecar rewritten
    }
}

fn d_case_only_dir(_: &J, i: usize, st: &mut J) {
    if i == 1 {
        blocklist_key(st, 0);
    }
}

fn d_multi_disc(_: &J, i: usize, st: &mut J) {
    if i == 3 {
        blocklist_key(st, 0);
    }
}

fn d_nfc_nfd(sc: &J, i: usize, st: &mut J) {
    // The Python app's new dir name (artist from the NFC tags, album from the NFD
    // dir name: mixed forms), as its step 1 input records it.
    let dir = s(&sc["steps"][1]["result"]["input"]["dir_rename"]["new"]).to_string();
    let nfd_file: String = "01 - Été.flac".nfd().collect();
    let nfc_file: String = "01 - Été.flac".nfc().collect();
    if i == 1 {
        // The NFD->NFC file rename is the same file, not a collision,
        // so the plan completes. Blocklist count in dir_migration.
        let new_id = st["events"][0]["new_album_id"].clone();
        success(st, 1, Some(migration(1, 0, 1, 0)), s(&new_id));
    }
    if i >= 1 {
        tree_rename(
            st,
            &format!("{dir}/{nfd_file}"),
            &format!("{dir}/{nfc_file}"),
        );
        db_rename_file(st, &dir, &nfd_file, &nfc_file, &["track_tags"]);
        // The Python app stopped before its rescan, leaving sample_track under the
        // old (NFD) dir and dir_artist parsed from the old (NFD) name; the
        // completed run rescans: both now come from the new dir name.
        let dir_artist = dir.split(" - ").next().unwrap().to_string();
        for r in st["db"]["albums"].as_array_mut().unwrap() {
            if r["id"] == id_of(&dir) {
                r["sample_track"] = json!(format!("<LIB>/{dir}/{nfc_file}"));
                r["dir_artist"] = json!(dir_artist);
            }
        }
        add_sidecar(st, &dir); // sidecar rewritten
    }
}

fn d_case_only_file(_: &J, i: usize, st: &mut J) {
    const DIR: &str = "Aurora Fields - Case Files [FLAC]";
    if i == 1 {
        // Refused by the Python app; same-file targets now rename.
        success(st, 2, None, &id_of(DIR));
        st["events"] = renamed_event(&id_of(DIR), &id_of(DIR));
        for (old, new) in [
            ("01 - one.flac", "01 - One.flac"),
            ("02 - two.flac", "02 - Two.flac"),
        ] {
            tree_rename(st, &format!("{DIR}/{old}"), &format!("{DIR}/{new}"));
            db_rename_file(st, DIR, old, new, &[]);
        }
    }
}

fn d_targets_exist(_: &J, i: usize, st: &mut J) {
    if i >= 4 {
        add_sidecar(st, "Mono Harbor - Taken [FLAC]"); // sidecar rewritten after the swap
    }
}

fn d_dir_then_file_refused(sc: &J, i: usize, st: &mut J) {
    if i >= 1 {
        // The collision is found before the dir rename: same
        // message, but no event and nothing changed.
        st["events"] = json!([]);
        restore_album(
            sc,
            st,
            "Mono Harbor - Partial",
            Some("Mono Harbor - Partial [FLAC]"),
        );
    }
}

fn d_blocklist_fk(_: &J, i: usize, st: &mut J) {
    const OLD: &str = "Mono Harbor - Blocked";
    const NEW: &str = "Mono Harbor - Blocked [FLAC]";
    if i >= 1 {
        // The blocklist row moves with the album; the rename succeeds.
        if i == 1 {
            success(st, 0, Some(migration(1, 0, 1, 1)), &id_of(NEW));
            st["events"] = renamed_event(&id_of(OLD), &id_of(NEW));
        }
        move_dir(st, OLD, NEW);
        // The dir-only success path rescans: the new name now parses as an
        // "Artist - Album [FLAC]" directory.
        for r in st["db"]["albums"].as_array_mut().unwrap() {
            if r["id"] == id_of(NEW) {
                r["dir_artist"] = json!("Mono Harbor");
                r["dir_album"] = json!("Blocked");
            }
        }
    }
}

fn d_duplicate_targets(sc: &J, i: usize, st: &mut J) {
    if i == 1 {
        refused(
            st,
            "Refusing rename: more than one file would be renamed to '01 - Same.flac'",
        );
    }
    if i >= 1 {
        restore_album(sc, st, "Mono Harbor - Dupes [FLAC]", None); // refused, nothing renamed
    }
}

fn d_duplicate_targets_folded(sc: &J, i: usize, st: &mut J) {
    if i == 0 {
        refused(
            st,
            "Refusing rename: more than one file would be renamed to '01 - SAME.flac'",
        );
    }
    restore_album(sc, st, "Mono Harbor - Fold Case [FLAC]", None);
    if i == 1 {
        let nfd: String = "01 - Été.flac".nfd().collect();
        refused(
            st,
            &format!("Refusing rename: more than one file would be renamed to '{nfd}'"),
        );
    }
    if i >= 1 {
        restore_album(sc, st, "Mono Harbor - Fold Form [FLAC]", None);
    }
}

/// In trash dir `dir`, the stamped name keeps the copy it got first
/// and later copies go to `<stamp>-2`, `-3`, ... (`copies[k]` = text of the
/// k-th copy trashed under that stamp).
fn trash_counter(st: &mut J, dir: &str, stem: &str, stamp: &str, ext: &str, copies: &[&str]) {
    let t = st["trash"].as_array_mut().unwrap();
    t.retain(|e| !s(&e["path"]).starts_with(&format!("{dir}/{stem}__{stamp}")));
    for (k, text) in copies.iter().enumerate() {
        let suffix = if k == 0 {
            String::new()
        } else {
            format!("-{}", k + 1)
        };
        t.push(file_entry(
            &format!("{dir}/{stem}__{stamp}{suffix}{ext}"),
            text.as_bytes(),
            true,
        ));
    }
}

fn d_trash(_: &J, i: usize, st: &mut J) {
    const DIR: &str = "Mono Harbor - Trash [FLAC]";
    if i == 4 {
        st["result"]["ok"] = json!(format!("<TRASH>/{DIR}/dup__2026-05-05T16-30-46-2.flac"));
    }
    if i >= 4 && st.get("trash").is_some() {
        trash_counter(
            st,
            DIR,
            "dup",
            "2026-05-05T16-30-46",
            ".flac",
            &["copy 2\n", "copy 3\n"],
        );
    }
}

fn d_trash_burst(_: &J, i: usize, st: &mut J) {
    const DIR: &str = "Mono Harbor - Burst [FLAC]";
    let stamp = "2026-05-05T16-30-45";
    let copies: &[&str] = match i {
        4 | 5 => &["copy 2\n", "copy 3\n"],
        6 => &["copy 2\n", "copy 3\n", "copy 4\n"],
        _ => return,
    };
    if i == 4 || i == 6 {
        let n = copies.len();
        st["result"]["ok"] = json!(format!("<TRASH>/{DIR}/x__{stamp}-{n}.flac"));
    }
    if st.get("trash").is_some() {
        trash_counter(st, DIR, "x", stamp, ".flac", copies);
    }
}

fn d_fingerprint_at_target(_: &J, i: usize, st: &mut J) {
    const DIR: &str = "Mono Harbor - Prints [FLAC]";
    if i == 1 {
        // The moved row replaces the stale row under the new name;
        // a stale row whose file had no row of its own stays.
        let rows = st["db"]["track_fingerprints"].as_array_mut().unwrap();
        rows.retain(|r| !(r["album_id"] == id_of(DIR) && r["filename"] == "01 - A.flac"));
        db_rename_file(st, DIR, "1 a.flac", "01 - A.flac", &["track_fingerprints"]);
    }
}
