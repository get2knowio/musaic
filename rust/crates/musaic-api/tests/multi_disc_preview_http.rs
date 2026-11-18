//! `GET /api/checks/multi-disc-layout/findings/{id}/conversion-preview`
//! over a library of generated FLACs: the route, its refusals (an unknown
//! finding, another check's finding), the plan for one folder and for three
//! disc folders (offsets exactly), covers identical and different, other
//! files, every blocker the disk can produce, and an incomplete set. It is a
//! read: no file of the library changes.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{call, no_key_fingerprinting};
use musaic_api::{AppState, Network, router};
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
    let vendor = b"multi-disc-test";
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

fn write_track(dir: &Path, name: &str, n: &str, disc: Option<i64>, title: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let mut tags = vec![
        ("ARTIST", "Band".to_string()),
        ("ALBUMARTIST", "Band".to_string()),
        ("ALBUM", "Record".to_string()),
        ("TITLE", title.to_string()),
        ("TRACKNUMBER", n.to_string()),
        ("DATE", "1999".to_string()),
    ];
    if let Some(d) = disc {
        tags.push(("DISCNUMBER", d.to_string()));
    }
    std::fs::write(dir.join(name), flac_bytes(&tags)).unwrap();
}

fn disc_dir(d: i64) -> String {
    format!("Band - Record (disc {d} of 3) [FLAC]")
}

/// Disc d with `n` tracks "Song d-k", numbered 1..n.
fn disc_folder(lib: &Path, d: i64, n: i64) -> PathBuf {
    let dir = lib.join(disc_dir(d));
    for k in 1..=n {
        write_track(
            &dir,
            &format!("{k:02} - Song {d}-{k}.flac"),
            &k.to_string(),
            Some(d),
            &format!("Song {d}-{k}"),
        );
    }
    dir
}

/// Every file under `dir` with its bytes.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p.clone());
                out.insert(p, Vec::new());
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

struct World {
    _tmp: tempfile::TempDir,
    lib: PathBuf,
    st: AppState,
    app: axum::Router,
}

