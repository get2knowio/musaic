//! Converting a multi-disc set kept in one folder per disc
//! (`POST /api/checks/multi-disc-layout/findings/{id}/convert` on a
//! `folders` finding), over generated FLACs in temp folders: the files of
//! every disc end in disc 1's folder with continuous numbers, no disc tags
//! and names by the template; one album row holds everything the members
//! had; covers, clutter, sidecars and leftovers as decided; an interruption
//! at every step leaves a library that a scan reads without losing
//! anything and that a second Convert finishes exactly as an uninterrupted
//! run; every refusal changes nothing; the fingerprint pool and the check
//! sweep are kept away while it runs.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request;
use common::{call, no_key_fingerprinting, send};
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
    let vendor = b"multi-disc-gather-test";
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
    out.push(0x81);
    out.extend_from_slice(&1024u32.to_be_bytes()[1..]);
    out.extend_from_slice(&[0; 1024]);
    out
}

/// The discs of a set: per disc, the titles of its tracks.
fn write_disc(dir: &Path, disc: i64, total: i64, titles: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    for (k, title) in titles.iter().enumerate() {
        let n = k + 1;
        let tags = vec![
            ("ARTIST", "Band".to_string()),
            ("ALBUMARTIST", "Band".to_string()),
            ("ALBUM", "Record".to_string()),
            ("DATE", "1999".to_string()),
            ("TITLE", title.to_string()),
            ("TRACKNUMBER", n.to_string()),
            ("TRACKTOTAL", titles.len().to_string()),
            ("DISCNUMBER", disc.to_string()),
            ("DISCTOTAL", total.to_string()),
        ];
        std::fs::write(
            dir.join(format!("{n:02} - {title}.flac")),
            flac_bytes(&tags),
        )
        .unwrap();
    }
}

