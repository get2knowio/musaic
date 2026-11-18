//! `POST /api/checks/multi-disc-layout/findings/{id}/convert` over a
//! library of generated FLACs in a temp folder: a set kept in one folder is
//! converted exactly as its preview shows (tags, names, TRACKTOTAL, no disc
//! tags, rows and the track-tag sidecar under the new names, the folder
//! renamed and re-keyed, the record written with old and new values), name
//! collisions inside the folder, interruptions at every step and the
//! Convert that finishes the job, and every refusal (which changes
//! nothing).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{call, no_key_fingerprinting};
use musaic_api::{AppState, Network, router};
use musaic_events::{Event, Subscriber, Topic};
use musaic_services::multi_disc_convert::{StopAt, convert};
use serde_json::{Value, json};

const CHECK: &str = "multi-disc-layout";

fn flac_bytes(tags: &[(&str, String)]) -> Vec<u8> {
    let mut si = Vec::new();
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&4096u16.to_be_bytes());
    si.extend_from_slice(&[0; 6]);
    let packed: u64 = (44100u64 << 44) | (1 << 41) | (15 << 36) | 44100;
    si.extend_from_slice(&packed.to_be_bytes());
    si.extend_from_slice(&[0; 16]);
    let vendor = b"multi-disc-convert-test";
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
    out.push(0x04);
    out.extend_from_slice(&(vc.len() as u32).to_be_bytes()[1..]);
    out.extend_from_slice(&vc);
    // Padding, so a tag write fits in place.
    out.push(0x81);
    out.extend_from_slice(&1024u32.to_be_bytes()[1..]);
    out.extend_from_slice(&[0; 1024]);
    out
}

/// One file to write: its name and the tags beyond the album's own.
struct Track {
    name: String,
    tags: Vec<(&'static str, String)>,
}

fn track(name: &str, title: &str, tracknumber: &str, disc: Option<i64>) -> Track {
    let mut tags = vec![
        ("TITLE", title.to_string()),
        ("TRACKNUMBER", tracknumber.to_string()),
    ];
    if let Some(d) = disc {
        tags.push(("DISCNUMBER", d.to_string()));
        tags.push(("DISCTOTAL", "2".to_string()));
    }
    Track {
        name: name.to_string(),
        tags,
    }
}

fn write_album(dir: &Path, tracks: &[Track]) {
    std::fs::create_dir_all(dir).unwrap();
    for t in tracks {
        let mut tags = vec![
            ("ARTIST", "Band".to_string()),
            ("ALBUMARTIST", "Band".to_string()),
            ("ALBUM", "Record".to_string()),
            ("DATE", "1999".to_string()),
        ];
        tags.extend(t.tags.iter().cloned());
        std::fs::write(dir.join(&t.name), flac_bytes(&tags)).unwrap();
    }
}

/// Two discs of 3 and 2 tracks, numbered per disc, DISCNUMBER tags,
/// track-only names.
fn two_discs() -> Vec<Track> {
    let mut v = Vec::new();
    for k in 1..=3 {
        let mut t = track(
            &format!("{k:02} - A{k}.flac"),
            &format!("A{k}"),
            &k.to_string(),
            Some(1),
        );
        t.tags.push(("TRACKTOTAL", "3".into()));
        v.push(t);
    }
    for k in 1..=2 {
        let mut t = track(
            &format!("{k:02} - B{k}.flac"),
            &format!("B{k}"),
            &k.to_string(),
            Some(2),
        );
        t.tags.push(("TRACKTOTAL", "2".into()));
        v.push(t);
    }
    v
}

/// The numbering tags of a file, as the record spells them.
fn numbering(path: &Path) -> BTreeMap<String, Vec<String>> {
    let meta = musaic_scan::flacmeta::read(path).unwrap();
    [
        "TRACKNUMBER",
        "TRACKTOTAL",
        "TOTALTRACKS",
        "DISCNUMBER",
        "DISCTOTAL",
        "TOTALDISCS",
    ]
    .iter()
    .map(|k| {
        (
            k.to_string(),
            meta.values(&k.to_lowercase()).map(str::to_string).collect(),
        )
    })
    .filter(|(_, v): &(String, Vec<String>)| !v.is_empty())
    .collect()
}

/// Every file of a folder with its numbering tags (FLACs) or its bytes.
fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if name.to_lowercase().ends_with(".flac") && p.is_file() {
            out.insert(name, format!("{:?}", numbering(&p)));
        } else if p.is_file() {
            out.insert(
                name,
                String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned(),
            );
        } else {
            out.insert(name, "<dir>".into());
        }
    }
    out
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    v.sort();
    v
}

struct World {
    _tmp: tempfile::TempDir,
    lib: PathBuf,
    cfg: PathBuf,
    st: AppState,
    app: axum::Router,
    events: Arc<Mutex<Vec<String>>>,
}

impl World {
    fn new() -> World {
        World::with_library_in(None)
    }