impl World {
    fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = musaic_testdata::canonical(tmp.path());
        let cfg = root.join("config");
        let lib = root.join("library");
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
        let app = router(st.clone());
        World {
            _tmp: tmp,
            lib,
            st,
            app,
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

    async fn findings(&self, check: &str) -> Vec<Value> {
        let r = call(
            &self.app,
            "GET",
            &format!("/api/checks/findings?check_id={check}"),
            None,
        )
        .await;
        r.json()["findings"].as_array().unwrap().clone()
    }

    async fn only_finding(&self) -> i64 {
        self.run(CHECK).await;
        let f = self.findings(CHECK).await;
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

    fn conn(&self) -> rusqlite::Connection {
        musaic_db::open_connection(self.st.db_path()).unwrap()
    }
}

fn codes(p: &Value) -> Vec<String> {
    p["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["code"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_disc_folders_preview_and_refusals() {
    let w = World::new();
    let d1 = disc_folder(&w.lib, 1, 3);
    let d2 = disc_folder(&w.lib, 2, 2);
    let d3 = disc_folder(&w.lib, 3, 4);
    std::fs::write(d1.join("cover.jpg"), b"front").unwrap();
    std::fs::write(d2.join("cover.jpg"), b"front").unwrap();
    std::fs::write(d3.join("cover.jpg"), b"another picture").unwrap();
    std::fs::write(d3.join("booklet.pdf"), b"pdf").unwrap();
    w.scan().await;
    let id = w.only_finding().await;
    let before = tree(&w.lib);

    let p = w.preview(id).await;
    assert_eq!(p["state"], "folders");
    assert_eq!(p["convertible"], true, "{p}");
    assert_eq!(p["conversion_available"], false);
    assert_eq!(p["blockers"], json!([]));
    let members: Vec<(Value, Value)> = p["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| (m["dir_name"].clone(), m["disc"].clone()))
        .collect();
    assert_eq!(
        members,
        (1..=3)
            .map(|d| (json!(disc_dir(d)), json!(d)))
            .collect::<Vec<_>>()
    );
    let rows: Vec<(i64, i64, i64, String)> = p["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["disc"].as_i64().unwrap(),
                t["track"].as_i64().unwrap(),
                t["new_track"].as_i64().unwrap(),
                t["new_filename"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (1, 1, 1, "01 - Song 1-1.flac".into()),
            (1, 2, 2, "02 - Song 1-2.flac".into()),
            (1, 3, 3, "03 - Song 1-3.flac".into()),
            (2, 1, 4, "04 - Song 2-1.flac".into()),
            (2, 2, 5, "05 - Song 2-2.flac".into()),
            (3, 1, 6, "06 - Song 3-1.flac".into()),
            (3, 2, 7, "07 - Song 3-2.flac".into()),
            (3, 3, 8, "08 - Song 3-3.flac".into()),
            (3, 4, 9, "09 - Song 3-4.flac".into()),
        ]
    );
    assert_eq!(p["track_total"], 9);
    assert_eq!(
        p["target_folder"],
        json!({"current": disc_dir(1), "new": "Band - Record [FLAC]"})
    );
    assert_eq!(p["removes_disc_tags"], true);
    let covers: Vec<Value> = p["folders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["covers"].clone())
        .collect();
    assert_eq!(
        covers,
        vec![
            json!([{"name": "cover.jpg", "action": "kept"}]),
            json!([{"name": "cover.jpg", "action": "identical"}]),
            json!([{"name": "cover.jpg", "action": "different"}]),
        ]
    );
    assert_eq!(p["folders"][2]["other_files"], json!(["booklet.pdf"]));
    assert_eq!(p["folders"][0]["destination"], true);

    // Refusals: an unknown finding, and another check's finding.
    let r = call(
        &w.app,
        "GET",
        &format!("/api/checks/{CHECK}/findings/999999/conversion-preview"),
        None,
    )
    .await;
    assert_eq!(r.status, 404);
    assert_eq!(r.json(), json!({"detail": "Finding not found"}));
    w.run("unidentified-album").await;
    let other = w.findings("unidentified-album").await;
    let other_id = other[0]["id"].as_i64().unwrap();
    let r = call(
        &w.app,
        "GET",
        &format!("/api/checks/{CHECK}/findings/{other_id}/conversion-preview"),
        None,
    )
    .await;
    assert_eq!(r.status, 404);
    let r = call(
        &w.app,
        "GET",
        &format!("/api/checks/{CHECK}/findings/abc/conversion-preview"),
        None,
    )
    .await;
    assert_eq!(r.status, 422);

    // Blockers the disk produces, one at a time.
    // The target name already taken by another folder.
    let taken = w.lib.join("Band - Record [FLAC]");
    std::fs::create_dir_all(&taken).unwrap();
    assert_eq!(codes(&w.preview(id).await), vec!["target_taken"]);
    std::fs::remove_dir(&taken).unwrap();
    // A job queued on a member.
    w.conn()
        .execute(
            "INSERT INTO ai_jobs (id, album_id, album_path, job_type, status, created_at) \
             SELECT 'job-1', id, path, 'metadata', 'queued', '2026-01-01T00:00:00Z' \
             FROM albums WHERE disc_number = 2",
            [],
        )
        .unwrap();
    assert_eq!(codes(&w.preview(id).await), vec!["job_active"]);
    w.conn().execute("DELETE FROM ai_jobs", []).unwrap();
    // A gap in disc 2's numbers.
    write_track(&d2, "02 - Song 2-2.flac", "3", Some(2), "Song 2-2");
    let p = w.preview(id).await;
    assert_eq!(codes(&p), vec!["track_numbers"]);
    assert!(
        p["blockers"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Fix the track gaps first"),
        "{p}"
    );
    write_track(&d2, "02 - Song 2-2.flac", "2", Some(2), "Song 2-2");
    // A file tagged with another disc than its folder.
    write_track(&d3, "01 - Song 3-1.flac", "1", Some(1), "Song 3-1");
    assert_eq!(codes(&w.preview(id).await), vec!["disc_mismatch"]);
    write_track(&d3, "01 - Song 3-1.flac", "1", Some(3), "Song 3-1");
    // An unreadable FLAC.
    std::fs::write(d3.join("05 - Broken.flac"), b"not a flac").unwrap();
    assert!(codes(&w.preview(id).await).contains(&"member_unreadable".to_string()));
    std::fs::remove_file(d3.join("05 - Broken.flac")).unwrap();
    // Two titles that differ only in case, under a template without the
    // track number: one name in the set's folder.
    write_track(&d1, "01 - Song 1-1.flac", "1", Some(1), "Intro");
    write_track(&d2, "01 - Song 2-1.flac", "1", Some(2), "INTRO");
    let r = call(
        &w.app,
        "PATCH",
        "/api/settings",
        Some(&json!({"filename_template_single_artist": "{title}"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let p = w.preview(id).await;
    assert_eq!(codes(&p), vec!["name_collision"], "{p}");
    assert_eq!(p["template"], "{title}");
    // A member folder gone from disk since the scan.
    std::fs::remove_dir_all(&d3).unwrap();
    let p = w.preview(id).await;
    assert!(codes(&p).contains(&"member_missing".to_string()), "{p}");
    std::fs::create_dir_all(&d3).unwrap();

    // Nothing in the library was written by any preview (the test's own
    // edits aside).
    let after = tree(&w.lib);
    assert_eq!(
        after.keys().filter(|k| !before.contains_key(*k)).count(),
        0,
        "a preview created files"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_folder_preview() {
    let w = World::new();
    let dir = w.lib.join("Band - Record [FLAC]");
    write_track(&dir, "1-01 A.flac", "1-01", Some(1), "A");
    write_track(&dir, "1-02 B.flac", "1-02", Some(1), "B");
    write_track(&dir, "2-01 C.flac", "2-01", Some(2), "C");
    std::fs::write(dir.join("cover.jpg"), b"front").unwrap();
    w.scan().await;
    let id = w.only_finding().await;
    let before = tree(&w.lib);
    let p = w.preview(id).await;
    assert_eq!(p["state"], "one_folder");
    assert_eq!(p["numbering"], "per_disc");
    assert_eq!(p["blockers"], json!([]), "{p}");
    let rows: Vec<(String, i64, String)> = p["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["filename"].as_str().unwrap().to_string(),
                t["new_track"].as_i64().unwrap(),
                t["new_filename"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("1-01 A.flac".into(), 1, "01 - A.flac".into()),
            ("1-02 B.flac".into(), 2, "02 - B.flac".into()),
            ("2-01 C.flac".into(), 3, "03 - C.flac".into()),
        ]
    );
    assert_eq!(
        p["target_folder"],
        json!({"current": "Band - Record [FLAC]", "new": "Band - Record [FLAC]"})
    );
    // Nothing moves between folders.
    assert_eq!(p["folders"], json!([]));
    assert_eq!(tree(&w.lib), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incomplete_set_preview() {
    let w = World::new();
    disc_folder(&w.lib, 1, 2);
    disc_folder(&w.lib, 3, 2);
    w.scan().await;
    let id = w.only_finding().await;
    let p = w.preview(id).await;
    assert_eq!(p["state"], "incomplete");
    assert_eq!(p["convertible"], false);
    assert_eq!(p["present_discs"], json!([1, 3]));
    assert_eq!(p["absent_discs"], json!([2]));
    assert_eq!(p["members"].as_array().unwrap().len(), 2);
    assert_eq!(p["tracks"], json!([]));
    assert!(
        p["reason"].as_str().unwrap().contains("disc 2 is not"),
        "{p}"
    );

    // The set is no longer one (disc 3 renamed to another album): the
    // preview says there is nothing to convert.
    std::fs::rename(
        w.lib.join(disc_dir(3)),
        w.lib.join("Band - Other (disc 3 of 3) [FLAC]"),
    )
    .unwrap();
    let conn = w.conn();
    conn.execute(
        "UPDATE albums SET dir_album = 'Other' WHERE disc_number = 3",
        [],
    )
    .unwrap();
    // Disc 1 alone is still an incomplete set of its own.
    let p = w.preview(id).await;
    assert_eq!(p["state"], "incomplete");
    assert_eq!(p["present_discs"], json!([1]));
    conn.execute(
        "UPDATE albums SET dir_album = 'Gone' WHERE disc_number = 1",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE albums SET disc_number = NULL, disc_total = NULL WHERE dir_album = 'Gone'",
        [],
    )
    .unwrap();
    let p = w.preview(id).await;
    assert_eq!(p["state"], "not_a_set", "{p}");
    assert!(p["reason"].as_str().is_some());
}