fn numbering(path: &Path) -> BTreeMap<String, Vec<String>> {
    let meta = musaic_scan::flacmeta::read(path).unwrap();
    [
        "TRACKNUMBER",
        "TRACKTOTAL",
        "TOTALTRACKS",
        "DISCNUMBER",
        "DISCTOTAL",
        "TOTALDISCS",
        "TITLE",
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

/// Every entry under `root`, by path relative to it: a FLAC's tags, another
/// file's bytes, `<dir>` for a folder.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if p.is_dir() {
                out.insert(rel, "<dir>".into());
                walk(root, &p, out);
            } else if rel.to_lowercase().ends_with(".flac") {
                out.insert(rel, format!("{:?}", numbering(&p)));
            } else {
                out.insert(
                    rel,
                    String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
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
                    Event::AlbumRenamed { .. } => "AlbumRenamed(ids)".to_string(),
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

    async fn convert_ok(&self, id: i64) -> Value {
        let p = self.preview(id).await;
        assert_eq!(p["conversion_available"], json!(true), "{p}");
        let (status, body) = self.post(id, p["digest"].as_str().unwrap()).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["ok"], json!(true), "{body}");
        body
    }

    fn convert_with(
        &self,
        id: i64,
        digest: &str,
        stop: Option<StopAt>,
        progress: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Value {
        let mut conn = self.conn();
        self.st
            .app()
            .with_convert_env(&mut conn, |env| {
                env.stop_at = stop;
                env.progress = progress;
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

    fn id_of(&self, dir_name: &str) -> String {
        self.conn()
            .query_row(
                "SELECT id FROM albums WHERE dir_name = ?1",
                [dir_name],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn missing(&self) -> Vec<String> {
        let c = self.conn();
        let mut s = c
            .prepare(
                "SELECT dir_name FROM albums WHERE missing_since IS NOT NULL ORDER BY dir_name",
            )
            .unwrap();
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

    /// The database as the user's data in it: album ids replaced by folder
    /// names (they hash temp paths), no time stamps.
    fn dump(&self) -> Vec<String> {
        let c = self.conn();
        let names: BTreeMap<String, String> = {
            let mut s = c.prepare("SELECT id, dir_name FROM albums").unwrap();
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let name = |id: &str| names.get(id).cloned().unwrap_or_else(|| "<gone>".into());
        let mut out = Vec::new();
        let mut s = c
            .prepare(
                "SELECT dir_name, track_numbers_json, track_discs_json, mb_release_group_mbid, \
                 missing_since IS NOT NULL, track_count FROM albums",
            )
            .unwrap();
        for r in s
            .query_map([], |r| {
                Ok(format!(
                    "album {:?} tracks={:?} discs={:?} rg={:?} missing={} n={:?}",
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                ))
            })
            .unwrap()
        {
            out.push(r.unwrap());
        }
        for (sql, what) in [
            (
                "SELECT album_id, filename || ' ' || tag FROM track_tags",
                "tag",
            ),
            (
                "SELECT album_id, filename || ' ' || fingerprint FROM track_fingerprints",
                "fp",
            ),
            (
                "SELECT album_id, filename FROM fingerprint_rejects",
                "reject",
            ),
            (
                "SELECT album_id, release_group_mbid FROM album_blocked_mb_release_groups",
                "block",
            ),
            (
                "SELECT album_id, check_id || ' ' || summary || ' ignored=' || (ignored_at IS NOT NULL) \
                 FROM check_findings WHERE resolved_at IS NULL AND check_id != 'multi-disc-layout'",
                "finding",
            ),
        ] {
            let mut s = c.prepare(sql).unwrap();
            for r in s
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
            {
                let (id, v) = r.unwrap();
                out.push(format!("{what} {} {v}", name(&id)));
            }
        }
        out.sort();
        out
    }

    /// The library and the app trash.
    fn disk(&self) -> (BTreeMap<String, String>, Vec<String>) {
        let trash = tree(&self.cfg.join("trash")).into_keys().collect();
        (tree(&self.lib), trash)
    }
}

fn count(events: &[String], name: &str) -> usize {
    events.iter().filter(|e| *e == name).count()
}

const D1: &str = "Band - Record (disc 1 of 3) [FLAC]";
const D2: &str = "Band - Record (disc 2 of 3) [FLAC]";
const D3: &str = "Band - Record (disc 3 of 3) [FLAC]";
const SET: &str = "Band - Record [FLAC]";

/// Three discs (3, 2 and 2 tracks) with data of every kind on every member:
/// a user tag (which writes the disc's sidecar), a tag kept only in a
/// sidecar, fingerprints, a decode reject, an anchor on disc 2, a finding
/// ignored on disc 2, the same cover in discs 1 and 2 and another in
/// disc 3, a text file in disc 2, a rip log in every disc (disc 3's a copy
/// of disc 1's, disc 2's another) and a `.DS_Store` in disc 3.
async fn box_set(w: &World) {
    write_disc(&w.lib.join(D1), 1, 3, &["A1", "A2", "A3"]);
    write_disc(&w.lib.join(D2), 2, 3, &["B1", "B2"]);
    write_disc(&w.lib.join(D3), 3, 3, &["C1", "C2"]);
    std::fs::write(w.lib.join(D1).join("cover.jpg"), b"picture one").unwrap();
    std::fs::write(w.lib.join(D2).join("cover.jpg"), b"picture one").unwrap();
    std::fs::write(w.lib.join(D3).join("cover.jpg"), b"picture three").unwrap();
    std::fs::write(w.lib.join(D2).join("notes.txt"), b"liner notes").unwrap();
    std::fs::write(w.lib.join(D1).join("rip.log"), b"log of disc one").unwrap();
    std::fs::write(w.lib.join(D2).join("rip.log"), b"log of disc two").unwrap();
    std::fs::write(w.lib.join(D3).join("rip.log"), b"log of disc one").unwrap();
    std::fs::write(w.lib.join(D3).join(".DS_Store"), b"clutter").unwrap();
    w.scan().await;
    let (d1, d2, d3) = (w.id_of(D1), w.id_of(D2), w.id_of(D3));
    for (album, file, tag) in [
        (&d1, "02 - A2.flac", "favourite"),
        (&d2, "01 - B1.flac", "live"),
    ] {
        let r = call(
            &w.app,
            "POST",
            &format!(
                "/api/albums/{album}/tracks/{}/tags",
                file.replace(' ', "%20")
            ),
            Some(&json!({"tag": tag})),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
    }
    // A tag Musaic wrote into disc 3's sidecar but no longer has a row for
    // (a database restored from before it, say).
    std::fs::write(
        w.lib.join(D3).join(".media-manager-tags.json"),
        "{\n  \"02 - C2.flac\": [\n    \"remaster\"\n  ]\n}",
    )
    .unwrap();
    let c = w.conn();
    for (album, file, fp) in [
        (&d1, "01 - A1.flac", "fp-a1"),
        (&d2, "02 - B2.flac", "fp-b2"),
        (&d3, "01 - C1.flac", "fp-c1"),
    ] {
        c.execute(
            "INSERT INTO track_fingerprints (album_id, filename, fingerprint, duration_s, last_computed_at) \
             VALUES (?1, ?2, ?3, 1.0, '2026-01-01T00:00:00Z')",
            [album.as_str(), file, fp],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO fingerprint_rejects (album_id, filename, file_signature, fingerprint_impl, rejected_at) \
         VALUES (?1, '02 - C2.flac', 'sig', 'impl', '2026-01-01T00:00:00Z')",
        [&d3],
    )
    .unwrap();
    c.execute(
        "UPDATE albums SET mb_release_group_mbid = 'rg-record', mb_match_score = 90 WHERE id = ?1",
        [&d2],
    )
    .unwrap();
    c.execute(
        "INSERT INTO check_findings (check_id, album_id, severity, summary, first_seen_at, last_seen_at, ignored_at) \
         VALUES ('missing-art', ?1, 'warn', 'no embedded art', 't', 't', 't')",
        [&d2],
    )
    .unwrap();
}

/// The final state of an uninterrupted conversion of [`box_set`].
async fn reference() -> ((BTreeMap<String, String>, Vec<String>), Vec<String>) {
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    w.convert_ok(id).await;
    (w.disk(), w.dump())
}

// ---------------------------------------------------------------- the conversion

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_disc_folders_become_one_with_everything_they_had() {
    let w = World::new();
    box_set(&w).await;
    let (d1, d2, d3) = (w.id_of(D1), w.id_of(D2), w.id_of(D3));
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["state"], json!("folders"), "{p}");
    assert_eq!(p["conversion_available"], json!(true), "{p}");
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
    assert_eq!(
        planned.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>(),
        [
            "01 - A1.flac",
            "02 - A2.flac",
            "03 - A3.flac",
            "04 - B1.flac",
            "05 - B2.flac",
            "06 - C1.flac",
            "07 - C2.flac"
        ]
    );
    // What the dialog lists for the folders.
    let folders = p["folders"].as_array().unwrap();
    assert_eq!(folders[1]["covers"][0]["action"], json!("identical"));
    assert_eq!(folders[2]["covers"][0]["action"], json!("different"));
    // Every other file of a disc's folder says where it goes: under its
    // own name, under a new one where the set's folder has a different
    // file of that name, or to the trash where it has the same one.
    assert_eq!(folders[0]["other_files"], json!(["rip.log"]));
    assert_eq!(folders[1]["other_files"], json!([]));
    assert_eq!(
        folders[1]["strays"],
        json!([
            {"name": "notes.txt", "action": "moved", "new_name": null, "done": false},
            {"name": "rip.log", "action": "moved", "new_name": "rip (disc 2).log", "done": false},
        ])
    );
    assert_eq!(
        folders[2]["strays"],
        json!([{"name": "rip.log", "action": "identical", "new_name": null, "done": false}])
    );
    assert_eq!(folders[2]["junk"][0]["name"], json!(".DS_Store"));
    w.events();

    let body = w.convert_ok(id).await;
    assert_eq!(body["folder"]["outcome"], json!("renamed"), "{body}");
    assert_eq!(body["merged"].as_array().unwrap().len(), 2, "{body}");

    // One folder with every file, numbered straight through, no disc tags.
    let set = w.lib.join(SET);
    // No disc folder is left behind: their other files came along.
    assert_eq!(names(&w.lib), [SET]);
    assert_eq!(
        names(&set),
        [
            ".media-manager-tags.json",
            "01 - A1.flac",
            "02 - A2.flac",
            "03 - A3.flac",
            "04 - B1.flac",
            "05 - B2.flac",
            "06 - C1.flac",
            "07 - C2.flac",
            "cover.jpg",
            "notes.txt",
            "rip (disc 2).log",
            "rip.log"
        ]
    );
    let text = |name: &str| String::from_utf8(std::fs::read(set.join(name)).unwrap()).unwrap();
    assert_eq!(text("notes.txt"), "liner notes");
    assert_eq!(text("rip.log"), "log of disc one");
    assert_eq!(text("rip (disc 2).log"), "log of disc two");
    // The record says where each went.
    let rec = &w.records()[0];
    assert_eq!(
        rec["folders"][1]["strays"],
        json!([
            {"name": "notes.txt", "action": "moved", "new_name": null},
            {"name": "rip.log", "action": "moved", "new_name": "rip (disc 2).log"},
        ])
    );
    let steps = rec["attempts"][0]["steps"].as_array().unwrap();
    let covers = steps.iter().find(|s| s["step"] == json!("covers")).unwrap();
    let moved: Vec<&Value> = covers["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["action"] == json!("file_moved"))
        .collect();
    assert_eq!(
        moved,
        [
            &json!({"folder": D2, "name": "notes.txt", "action": "file_moved", "to": "notes.txt", "outcome": "moved"}),
            &json!({"folder": D2, "name": "rip.log", "action": "file_moved", "to": "rip (disc 2).log", "outcome": "moved"}),
        ]
    );
    assert_eq!(body["folders"][1]["outcome"], json!("removed"), "{body}");
    assert_eq!(body["folders"][2]["outcome"], json!("removed"), "{body}");
    for (n, name) in names(&set)
        .iter()
        .filter(|n| n.ends_with(".flac"))
        .enumerate()
    {
        let t = numbering(&set.join(name));
        assert_eq!(t["TRACKNUMBER"], [(n + 1).to_string()], "{name}");
        assert_eq!(t["TRACKTOTAL"], ["7"], "{name}");
        assert!(
            !t.contains_key("DISCNUMBER") && !t.contains_key("DISCTOTAL"),
            "{name}"
        );
    }
    assert_eq!(
        std::fs::read(set.join("cover.jpg")).unwrap(),
        b"picture one"
    );
    assert!(!w.lib.join(D2).exists());
    assert!(!w.lib.join(D3).exists());
    // The covers and the clutter that are no longer needed, and the old
    // sidecars, are in the app trash.
    let (_, trash) = w.disk();
    for t in [
        format!("{D2}/cover.jpg"),
        format!("{D2}/.media-manager-tags.json"),
        format!("{D3}/cover.jpg"),
        format!("{D3}/.DS_Store"),
        format!("{D3}/.media-manager-tags.json"),
        // The copy of a log the set's folder already has.
        format!("{D3}/rip.log"),
    ] {
        assert!(trash.contains(&t), "{t} not in {trash:?}");
    }
    assert_eq!(
        std::fs::read(w.cfg.join("trash").join(D3).join("cover.jpg")).unwrap(),
        b"picture three"
    );

    // One album row with everything the three had.
    let survivor = musaic_core::album_id(&set.to_string_lossy());
    assert_eq!(w.album_ids(), std::slice::from_ref(&survivor));
    assert_eq!(body["album_id"], json!(survivor));
    let c = w.conn();
    let rows = |sql: &str| -> Vec<String> {
        let mut s = c.prepare(sql).unwrap();
        s.query_map([&survivor], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    };
    assert_eq!(
        rows("SELECT filename || ':' || tag FROM track_tags WHERE album_id = ?1 ORDER BY 1"),
        [
            "02 - A2.flac:favourite",
            "04 - B1.flac:live",
            "07 - C2.flac:remaster"
        ]
    );
    assert_eq!(
        rows(
            "SELECT filename || ':' || fingerprint FROM track_fingerprints WHERE album_id = ?1 ORDER BY 1"
        ),
        [
            "01 - A1.flac:fp-a1",
            "05 - B2.flac:fp-b2",
            "06 - C1.flac:fp-c1"
        ]
    );
    assert_eq!(
        rows("SELECT filename FROM fingerprint_rejects WHERE album_id = ?1"),
        ["07 - C2.flac"]
    );
    let (rg, tracks): (Option<String>, String) = c
        .query_row(
            "SELECT mb_release_group_mbid, track_numbers_json FROM albums WHERE id = ?1",
            [&survivor],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rg.as_deref(), Some("rg-record"));
    assert_eq!(tracks, "[1, 2, 3, 4, 5, 6, 7]");
    // The sidecar holds every tag, the one only the old sidecar had too.
    let sidecar: Value = serde_json::from_str(
        &std::fs::read_to_string(set.join(".media-manager-tags.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        sidecar,
        json!({"02 - A2.flac": ["favourite"], "04 - B1.flac": ["live"], "07 - C2.flac": ["remaster"]})
    );
    // Disc 2's ignored finding is resolved under its old id and does not
    // carry over.
    let (resolved, ignored): (Option<String>, Option<String>) = c
        .query_row(
            "SELECT resolved_at, ignored_at FROM check_findings WHERE check_id = 'missing-art'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(resolved.is_some() && ignored.is_some());
    let n: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM check_findings WHERE album_id = ?1 AND check_id = 'missing-art'",
            [&survivor],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 0);

    // The events: the members' renames and tag writes, a rename onto the
    // survivor per merged row, the folder's, one batch at the end.
    let ev = w.events();
    assert_eq!(count(&ev, "AlbumRenamed(ids)"), 3, "{ev:?}");
    assert_eq!(count(&ev, "BulkBatchCompleted"), 1, "{ev:?}");
    assert!(count(&ev, "AlbumTagsChanged") >= 3, "{ev:?}");

    // The record tells the whole story.
    let recs = w.records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert_eq!(r["kind"], json!("folders"));
    assert_eq!(r["version"], json!(2));
    assert_eq!(r["album_id_before"], json!(d1));
    assert_eq!(r["album_id_after"], json!(survivor));
    assert_eq!(r["members"].as_array().unwrap().len(), 3);
    assert_eq!(r["files"][3]["album_id"], json!(d2));
    assert_eq!(r["files"][3]["old_name"], json!("01 - B1.flac"));
    assert_eq!(r["files"][3]["new_name"], json!("04 - B1.flac"));
    assert_eq!(r["files"][3]["old_tags"]["DISCNUMBER"], json!(["2"]));
    assert!(r["finished_at"].is_string());
    let steps: Vec<&str> = r["attempts"][0]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["step"].as_str().unwrap())
        .collect();
    assert_eq!(
        steps,
        [
            "merge_sidecar_tags",
            "rename_files",
            "write_tags",
            "move_files",
            "covers",
            "sidecar",
            "fold",
            "remove_folders",
            "rename_folder"
        ]
    );
    let _ = d3;

    // The check finds nothing; a full scan afterwards changes nothing.
    w.run(CHECK).await;
    assert!(w.open_findings().await.is_empty());
    let before = (w.disk(), w.dump(), w.album_ids());
    w.scan().await;
    assert_eq!((w.disk(), w.dump(), w.album_ids()), before);
    assert!(w.missing().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cd_markers_in_folder_names_gather_too() {
    let w = World::new();
    let one = "Band - Record CD1 [FLAC]";
    let two = "Band - Record CD 2 [FLAC]";
    write_disc(&w.lib.join(one), 1, 2, &["A1", "A2"]);
    write_disc(&w.lib.join(two), 2, 2, &["B1"]);
    w.scan().await;
    let id = w.only_finding().await;
    let body = w.convert_ok(id).await;
    assert_eq!(body["folder"]["new"], json!(SET), "{body}");
    assert_eq!(names(&w.lib), [SET]);
    assert_eq!(
        names(&w.lib.join(SET)),
        ["01 - A1.flac", "02 - A2.flac", "03 - B1.flac"]
    );
    assert_eq!(w.album_ids().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_cover_of_its_own_the_destination_takes_the_next_discs() {
    let w = World::new();
    write_disc(&w.lib.join(D1), 1, 3, &["A1"]);
    write_disc(&w.lib.join(D2), 2, 3, &["B1"]);
    write_disc(&w.lib.join(D3), 3, 3, &["C1"]);
    std::fs::write(w.lib.join(D2).join("folder.jpg"), b"two").unwrap();
    std::fs::write(w.lib.join(D3).join("folder.jpg"), b"two").unwrap();
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["folders"][1]["covers"][0]["action"], json!("moved"));
    assert_eq!(p["folders"][2]["covers"][0]["action"], json!("identical"));
    w.convert_ok(id).await;
    assert_eq!(
        std::fs::read(w.lib.join(SET).join("folder.jpg")).unwrap(),
        b"two"
    );
    assert_eq!(names(&w.lib), [SET]);
}

// ---------------------------------------------------------------- interruption

/// Stop at `stop` (as a crash there), then: the library is valid, a scan
/// run right then loses nothing and marks nothing Missing, the finding
/// leads back to the conversion, a second Convert finishes, and the end is
/// exactly the uninterrupted run's.
async fn interrupted_then_finished(stop: StopAt) {
    let (want_disk, want_db) = reference().await;
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let out = w.convert_with(id, p["digest"].as_str().unwrap(), Some(stop), None);
    assert_eq!(out["interrupted"], json!(true), "{stop:?}: {out}");
    let recs = w.records();
    assert_eq!(recs.len(), 1);
    assert!(recs[0]["finished_at"].is_null());

    // Every file is a readable FLAC under one name, in one folder.
    let flacs: Vec<String> = tree(&w.lib)
        .keys()
        .filter(|k| k.ends_with(".flac"))
        .cloned()
        .collect();
    assert_eq!(flacs.len(), 7, "{stop:?}: {flacs:?}");
    for f in &flacs {
        assert!(musaic_scan::flacmeta::read(&w.lib.join(f)).is_ok(), "{f}");
    }

    // A scan right now (after a restart, say) leaves the set as it is.
    let ids_before = w.album_ids();
    let db_before = w.dump();
    w.scan().await;
    assert!(w.missing().is_empty(), "{stop:?}: {:?}", w.missing());
    assert_eq!(w.album_ids(), ids_before, "{stop:?}");
    assert_eq!(w.dump(), db_before, "{stop:?}");

    // The check keeps one finding, which offers to finish.
    w.run(CHECK).await;
    let open = w.open_findings().await;
    assert_eq!(open.len(), 1, "{stop:?}: {open:?}");
    let id = open[0]["id"].as_i64().unwrap();
    let p2 = w.preview(id).await;
    assert_eq!(p2["conversion_available"], json!(true), "{stop:?}: {p2}");
    assert!(p2["resume"]["record"].is_string(), "{p2}");
    let plan = |p: &Value| -> Vec<(Value, Value)> {
        p["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["new_track"].clone(), t["new_filename"].clone()))
            .collect()
    };
    assert_eq!(plan(&p2), plan(&p), "{stop:?}");

    let body = w.convert_ok(id).await;
    assert_eq!(body["ok"], json!(true));
    assert_eq!(w.disk(), want_disk, "{stop:?}");
    assert_eq!(w.dump(), want_db, "{stop:?}");
    let recs = w.records();
    assert_eq!(recs.len(), 1);
    let r = &recs[0];
    assert!(r["finished_at"].is_string(), "{r}");
    let attempts = r["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "{r}");
    assert!(attempts[0]["outcome"].is_null(), "{r}");
    assert_eq!(attempts[1]["outcome"], json!("completed"));
    // The fold never refused: no step failed in either attempt.
    for a in attempts {
        for s in a["steps"].as_array().unwrap() {
            assert_eq!(s["ok"], json!(true), "{stop:?}: {s}");
        }
    }
    w.run(CHECK).await;
    assert!(w.open_findings().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_the_record() {
    interrupted_then_finished(StopAt::AfterRecord).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_some_renames() {
    // Disc 2's two files renamed, disc 3's not.
    interrupted_then_finished(StopAt::RenamedFiles(2)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_the_renames_before_the_tags() {
    interrupted_then_finished(StopAt::TaggedFiles(0)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_the_tags_of_some_members() {
    // Disc 1 and one file of disc 2.
    interrupted_then_finished(StopAt::TaggedFiles(4)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_between_a_move_and_its_rows() {
    interrupted_then_finished(StopAt::MovedFiles { n: 1, rows: false }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_some_moves() {
    interrupted_then_finished(StopAt::MovedFiles { n: 3, rows: true }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_the_last_move_before_its_rows() {
    interrupted_then_finished(StopAt::MovedFiles { n: 4, rows: false }).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_moving_one_other_file() {
    interrupted_then_finished(StopAt::MovedStrays(1)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_moving_every_other_file() {
    interrupted_then_finished(StopAt::MovedStrays(2)).await;
}

/// Stopped right after a rip log moved under its new name: the preview of
/// what is left says which files are done and which are not, from the
/// record and the disk alone, and nothing is moved twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_conversion_stopped_between_two_other_files_knows_which_moved() {
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let digest = p["digest"].as_str().unwrap();
    let out = w.convert_with(id, digest, Some(StopAt::MovedStrays(2)), None);
    assert_eq!(out["interrupted"], json!(true), "{out}");
    // notes.txt and disc 2's log are in the set's folder; disc 3's copy
    // of the log has not been looked at yet.
    assert!(w.lib.join(D1).join("notes.txt").is_file());
    assert_eq!(
        std::fs::read(w.lib.join(D1).join("rip (disc 2).log")).unwrap(),
        b"log of disc two"
    );
    assert_eq!(
        std::fs::read(w.lib.join(D1).join("rip.log")).unwrap(),
        b"log of disc one"
    );
    assert!(!w.lib.join(D2).join("rip.log").exists());
    assert!(w.lib.join(D3).join("rip.log").is_file());
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    let p2 = w.preview(id).await;
    assert_eq!(p2["conversion_available"], json!(true), "{p2}");
    assert_eq!(
        p2["folders"][1]["strays"],
        json!([
            {"name": "notes.txt", "action": "moved", "new_name": null, "done": true},
            {"name": "rip.log", "action": "moved", "new_name": "rip (disc 2).log", "done": true},
        ])
    );
    assert_eq!(
        p2["folders"][2]["strays"],
        json!([{"name": "rip.log", "action": "identical", "new_name": null, "done": false}])
    );
    let body = w.convert_ok(id).await;
    assert_eq!(
        body["folders"][1]["strays"],
        json!([
            {"name": "notes.txt", "action": "moved", "new_name": null, "outcome": "already"},
            {"name": "rip.log", "action": "moved", "new_name": "rip (disc 2).log", "outcome": "already"},
        ])
    );
    assert_eq!(names(&w.lib), [SET]);
}

/// A moved file that is then in both folders, or in neither, is never
/// guessed at: finishing is refused until it is sorted out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moved_file_found_twice_or_not_at_all_refuses_to_finish() {
    for both in [true, false] {
        let w = World::new();
        box_set(&w).await;
        let id = w.only_finding().await;
        let p = w.preview(id).await;
        let digest = p["digest"].as_str().unwrap();
        w.convert_with(id, digest, Some(StopAt::MovedStrays(1)), None);
        if both {
            std::fs::write(w.lib.join(D2).join("notes.txt"), b"back again").unwrap();
        } else {
            std::fs::remove_file(w.lib.join(D1).join("notes.txt")).unwrap();
        }
        w.run(CHECK).await;
        let id = w.open_findings().await[0]["id"].as_i64().unwrap();
        let p2 = w.preview(id).await;
        assert_eq!(p2["conversion_available"], json!(false), "{both}: {p2}");
        let codes: Vec<&str> = p2["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, ["resume_mismatch"], "{both}: {p2}");
    }
}

/// A subfolder of a disc's folder is never moved (Musaic does not look
/// inside one), so that one folder stays, holding only it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_disc_folder_with_a_subfolder_stays() {
    let w = World::new();
    box_set(&w).await;
    std::fs::create_dir(w.lib.join(D2).join("Scans")).unwrap();
    std::fs::write(w.lib.join(D2).join("Scans").join("back.png"), b"scan").unwrap();
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["folders"][1]["other_files"], json!(["Scans"]), "{p}");
    let body = w.convert_ok(id).await;
    assert_eq!(names(&w.lib), [D2, SET]);
    assert_eq!(names(&w.lib.join(D2)), ["Scans"]);
    assert_eq!(body["folders"][1]["outcome"], json!("kept"), "{body}");
    assert_eq!(body["folders"][1]["left"], json!(["Scans"]), "{body}");
    assert!(w.lib.join(SET).join("notes.txt").is_file());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_the_covers() {
    interrupted_then_finished(StopAt::AfterCovers).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_the_sidecar() {
    interrupted_then_finished(StopAt::AfterSidecar).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_the_fold() {
    interrupted_then_finished(StopAt::AfterFold).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_removing_a_folder() {
    interrupted_then_finished(StopAt::RemovedFolders(1)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_before_the_folder_rename() {
    interrupted_then_finished(StopAt::BeforeFolderRename).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_after_the_folder_rename() {
    interrupted_then_finished(StopAt::AfterFolderRename).await;
}

/// The folder got its new name on disk but the process died before its
/// row did (the window inside the rename): a scan leaves the old row as it
/// is and adds no album for the new name; Convert moves the row to it and
/// finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_renamed_on_disk_but_not_in_the_database_is_repaired() {
    let (want_disk, want_db) = reference().await;
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    w.convert_with(
        id,
        p["digest"].as_str().unwrap(),
        Some(StopAt::BeforeFolderRename),
        None,
    );
    // The rename itself, without its database half.
    std::fs::rename(w.lib.join(D1), w.lib.join(SET)).unwrap();
    let ids = w.album_ids();
    w.scan().await;
    assert_eq!(w.album_ids(), ids);
    assert!(w.missing().is_empty());
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    w.convert_ok(id).await;
    assert_eq!(w.disk(), want_disk);
    assert_eq!(w.dump(), want_db);
}

/// A conversion the user does not want to finish: abandoning it closes the
/// record (kept), moves nothing, and scans read the folders again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abandoned_conversion_leaves_every_file_where_it_is() {
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    w.convert_with(
        id,
        p["digest"].as_str().unwrap(),
        Some(StopAt::MovedFiles { n: 1, rows: false }),
        None,
    );
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    let p2 = w.preview(id).await;
    // The preview says where each file is now.
    let moved: Vec<&Value> = p2["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["state"] == json!("moved"))
        .collect();
    assert_eq!(moved.len(), 1, "{p2}");
    assert_eq!(moved[0]["folder"], json!(D1));
    let disk = w.disk();
    let r = call(
        &w.app,
        "POST",
        &format!("/api/checks/{CHECK}/findings/{id}/abandon-conversion"),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(w.disk(), disk);
    let rec = &w.records()[0];
    assert!(rec["abandoned_at"].is_string() && rec["finished_at"].is_string());
    assert_eq!(
        rec["attempts"].as_array().unwrap().last().unwrap()["outcome"],
        json!("abandoned")
    );
    // The moved file's rows went with it before the record was closed.
    let c = w.conn();
    let d1 = w.id_of(D1);
    let fp: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM track_tags WHERE album_id = ?1 AND filename = '04 - B1.flac'",
            [&d1],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fp, 1);
    // Scans read the folders again: every album stays, none Missing.
    w.scan().await;
    assert_eq!(w.album_ids().len(), 3);
    assert!(w.missing().is_empty());
}

/// Abandoned after one of the other files had moved: it stays in the
/// set's folder (abandoning moves nothing back), and the closed record says
/// where each of those files is and what it was called before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abandoned_conversion_records_where_the_other_files_are() {
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    w.convert_with(
        id,
        p["digest"].as_str().unwrap(),
        Some(StopAt::MovedStrays(2)),
        None,
    );
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    let disk = w.disk();
    let r = call(
        &w.app,
        "POST",
        &format!("/api/checks/{CHECK}/findings/{id}/abandon-conversion"),
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(w.disk(), disk);
    assert_eq!(
        std::fs::read(w.lib.join(D1).join("rip (disc 2).log")).unwrap(),
        b"log of disc two"
    );
    let rec = &w.records()[0];
    assert!(rec["abandoned_at"].is_string() && rec["finished_at"].is_string());
    // The plan it was following is still there to put a file back by hand.
    assert_eq!(
        rec["folders"][1]["strays"][1],
        json!({"name": "rip.log", "action": "moved", "new_name": "rip (disc 2).log"})
    );
    let last = rec["attempts"].as_array().unwrap().last().unwrap();
    let strays: Vec<&Value> = last["steps"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["from"].is_string())
        .collect();
    assert_eq!(
        strays,
        [
            &json!({"file": "notes.txt", "from": D2, "folder": D1, "name": "notes.txt", "state": "moved"}),
            &json!({"file": "rip.log", "from": D2, "folder": D1, "name": "rip (disc 2).log", "state": "moved"}),
            &json!({"file": "rip.log", "from": D3, "folder": D3, "name": "rip.log", "state": "untouched"}),
        ]
    );
    // Scans read the folders again; the discs whose files had all moved
    // hold no album any more, and nothing is lost.
    w.scan().await;
    assert!(w.lib.join(D3).join("rip.log").is_file());
}

// ---------------------------------------------------------------- refusals

/// Each preflight refusal: 409 with the blocker, nothing changed on disk
/// or in the database, no record.
async fn refused(setup: impl FnOnce(&World), code: &str) {
    let w = World::new();
    box_set(&w).await;
    setup(&w);
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["conversion_available"], json!(false), "{p}");
    let codes: Vec<&str> = p["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&code), "{code} not in {codes:?}");
    let (disk, db) = (w.disk(), w.dump());
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("blocked"));
    assert_eq!((w.disk(), w.dump()), (disk, db));
    assert!(w.records().is_empty());
}

fn job(w: &World, dir: &str, status: &str) {
    w.conn()
        .execute(
            "INSERT INTO ai_jobs (id, album_id, album_path, job_type, status, created_at) \
             VALUES ('job1', ?1, 'x', 'metadata', ?2, 't')",
            [w.id_of(dir), status.to_string()],
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_job_on_a_disc_refuses() {
    refused(|w| job(w, D2, "running"), "job_active").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_suggestion_waiting_on_a_disc_refuses() {
    // The fold would refuse it: the suggestion was made for one disc.
    refused(|w| job(w, D3, "completed"), "job_active").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anchors_that_differ_refuse() {
    // Detection compares them folded; the fold compares them exactly.
    refused(
        |w| {
            w.conn()
                .execute(
                    "UPDATE albums SET mb_release_group_mbid = 'RG-RECORD' WHERE id = ?1",
                    [w.id_of(D3)],
                )
                .unwrap();
        },
        "anchors_differ",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_anchor_blocked_on_a_disc_refuses() {
    refused(
        |w| {
            w.conn()
                .execute(
                    "INSERT INTO album_blocked_mb_release_groups (album_id, release_group_mbid, blocked_at) \
                     VALUES (?1, 'rg-record', 't')",
                    [w.id_of(D3)],
                )
                .unwrap();
        },
        "anchor_blocked",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_taken_in_a_discs_own_folder_refuses() {
    refused(
        |w| std::fs::create_dir(w.lib.join(D2).join("05 - B2.flac")).unwrap(),
        "name_collision",
    )
    .await;
}

/// A different file under a disc's file's name in the set's folder, and
/// the name it would get instead taken too: refused, never guessed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_file_with_no_free_name_in_the_destination_refuses() {
    refused(
        |w| std::fs::write(w.lib.join(D1).join("Rip (Disc 2).log"), b"taken").unwrap(),
        "name_collision",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_taken_in_the_destination_refuses() {
    refused(
        |w| std::fs::create_dir(w.lib.join(D1).join("06 - C1.flac")).unwrap(),
        "name_collision",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rows_of_a_gone_file_under_a_new_name_refuse() {
    refused(
        |w| {
            w.conn()
                .execute(
                    "INSERT INTO track_tags (album_id, filename, tag, created_at) \
                     VALUES (?1, '06 - C1.flac', 'stale', 't')",
                    [w.id_of(D2)],
                )
                .unwrap();
        },
        "orphan_rows",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_disc_refuses() {
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    w.conn()
        .execute(
            "UPDATE albums SET missing_since = 't' WHERE id = ?1",
            [w.id_of(D3)],
        )
        .unwrap();
    let p = w.preview(id).await;
    assert_eq!(p["conversion_available"], json!(false), "{p}");
    let (disk, db) = (w.disk(), w.dump());
    let (status, _) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409);
    assert_eq!((w.disk(), w.dump()), (disk, db));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plan_that_changed_since_the_preview_refuses() {
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    // Disc 3's picture is now the same as the one kept: another plan.
    std::fs::write(w.lib.join(D3).join("cover.jpg"), b"picture one").unwrap();
    let (disk, db) = (w.disk(), w.dump());
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("plan_changed"));
    assert_eq!((w.disk(), w.dump()), (disk, db));
    assert!(w.records().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incomplete_set_of_folders_never_converts() {
    let w = World::new();
    write_disc(&w.lib.join(D1), 1, 3, &["A1"]);
    write_disc(&w.lib.join(D3), 3, 3, &["C1"]);
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["state"], json!("incomplete"));
    assert_eq!(p["conversion_available"], json!(false));
    let (status, body) = w.post(id, p["digest"].as_str().unwrap_or("")).await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["reason_code"], json!("not_convertible"));
}

// ---------------------------------------------------------------- real failures

/// A file appears in the destination under a name a disc's file is about
/// to move to (after the preview, while the conversion runs): that file is
/// not moved, nothing is overwritten, and once the stranger is gone a
/// second Convert finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_name_appearing_in_the_destination_stops_the_moves() {
    let (want_disk, _) = reference().await;
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let intruder = w.lib.join(D1).join("05 - B2.flac");
    let at = intruder.clone();
    let progress: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |v: Value| {
        if v["step"] == json!("write_tags") && v["done"] == v["total"] && !at.exists() {
            std::fs::write(&at, b"someone else's file").unwrap();
        }
    });
    let out = w.convert_with(id, p["digest"].as_str().unwrap(), None, Some(progress));
    assert_eq!(out["ok"], json!(false), "{out}");
    assert_eq!(std::fs::read(&intruder).unwrap(), b"someone else's file");
    let failed: Vec<&Value> = out["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["move"] == json!("failed"))
        .collect();
    assert_eq!(failed.len(), 1, "{out}");
    assert_eq!(failed[0]["reason_code"], json!("already_exists"), "{out}");
    // The disc's file is still in its own folder, renamed and retagged.
    assert!(w.lib.join(D2).join("05 - B2.flac").is_file());
    std::fs::remove_file(&intruder).unwrap();
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    w.convert_ok(id).await;
    assert_eq!(w.disk().0, want_disk.0);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unwritable_disc_folder_and_a_read_only_file_stop_cleanly() {
    use std::os::unix::fs::PermissionsExt;
    let mode =
        |p: &Path, m: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    let (want_disk, want_db) = reference().await;
    let w = World::new();
    box_set(&w).await;
    let d3 = w.lib.join(D3);
    mode(&d3, 0o555);
    if std::fs::write(d3.join(".probe"), b"x").is_ok() {
        let _ = std::fs::remove_file(d3.join(".probe"));
        mode(&d3, 0o755);
        return;
    }
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let (status, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    mode(&d3, 0o755);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ok"], json!(false), "{body}");
    assert!(
        body["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rename"] == json!("failed") && f["user_message"].is_string()),
        "{body}"
    );
    // Nothing moved: the library is the three discs.
    assert_eq!(w.album_ids().len(), 3);
    assert!(w.lib.join(D3).join("01 - C1.flac").is_file());

    // A read-only file: its tag write fails, nothing moves.
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    let ro = w.lib.join(D2).join("05 - B2.flac");
    mode(&ro, 0o444);
    let p = w.preview(id).await;
    let (_, body) = w.post(id, p["digest"].as_str().unwrap()).await;
    mode(&ro, 0o644);
    if body["ok"] == json!(false) {
        assert!(
            body["files"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["tags"] == json!("failed")),
            "{body}"
        );
        assert_eq!(w.album_ids().len(), 3);
        w.run(CHECK).await;
        let id = w.open_findings().await[0]["id"].as_i64().unwrap();
        w.convert_ok(id).await;
    }
    assert_eq!(w.disk(), want_disk);
    assert_eq!(w.dump(), want_db);
}

/// A file held open with read sharing only (as a player holds it) cannot
/// leave its folder: the move fails cleanly and a second Convert finishes
/// once it is closed.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_held_open_fails_its_move_and_resumes() {
    use std::os::windows::fs::OpenOptionsExt;
    let (want_disk, _) = reference().await;
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let held = w.lib.join(D2).join("05 - B2.flac");
    let slot: Arc<Mutex<Option<std::fs::File>>> = Arc::default();
    let s2 = Arc::clone(&slot);
    let h2 = held.clone();
    let progress: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |v: Value| {
        if v["step"] == json!("write_tags") && v["done"] == v["total"] {
            let mut s = s2.lock().unwrap();
            if s.is_none() {
                // FILE_SHARE_READ only: no delete, no rename.
                *s = std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(1)
                    .open(&h2)
                    .ok();
            }
        }
    });
    let out = w.convert_with(id, p["digest"].as_str().unwrap(), None, Some(progress));
    drop(slot.lock().unwrap().take());
    assert_eq!(out["ok"], json!(false), "{out}");
    assert!(held.is_file());
    w.run(CHECK).await;
    let id = w.open_findings().await[0]["id"].as_i64().unwrap();
    w.convert_ok(id).await;
    assert_eq!(w.disk().0, want_disk.0);
}

// ---------------------------------------------------------------- the pool, the sweep, the stream

/// While a conversion runs the fingerprint pool is paused (a pass asked
/// for meanwhile waits) and the check sweep is suspended; both are back
/// afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pool_and_the_sweep_stay_away_while_it_runs() {
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let seen: Arc<Mutex<Vec<(bool, bool)>>> = Arc::default();
    let s2 = Arc::clone(&seen);
    let app = w.st.app().clone();
    let progress: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |_| {
        s2.lock()
            .unwrap()
            .push((app.pool().is_paused(), app.sweeper().is_suspended()));
    });
    let out = w.convert_with(id, p["digest"].as_str().unwrap(), None, Some(progress));
    assert_eq!(out["ok"], json!(true), "{out}");
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|(p, s)| *p && *s), "{seen:?}");
    assert!(!w.st.app().pool().is_paused());
    assert!(!w.st.app().sweeper().is_suspended());
}

/// Asked for a stream, Convert answers progress frames and one result.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_conversion_streams_its_progress() {
    let w = World::new();
    box_set(&w).await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/checks/{CHECK}/findings/{id}/convert"))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(Body::from(
            json!({"digest": p["digest"]}).to_string().into_bytes(),
        ))
        .unwrap();
    let r = send(&w.app, req).await;
    assert_eq!(r.status, 200);
    let text = r.text();
    assert!(text.contains("event: progress"), "{text}");
    assert!(text.contains("\"step\":\"move_files\""), "{text}");
    let result = text
        .split("event: result\ndata: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .expect("a result frame");
    let body: Value = serde_json::from_str(result).unwrap();
    assert_eq!(body["ok"], json!(true), "{body}");
    // A refusal is a `refused` frame with its status.
    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/checks/{CHECK}/findings/{id}/convert"))
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(Body::from(
            json!({"digest": "nope"}).to_string().into_bytes(),
        ))
        .unwrap();
    let text = send(&w.app, req).await.text();
    assert!(text.contains("event: refused"), "{text}");
}

// ---------------------------------------------------------------- HFS+

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

/// On HFS+ every name is listed decomposed: the moved files, their rows
/// and the survivor's id follow the listed spelling.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "mounts an HFS+ disk image; run with --ignored on macOS"]
async fn an_hfs_plus_volume_gathers_under_the_listed_spellings() {
    use unicode_normalization::UnicodeNormalization;
    let nfd = |s: &str| s.nfd().collect::<String>();
    let volume = HfsVolume::new();
    let w = World::with_library_in(Some(&volume.mount));
    write_disc(
        &w.lib.join("Band - Caf\u{e9} (disc 1 of 2) [FLAC]"),
        1,
        2,
        &["Caf\u{e9}"],
    );
    write_disc(
        &w.lib.join("Band - Caf\u{e9} (disc 2 of 2) [FLAC]"),
        2,
        2,
        &["Cr\u{e8}me"],
    );
    w.scan().await;
    let c = w.conn();
    let d2_id: String = c
        .query_row("SELECT id FROM albums WHERE disc_number = 2", [], |r| {
            r.get(0)
        })
        .unwrap();
    let r = call(
        &w.app,
        "POST",
        &format!("/api/albums/{d2_id}/tracks/01%20-%20Cre%CC%80me.flac/tags"),
        Some(&json!({"tag": "live"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let id = w.only_finding().await;
    w.convert_ok(id).await;
    let set = "Band - Caf\u{e9} [FLAC]";
    assert_eq!(names(&w.lib), [nfd(set)]);
    let dir = w.lib.join(nfd(set));
    assert_eq!(
        names(&dir)
            .into_iter()
            .filter(|n| n.ends_with(".flac"))
            .collect::<Vec<_>>(),
        [nfd("01 - Caf\u{e9}.flac"), nfd("02 - Cr\u{e8}me.flac")]
    );
    let ids = w.album_ids();
    assert_eq!(ids, [musaic_core::album_id(&dir.to_string_lossy())]);
    let tagged: String = w
        .conn()
        .query_row("SELECT filename FROM track_tags", [], |r| r.get(0))
        .unwrap();
    assert_eq!(tagged, nfd("02 - Cr\u{e8}me.flac"));
    w.run(CHECK).await;
    assert!(w.open_findings().await.is_empty());
    w.scan().await;
    assert_eq!(w.album_ids(), ids);
}