    /// The library under `lib_root` (another volume) instead of the temp
    /// dir that holds the config.
    fn with_library_in(lib_root: Option<&Path>) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = musaic_testdata::canonical(tmp.path());
        let cfg = root.join("config");
        let lib = match lib_root {
            Some(r) => musaic_testdata::canonical(r).join("library"),
            None => root.join("library"),
        };
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        let st = AppState::builder(&cfg)
            .secrets(Arc::new(musaic_secrets::MemoryStore::new()))
            .network(Network::Offline)
            .fingerprinting(no_key_fingerprinting())
            .frontend_dir(None)
            .subscribers(false)
            .auto_snapshot(false)
            .build()
            .unwrap();
        let events: Arc<Mutex<Vec<String>>> = Arc::default();
        let e2 = Arc::clone(&events);
        st.bus().register(
            Topic::Event,
            Subscriber::from_fn(move |ev| {
                let s = match ev {
                    Event::AlbumRenamed {
                        old_album_id,
                        new_album_id,
                    } if old_album_id == new_album_id => "AlbumRenamed(files)".to_string(),
                    Event::AlbumRenamed { .. } => "AlbumRenamed(folder)".to_string(),
                    other => other.name().to_string(),
                };
                e2.lock().unwrap().push(s);
                Ok(())
            }),
        );
        let app = router(st.clone());
        World {
            _tmp: tmp,
            lib,
            cfg,
            st,
            app,
            events,
        }
    }

    async fn scan(&self) {
        let r = call(
            &self.app,
            "POST",
            "/api/config",
            Some(&json!({"library_dir": self.lib.to_string_lossy()})),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        let r = call(&self.app, "GET", "/api/scan/stream", None).await;
        assert!(r.text().contains("event: done"), "{}", r.text());
    }

    async fn run(&self, check: &str) {
        let r = call(
            &self.app,
            "POST",
            &format!("/api/checks/{check}/run"),
            Some(&json!({"scope": "all"})),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    }

    async fn open_findings(&self) -> Vec<Value> {
        let r = call(
            &self.app,
            "GET",
            &format!("/api/checks/findings?check_id={CHECK}"),
            None,
        )
        .await;
        r.json()["findings"].as_array().unwrap().clone()
    }

    async fn only_finding(&self) -> i64 {
        self.run(CHECK).await;
        let f = self.open_findings().await;
        assert_eq!(f.len(), 1, "{f:?}");
        f[0]["id"].as_i64().unwrap()
    }

    async fn preview(&self, id: i64) -> Value {
        let r = call(
            &self.app,
            "GET",
            &format!("/api/checks/{CHECK}/findings/{id}/conversion-preview"),
            None,
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        r.json()
    }

    async fn post(&self, id: i64, digest: &str) -> (u16, Value) {
        let r = call(
            &self.app,
            "POST",
            &format!("/api/checks/{CHECK}/findings/{id}/convert"),
            Some(&json!({"digest": digest})),
        )
        .await;
        (r.status, r.json())
    }

    /// Preview, then Convert with its digest; must succeed.
    async fn convert_ok(&self, id: i64) -> Value {
        let p = self.preview(id).await;
        assert_eq!(p["conversion_available"], json!(true), "{p}");
        let (status, body) = self.post(id, p["digest"].as_str().unwrap()).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["ok"], json!(true), "{body}");
        body
    }

    /// Convert through the service with a stop point (as a crash there).
    fn convert_stopping(&self, id: i64, digest: &str, stop: StopAt) -> Value {
        let mut conn = self.conn();
        self.st
            .app()
            .with_convert_env(&mut conn, |env| {
                env.stop_at = Some(stop);
                convert(env, id, digest)
            })
            .unwrap()
    }

    fn conn(&self) -> rusqlite::Connection {
        musaic_db::open_connection(self.st.db_path()).unwrap()
    }

    fn album_ids(&self) -> Vec<String> {
        let c = self.conn();
        let mut s = c.prepare("SELECT id FROM albums ORDER BY id").unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    }

    fn records(&self) -> Vec<Value> {
        let dir = self.cfg.join("conversions");
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.ends_with(".json") && !n.starts_with('.') {
                    out.push(
                        serde_json::from_str(&std::fs::read_to_string(e.path()).unwrap()).unwrap(),
                    );
                }
            }
        }
        out
    }

    fn events(&self) -> Vec<String> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    async fn set_setting(&self, key: &str, value: &str) {
        let r = call(
            &self.app,
            "PATCH",
            "/api/settings",
            Some(&json!({ key: value })),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    }
}

fn count(events: &[String], name: &str) -> usize {
    events.iter().filter(|e| *e == name).count()
}

// ---------------------------------------------------------------- the conversion

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_discs_in_one_folder_convert_with_their_rows_and_sidecar() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    write_album(&dir, &two_discs());
    w.scan().await;
    let album = w.album_ids()[0].clone();
    // Rows keyed by file name: a user tag (which writes the sidecar), a
    // fingerprint and a decode reject.
    let r = call(
        &w.app,
        "POST",
        &format!("/api/albums/{album}/tracks/01%20-%20B1.flac/tags"),
        Some(&json!({"tag": "favourite"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    {
        let c = w.conn();
        c.execute(
            "INSERT INTO track_fingerprints (album_id, filename, fingerprint, duration_s, last_computed_at) \
             VALUES (?1, '02 - B2.flac', 'fp', 1.0, '2026-01-01T00:00:00Z')",
            [&album],
        )
        .unwrap();
        c.execute(
            "INSERT INTO fingerprint_rejects (album_id, filename, file_signature, fingerprint_impl, rejected_at) \
             VALUES (?1, '01 - B1.flac', 'sig', 'impl', '2026-01-01T00:00:00Z')",
            [&album],
        )
        .unwrap();
    }
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let planned: Vec<(String, String)> = p["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["filename"].as_str().unwrap().to_string(),
                t["new_filename"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    w.events();

    let body = w.convert_ok(id).await;
    assert_eq!(body["folder"]["outcome"], json!("unchanged"));
    assert_eq!(body["album_id"], json!(album));

    // Exactly the names the preview showed.
    let want: Vec<String> = planned.iter().map(|(_, n)| n.clone()).collect();
    assert_eq!(
        names(&dir),
        vec![
            "01 - A1.flac",
            "02 - A2.flac",
            "03 - A3.flac",
            "04 - B1.flac",
            "05 - B2.flac"
        ]
    );
    let mut sorted = want.clone();
    sorted.sort();
    assert_eq!(names(&dir), sorted);
    for (k, n) in names(&dir).iter().enumerate() {
        let tags = numbering(&dir.join(n));
        assert_eq!(
            tags,
            BTreeMap::from([
                ("TRACKNUMBER".to_string(), vec![(k + 1).to_string()]),
                ("TRACKTOTAL".to_string(), vec!["5".to_string()]),
            ]),
            "{n}"
        );
    }
    // Everything else in the files is untouched.
    let meta = musaic_scan::flacmeta::read(&dir.join("04 - B1.flac")).unwrap();
    assert_eq!(meta.values("title").collect::<Vec<_>>(), ["B1"]);
    assert_eq!(meta.values("album").collect::<Vec<_>>(), ["Record"]);

    // The rows follow the files.
    let c = w.conn();
    let row = |sql: &str| -> Vec<String> {
        let mut s = c.prepare(sql).unwrap();
        s.query_map([&album], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    };
    assert_eq!(
        row("SELECT filename FROM track_tags WHERE album_id = ?1"),
        ["04 - B1.flac"]
    );
    assert_eq!(
        row("SELECT filename FROM track_fingerprints WHERE album_id = ?1"),
        ["05 - B2.flac"]
    );
    assert_eq!(
        row("SELECT filename FROM fingerprint_rejects WHERE album_id = ?1"),
        ["04 - B1.flac"]
    );
    // The track-tag sidecar names the new file.
    let sidecar = std::fs::read_to_string(dir.join(".media-manager-tags.json")).unwrap();
    assert!(sidecar.contains("04 - B1.flac"), "{sidecar}");
    assert!(!sidecar.contains("01 - B1.flac"), "{sidecar}");
    // The album row: no disc numbers left per file.
    let discs: Option<String> = c
        .query_row(
            "SELECT track_discs_json FROM albums WHERE id = ?1",
            [&album],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(discs.as_deref(), Some("[null, null, null, null, null]"));

    // Events: one file-rename event, one tag change, one batch end.
    let ev = w.events();
    assert_eq!(count(&ev, "AlbumRenamed(files)"), 1, "{ev:?}");
    assert_eq!(count(&ev, "AlbumTagsChanged"), 1, "{ev:?}");
    assert_eq!(count(&ev, "BulkBatchCompleted"), 1, "{ev:?}");
    assert_eq!(count(&ev, "AlbumRenamed(folder)"), 0, "{ev:?}");

    // The record: the whole plan with old and new values, one attempt.
    let recs = w.records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert!(r["finished_at"].is_string(), "{r}");
    assert_eq!(r["album_id_before"], json!(album));
    assert_eq!(r["album_id_after"], json!(album));
    assert_eq!(r["track_total"], json!(5));
    assert_eq!(r["attempts"].as_array().unwrap().len(), 1);
    assert_eq!(r["attempts"][0]["outcome"], json!("completed"));
    let b1 = r["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["old_name"] == json!("01 - B1.flac"))
        .unwrap();
    assert_eq!(b1["new_name"], json!("04 - B1.flac"));
    assert_eq!(b1["disc"], json!(2));
    assert_eq!(b1["old_tags"]["DISCNUMBER"], json!(["2"]));
    assert_eq!(b1["old_tags"]["DISCTOTAL"], json!(["2"]));
    assert_eq!(b1["old_tags"]["TRACKNUMBER"], json!(["1"]));
    assert_eq!(b1["old_tags"]["TRACKTOTAL"], json!(["2"]));
    assert_eq!(b1["new_tags"]["TRACKNUMBER"], json!(["4"]));
    assert_eq!(b1["new_tags"]["TRACKTOTAL"], json!(["5"]));
    assert_eq!(b1["new_tags"]["DISCNUMBER"], json!([]));
    let steps: Vec<&str> = r["attempts"][0]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["step"].as_str().unwrap())
        .collect();
    assert_eq!(steps, ["rename_files", "write_tags"]);

    // The rerun (the sweep would run it) resolves the finding.
    w.run(CHECK).await;
    assert!(w.open_findings().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disc_track_template_and_disc_track_numbers_without_discnumber() {
    let w = World::new();
    w.set_setting(
        "filename_template_single_artist",
        "{disc}-{tracknumber:02} {title}",
    )
    .await;
    let dir = w.lib.join("Band - Record [FLAC]");
    // `2-05` style TRACKNUMBERs and no DISCNUMBER at all.
    let tracks = vec![
        track("1-01 A.flac", "A", "1-01", None),
        track("1-02 B.flac", "B", "1-02", None),
        track("2-01 C.flac", "C", "2-01", None),
        track("2-02 D.flac", "D", "2-02", None),
    ];
    write_album(&dir, &tracks);
    w.scan().await;
    let id = w.only_finding().await;
    w.convert_ok(id).await;
    // The set has no disc any more, so the disc part of the template
    // renders empty: the track alone.
    assert_eq!(
        names(&dir),
        ["01 A.flac", "02 B.flac", "03 C.flac", "04 D.flac"]
    );
    assert_eq!(
        numbering(&dir.join("03 C.flac")),
        BTreeMap::from([
            ("TRACKNUMBER".to_string(), vec!["3".to_string()]),
            ("TRACKTOTAL".to_string(), vec!["4".to_string()]),
        ])
    );
    w.run(CHECK).await;
    assert!(w.open_findings().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_named_as_disc_one_holding_both_discs_is_renamed_and_rekeyed() {
    let w = World::new();
    let dir = w.lib.join("Band - Record (disc 1 of 2) [FLAC]");
    write_album(&dir, &two_discs());
    w.scan().await;
    let old = w.album_ids()[0].clone();
    let r = call(
        &w.app,
        "POST",
        &format!("/api/albums/{old}/tracks/02%20-%20B2.flac/tags"),
        Some(&json!({"tag": "live"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["target_folder"]["new"], json!("Band - Record [FLAC]"));
    w.events();
    let body = w.convert_ok(id).await;
    assert_eq!(body["folder"]["outcome"], json!("renamed"));
    let new_dir = w.lib.join("Band - Record [FLAC]");
    assert!(!dir.exists());
    assert_eq!(names(&new_dir).len(), 5);
    let ids = w.album_ids();
    assert_eq!(ids.len(), 1);
    assert_ne!(ids[0], old);
    assert_eq!(body["album_id"], json!(ids[0]));
    // The album's data followed the re-key, under the new file name.
    let c = w.conn();
    let tagged: Vec<(String, String)> = {
        let mut s = c
            .prepare("SELECT album_id, filename FROM track_tags")
            .unwrap();
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    assert_eq!(tagged, [(ids[0].clone(), "05 - B2.flac".to_string())]);
    let ev = w.events();
    assert_eq!(count(&ev, "AlbumRenamed(folder)"), 1, "{ev:?}");
    assert_eq!(count(&ev, "AlbumRenamed(files)"), 1, "{ev:?}");
    assert_eq!(count(&ev, "AlbumTagsChanged"), 1, "{ev:?}");
    assert_eq!(count(&ev, "BulkBatchCompleted"), 1, "{ev:?}");
    let rec = &w.records()[0];
    assert_eq!(rec["album_id_before"], json!(old));
    assert_eq!(rec["album_id_after"], json!(ids[0]));
    assert_eq!(
        rec["folder_name_before"],
        json!("Band - Record (disc 1 of 2) [FLAC]")
    );
    assert_eq!(rec["folder_name_after"], json!("Band - Record [FLAC]"));
    let steps: Vec<&str> = rec["attempts"][0]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["step"].as_str().unwrap())
        .collect();
    assert_eq!(steps, ["rename_files", "write_tags", "rename_folder"]);
    w.run(CHECK).await;
    assert!(w.open_findings().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn already_continuous_numbering_only_loses_its_disc_tags() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    let tracks = vec![
        track("01 - A.flac", "A", "1", Some(1)),
        track("02 - B.flac", "B", "2", Some(1)),
        track("03 - C.flac", "C", "3", Some(2)),
    ];
    write_album(&dir, &tracks);
    w.scan().await;
    let id = w.only_finding().await;
    assert_eq!(w.preview(id).await["numbering"], json!("continuous"));
    w.events();
    let body = w.convert_ok(id).await;
    assert!(
        body["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["rename"] == json!("already") && f["tags"] == json!("written")),
        "{body}"
    );
    assert_eq!(names(&dir), ["01 - A.flac", "02 - B.flac", "03 - C.flac"]);
    assert_eq!(
        numbering(&dir.join("03 - C.flac")),
        BTreeMap::from([
            ("TRACKNUMBER".to_string(), vec!["3".to_string()]),
            ("TRACKTOTAL".to_string(), vec!["3".to_string()]),
        ])
    );
    let ev = w.events();
    assert_eq!(count(&ev, "AlbumRenamed(files)"), 0, "{ev:?}");
    assert_eq!(count(&ev, "AlbumTagsChanged"), 1, "{ev:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn totaltracks_is_kept_in_step_where_a_file_has_it() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    let mut tracks = two_discs();
    tracks[3].tags.push(("TOTALTRACKS", "2".into()));
    tracks[3].tags.push(("TOTALDISCS", "2".into()));
    write_album(&dir, &tracks);
    w.scan().await;
    let id = w.only_finding().await;
    w.convert_ok(id).await;
    assert_eq!(
        numbering(&dir.join("04 - B1.flac")),
        BTreeMap::from([
            ("TRACKNUMBER".to_string(), vec!["4".to_string()]),
            ("TRACKTOTAL".to_string(), vec!["5".to_string()]),
            ("TOTALTRACKS".to_string(), vec!["5".to_string()]),
        ])
    );
    // Not added where it was not.
    assert!(!numbering(&dir.join("05 - B2.flac")).contains_key("TOTALTRACKS"));
}

// ---------------------------------------------------------------- collisions

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn names_that_swap_between_discs_never_overwrite_a_file() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    // Disc 2's files hold the names disc 1's files are to get.
    let tracks = vec![
        track("1-01 - Song.flac", "Song", "1", Some(1)),
        track("1-02 - Song.flac", "Song", "2", Some(1)),
        track("01 - Song.flac", "Song", "1", Some(2)),
        track("02 - Song.flac", "Song", "2", Some(2)),
    ];
    write_album(&dir, &tracks);
    w.scan().await;
    let id = w.only_finding().await;
    w.convert_ok(id).await;
    assert_eq!(
        names(&dir),
        [
            "01 - Song.flac",
            "02 - Song.flac",
            "03 - Song.flac",
            "04 - Song.flac"
        ]
    );
    // Each file kept its own audio identity: disc 2's first track is now 3.
    for (n, want) in [
        ("01 - Song.flac", "1"),
        ("02 - Song.flac", "2"),
        ("03 - Song.flac", "3"),
        ("04 - Song.flac", "4"),
    ] {
        assert_eq!(numbering(&dir.join(n))["TRACKNUMBER"], [want]);
    }
    // No temporary name left behind.
    assert!(
        std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains(".renaming")),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn case_only_and_normalisation_only_names() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    let tracks = vec![
        // Case only: the template capitalises the title.
        track("01 - song.flac", "Song", "1", Some(1)),
        // Already the planned name, spelled decomposed.
        track("02 - Cafe\u{301}.flac", "Caf\u{e9}", "2", Some(1)),
        track("01 - Other.flac", "Other", "1", Some(2)),
    ];
    write_album(&dir, &tracks);
    w.scan().await;
    let id = w.only_finding().await;
    let body = w.convert_ok(id).await;
    let listed = names(&dir);
    assert!(listed.contains(&"01 - Song.flac".to_string()), "{listed:?}");
    assert!(
        !listed.contains(&"01 - song.flac".to_string()),
        "{listed:?}"
    );
    assert!(
        listed.contains(&"03 - Other.flac".to_string()),
        "{listed:?}"
    );
    assert_eq!(listed.len(), 3, "{listed:?}");
    let cafe = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["new_filename"] == json!("02 - Caf\u{e9}.flac"))
        .unwrap();
    assert_eq!(cafe["rename"], json!("already"), "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_non_audio_entry_holding_a_target_name_blocks_and_nothing_changes() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    write_album(&dir, &two_discs());
    // A folder that has the name disc 2's first track is to get.
    std::fs::create_dir(dir.join("04 - B1.flac")).unwrap();
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["conversion_available"], json!(false));
    let codes: Vec<&str> = p["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["name_collision"]);
    let before = snapshot(&dir);
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("blocked"));
    assert_eq!(body["blockers"][0]["code"], json!("name_collision"));
    assert_eq!(snapshot(&dir), before);
    assert!(w.records().is_empty());
}

// ---------------------------------------------------------------- interruption

/// The final state of an uninterrupted conversion of [`two_discs`] in a
/// folder named as disc 1 (every step has something to do).
async fn reference() -> BTreeMap<String, String> {
    let w = World::new();
    write_album(
        &w.lib.join("Band - Record (disc 1 of 2) [FLAC]"),
        &two_discs(),
    );
    w.scan().await;
    let id = w.only_finding().await;
    w.convert_ok(id).await;
    snapshot(&w.lib.join("Band - Record [FLAC]"))
}

async fn interrupted_then_finished(stop: StopAt) {
    let want = reference().await;
    let w = World::new();
    let dir = w.lib.join("Band - Record (disc 1 of 2) [FLAC]");
    write_album(&dir, &two_discs());
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let out = w.convert_stopping(id, p["digest"].as_str().unwrap(), stop);
    assert_eq!(out["interrupted"], json!(true), "{out}");

    // A valid library: every file is a readable FLAC under one name, the
    // record says the conversion did not finish.
    let recs = w.records();
    assert_eq!(recs.len(), 1);
    assert!(recs[0]["finished_at"].is_null());
    for n in names(&dir) {
        assert!(musaic_scan::flacmeta::read(&dir.join(&n)).is_ok(), "{n}");
    }
    // A rescan sees the files as they are; the check keeps a finding that
    // leads back to the conversion, whatever disc tags are left.
    w.scan().await;
    w.run(CHECK).await;
    let open = w.open_findings().await;
    assert_eq!(open.len(), 1, "{open:?}");
    let id = open[0]["id"].as_i64().unwrap();
    // The preview re-derives the remaining plan from the disk and the
    // record: the same numbers and names as before.
    let p2 = w.preview(id).await;
    assert_eq!(p2["conversion_available"], json!(true), "{p2}");
    assert!(p2["resume"]["record"].is_string(), "{p2}");
    let plan = |p: &Value| -> Vec<(Value, Value)> {
        p["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["new_track"].clone(), t["new_filename"].clone()))
            .collect()
    };
    assert_eq!(plan(&p2), plan(&p));
    assert_eq!(p2["target_folder"]["new"], p["target_folder"]["new"]);

    // The second Convert finishes; the result equals the uninterrupted one.
    w.convert_ok(id).await;
    assert_eq!(snapshot(&w.lib.join("Band - Record [FLAC]")), want);
    let recs = w.records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert!(r["finished_at"].is_string(), "{r}");
    let attempts = r["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "{r}");
    assert!(attempts[0]["outcome"].is_null(), "{r}");
    assert_eq!(attempts[1]["outcome"], json!("completed"));
    w.run(CHECK).await;
    assert!(w.open_findings().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_mid_renames_then_finished() {
    interrupted_then_finished(StopAt::RenamedFiles(1)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_renames_before_tags_then_finished() {
    interrupted_then_finished(StopAt::TaggedFiles(0)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_some_files_tagged_then_finished() {
    // Disc 1 entirely and nothing of disc 2: no disc tag of disc 1 is left
    // to detect the set by.
    interrupted_then_finished(StopAt::TaggedFiles(3)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_one_file_of_disc_two_tagged_then_finished() {
    interrupted_then_finished(StopAt::TaggedFiles(4)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_before_the_folder_rename_then_finished() {
    interrupted_then_finished(StopAt::BeforeFolderRename).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_changed_after_an_interruption_is_refused() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    write_album(&dir, &two_discs());
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    w.convert_stopping(id, p["digest"].as_str().unwrap(), StopAt::TaggedFiles(2));
    // Someone retags a written file by hand.
    let path = dir.join("02 - A2.flac");
    let mut f = musaic_tags::FlacFile::open(&path).unwrap();
    f.tags_mut().set("TRACKNUMBER", "7");
    f.save().unwrap();
    w.scan().await;
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    let p2 = w.preview(id).await;
    assert_eq!(p2["conversion_available"], json!(false), "{p2}");
    let codes: Vec<&str> = p2["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"resume_mismatch"), "{codes:?}");
    let before = snapshot(&dir);
    let (status, _) = w.post(id, p2["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409);
    assert_eq!(snapshot(&dir), before);
}

/// Real failures, not a stop point: a folder that cannot be written stops
/// the renames (nothing is renamed or tagged), a read-only file stops its
/// own tag write (the others are written, the folder keeps its name). Each
/// time the dialog's answer names the file and the reason, and once the
/// cause is gone a second Convert finishes.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_failures_stop_cleanly_and_a_second_convert_finishes() {
    use std::os::unix::fs::PermissionsExt;
    let mode =
        |p: &Path, m: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    let want = reference().await;
    let w = World::new();
    let dir = w.lib.join("Band - Record (disc 1 of 2) [FLAC]");
    write_album(&dir, &two_discs());
    w.scan().await;
    let id = w.only_finding().await;

    // The folder cannot be written: no rename can happen.
    mode(&dir, 0o555);
    if std::fs::write(dir.join(".probe"), b"x").is_ok() {
        // Running with the rights to write anyway (root): nothing to test.
        let _ = std::fs::remove_file(dir.join(".probe"));
        mode(&dir, 0o755);
        return;
    }
    let before = snapshot(&dir);
    let p = w.preview(id).await;
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    mode(&dir, 0o755);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], json!(false), "{body}");
    let failed: Vec<&Value> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rename"] == json!("failed"))
        .collect();
    assert!(!failed.is_empty(), "{body}");
    assert!(
        failed
            .iter()
            .all(|f| f["reason_code"] == json!("permission_denied")),
        "{body}"
    );
    assert!(
        body["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["tags"] == json!("not_reached"))
    );
    assert_eq!(snapshot(&dir), before);

    // A read-only file: its tags cannot be written, the others are.
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    let ro = dir.join("02 - B2.flac");
    mode(&ro, 0o444);
    let p = w.preview(id).await;
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 200, "{body}");
    if body["ok"] == json!(true) {
        // The tag writer replaced the file instead of writing it in place,
        // which a read-only file allows: nothing more to see here.
        return;
    }
    let b2 = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["new_filename"] == json!("05 - B2.flac"))
        .unwrap();
    assert_eq!(b2["tags"], json!("failed"), "{body}");
    assert_eq!(b2["reason_code"], json!("permission_denied"), "{body}");
    assert_eq!(body["folder"]["outcome"], json!("not_reached"), "{body}");
    assert!(dir.exists());
    mode(&dir.join("05 - B2.flac"), 0o644);
    w.scan().await;
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    w.convert_ok(id).await;
    assert_eq!(snapshot(&w.lib.join("Band - Record [FLAC]")), want);
    let attempts = w.records()[0]["attempts"].as_array().unwrap().len();
    assert_eq!(attempts, 3);
}

// ---------------------------------------------------------------- refusals

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refusals_change_nothing() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    write_album(&dir, &two_discs());
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let digest = p["digest"].as_str().unwrap().to_string();
    let before = snapshot(&dir);
    let album = w.album_ids()[0].clone();

    // A digest that is not the plan's.
    let (status, body) = w.post(id, "not-the-digest").await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("plan_changed"));
    assert_eq!(body["digest"], json!(digest));

    // A plan that changed since the preview: a title edited by hand.
    {
        let mut f = musaic_tags::FlacFile::open(&dir.join("01 - A1.flac")).unwrap();
        f.tags_mut().set("TITLE", "A1 (edit)");
        f.save().unwrap();
    }
    let (status, body) = w.post(id, &digest).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("plan_changed"));
    let before = {
        let mut b = before;
        b.insert(
            "01 - A1.flac".into(),
            snapshot(&dir)["01 - A1.flac"].clone(),
        );
        b
    };
    let digest = w.preview(id).await["digest"].as_str().unwrap().to_string();

    // A scan in progress (its hold taken here stands in for one).
    {
        let hold = w.st.scan().hold_layout().unwrap();
        let (status, body) = w.post(id, &digest).await;
        assert_eq!(status, 409, "{body}");
        assert_eq!(body["reason_code"], json!("scan_running"));
        drop(hold);
    }

    // A restore in progress.
    {
        let restore = w.st.gate().begin_restore().unwrap();
        let (status, _) = w.post(id, &digest).await;
        assert_eq!(status, 409);
        drop(restore);
    }

    // A job queued on the album.
    w.conn()
        .execute(
            "INSERT INTO ai_jobs (id, album_id, album_path, job_type, status, created_at) \
             VALUES ('j1', ?1, '', 'metadata', 'queued', '2026-01-01T00:00:00Z')",
            [&album],
        )
        .unwrap();
    let p = w.preview(id).await;
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("blocked"));
    assert_eq!(body["blockers"][0]["code"], json!("job_active"));
    w.conn().execute("DELETE FROM ai_jobs", []).unwrap();

    // A Missing album.
    w.conn()
        .execute(
            "UPDATE albums SET missing_since = '2026-01-01T00:00:00Z' WHERE id = ?1",
            [&album],
        )
        .unwrap();
    let (status, body) = w.post(id, &digest).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("album_missing"));
    w.conn()
        .execute(
            "UPDATE albums SET missing_since = NULL WHERE id = ?1",
            [&album],
        )
        .unwrap();

    // Another check's finding, and an unknown one.
    let (status, _) = w.post(999_999, &digest).await;
    assert_eq!(status, 404);
    // A body without the digest.
    let r = call(
        &w.app,
        "POST",
        &format!("/api/checks/{CHECK}/findings/{id}/convert"),
        Some(&json!({})),
    )
    .await;
    assert_eq!(r.status, 422, "{}", r.text());

    assert_eq!(snapshot(&dir), before);
    assert!(w.records().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_in_several_folders_is_not_converted_yet() {
    let w = World::new();
    for d in 1..=2 {
        let dir = w.lib.join(format!("Band - Record (disc {d} of 2) [FLAC]"));
        write_album(
            &dir,
            &[track(
                &format!("01 - T{d}.flac"),
                &format!("T{d}"),
                "1",
                Some(d),
            )],
        );
    }
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["state"], json!("folders"));
    assert_eq!(p["conversion_available"], json!(false));
    let before = snapshot(&w.lib.join("Band - Record (disc 2 of 2) [FLAC]"));
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("not_available"));
    assert_eq!(
        snapshot(&w.lib.join("Band - Record (disc 2 of 2) [FLAC]")),
        before
    );
    assert!(w.records().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incomplete_set_never_converts() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    // Discs 1 and 3 in one folder: disc 2 is missing.
    let tracks = vec![
        track("01 - A.flac", "A", "1", Some(1)),
        track("01 - C.flac", "C", "1", Some(3)),
    ];
    write_album(&dir, &tracks);
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["conversion_available"], json!(false));
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["blockers"][0]["code"], json!("disc_gap"));
    let one = w.lib.join("Band - Other (disc 2 of 3) [FLAC]");
    write_album(&one, &[track("01 - X.flac", "X", "1", Some(2))]);
    w.scan().await;
    w.run(CHECK).await;
    let inc = w
        .open_findings()
        .await
        .into_iter()
        .find(|f| f["detail"]["kind"] == json!("incomplete"))
        .unwrap();
    let iid = inc["id"].as_i64().unwrap();
    let p = w.preview(iid).await;
    let (status, body) = w.post(iid, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("not_convertible"));
    assert!(w.records().is_empty());
}

// ---------------------------------------------------------------- the general apply

/// TRACKTOTAL and the disc tags are not fields the general apply knows:
/// a suggestion for them is ignored and a removal of them refused, so only
/// a conversion writes them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_general_apply_cannot_set_or_remove_the_disc_fields() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    write_album(&dir, &two_discs());
    w.scan().await;
    let album = w.album_ids()[0].clone();
    let before = snapshot(&dir);
    let r = call(
        &w.app,
        "POST",
        &format!("/api/albums/{album}/apply/stream"),
        Some(&json!({
            "suggestions": {"disctotal": "9", "tracktotal": "9"},
            "track_suggestions": {"01 - B1.flac": {
                "tracktotal": "9", "discnumber": "9", "disctotal": "9", "totaldiscs": "9", "totaltracks": "9"
            }},
        })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(snapshot(&dir), before);
    for field in [
        "discnumber",
        "disctotal",
        "tracktotal",
        "totaldiscs",
        "tracknumber",
    ] {
        let r = call(
            &w.app,
            "POST",
            &format!("/api/albums/{album}/apply/stream"),
            Some(&json!({"track_clear": {"01 - B1.flac": [field]}})),
        )
        .await;
        assert_eq!(r.status, 422, "{field}: {}", r.text());
        let r = call(
            &w.app,
            "POST",
            &format!("/api/albums/{album}/apply/stream"),
            Some(&json!({"clear": [field]})),
        )
        .await;
        assert_eq!(r.status, 422, "{field}: {}", r.text());
    }
    assert_eq!(snapshot(&dir), before);
}

// ---------------------------------------------------------------- Windows

/// A file another program holds open (without sharing delete) cannot be
/// renamed: the rename step fails as a whole, nothing is tagged, and once
/// the file is closed a second Convert finishes.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_held_open_fails_cleanly_and_resumes() {
    use std::os::windows::fs::OpenOptionsExt;
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    write_album(&dir, &two_discs());
    w.scan().await;
    let id = w.only_finding().await;
    let held = std::fs::OpenOptions::new()
        .read(true)
        // Others may read it (as a player allows), not rename or write it;
        // with no sharing at all the preview cannot read it and refuses.
        .share_mode(1)
        .open(dir.join("01 - B1.flac"))
        .unwrap();
    let p = w.preview(id).await;
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], json!(false), "{body}");
    let b1 = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["filename"] == json!("01 - B1.flac"))
        .unwrap();
    assert_eq!(b1["rename"], json!("failed"), "{body}");
    assert!(b1["user_message"].is_string(), "{body}");
    drop(held);
    w.scan().await;
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    w.convert_ok(id).await;
    assert_eq!(names(&dir).len(), 5);
    assert_eq!(numbering(&dir.join("04 - B1.flac"))["TRACKNUMBER"], ["4"]);
    assert_eq!(w.records()[0]["attempts"].as_array().unwrap().len(), 2);
}

// ---------------------------------------------------------------- HFS+

/// An HFS+ disk image mounted under a temp dir, detached when dropped.
#[cfg(target_os = "macos")]
struct HfsVolume {
    mount: PathBuf,
    _dir: tempfile::TempDir,
}

#[cfg(target_os = "macos")]
impl HfsVolume {
    fn new() -> HfsVolume {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("volume.dmg");
        let mount = dir.path().join("mnt");
        std::fs::create_dir(&mount).unwrap();
        let run = |cmd: &mut Command| {
            let out = cmd.output().expect("hdiutil");
            assert!(
                out.status.success(),
                "{cmd:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(Command::new("hdiutil")
            .args(["create", "-quiet", "-fs", "HFS+", "-size", "20m"])
            .args(["-volname", "musaic-test"])
            .arg(&image));
        run(Command::new("hdiutil")
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&mount)
            .arg(&image));
        HfsVolume { mount, _dir: dir }
    }
}

#[cfg(target_os = "macos")]
impl Drop for HfsVolume {
    fn drop(&mut self) {
        use std::process::Command;
        let detach = |force: bool| {
            let mut cmd = Command::new("hdiutil");
            cmd.args(["detach", "-quiet"]).arg(&self.mount);
            if force {
                cmd.arg("-force");
            }
            cmd.status().is_ok_and(|s| s.success())
        };
        if !detach(false) && !detach(true) {
            eprintln!("could not detach {}", self.mount.display());
        }
    }
}

/// On a real HFS+ volume every name is listed decomposed: the files and
/// the folder end under the planned names, and the rows, the new album id
/// and the record follow the spelling the volume lists. Needs `hdiutil`
/// (no root); run by an explicit CI step on macOS.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "mounts an HFS+ disk image; run with --ignored on macOS"]
async fn an_hfs_plus_volume_converts_under_the_listed_spellings() {
    use unicode_normalization::UnicodeNormalization;
    let volume = HfsVolume::new();
    let w = World::with_library_in(Some(&volume.mount));
    let dir = w.lib.join("Band - Caf\u{e9} (disc 1 of 2) [FLAC]");
    let tracks = vec![
        track("01 - Caf\u{e9}.flac", "Caf\u{e9}", "1", Some(1)),
        track("01 - Cr\u{e8}me.flac", "Cr\u{e8}me", "1", Some(2)),
    ];
    write_album(&dir, &tracks);
    w.scan().await;
    let old = w.album_ids()[0].clone();
    // The file as the volume lists it (decomposed), percent-encoded.
    let r = call(
        &w.app,
        "POST",
        &format!("/api/albums/{old}/tracks/01%20-%20Cre%CC%80me.flac/tags"),
        Some(&json!({"tag": "live"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let id = w.only_finding().await;
    let body = w.convert_ok(id).await;
    assert_eq!(body["folder"]["outcome"], json!("renamed"), "{body}");
    let nfd = |s: &str| s.nfd().collect::<String>();
    let new_dir_name = "Band - Caf\u{e9} [FLAC]";
    let listed: Vec<String> = std::fs::read_dir(&w.lib)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(listed, [nfd(new_dir_name)]);
    let new_dir = w.lib.join(nfd(new_dir_name));
    assert_eq!(
        names(&new_dir),
        [nfd("01 - Caf\u{e9}.flac"), nfd("02 - Cr\u{e8}me.flac")]
    );
    assert_eq!(
        numbering(&new_dir.join(nfd("02 - Cr\u{e8}me.flac")))["TRACKNUMBER"],
        ["2"]
    );
    // The id is the one a scan derives from the listing.
    let ids = w.album_ids();
    assert_eq!(ids, [musaic_core::album_id(&new_dir.to_string_lossy())]);
    let c = w.conn();
    let tagged: String = c
        .query_row("SELECT filename FROM track_tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tagged, nfd("02 - Cr\u{e8}me.flac"));
    assert_eq!(w.records()[0]["album_id_after"], json!(ids[0]));
    w.run(CHECK).await;
    assert!(w.open_findings().await.is_empty());
}
