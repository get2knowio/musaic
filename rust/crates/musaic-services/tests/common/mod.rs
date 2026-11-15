//! Shared helpers for the golden tests against testdata/golden/services/*.json
//! (recorded from the Python app).
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use musaic_db::Db;
use musaic_events::{Bus, Event, FieldValue, Subscriber, Topic};
use musaic_scan::py_resolve;
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// A golden under golden/services/, merged with its private half when available.
pub fn load_golden(name: &str) -> Value {
    musaic_testdata::golden(&format!("services/{name}"))
}

pub fn tags_golden() -> &'static Value {
    static G: OnceLock<Value> = OnceLock::new();
    G.get_or_init(|| musaic_testdata::golden("tags/apply-scenarios.json"))
}

pub fn sha256(b: &[u8]) -> String {
    let d = Sha256::digest(b);
    d.iter().map(|x| format!("{x:02x}")).collect()
}

/// A temp library dir. The services goldens were recorded on a
/// case-insensitive volume (APFS); no scenario in them depends on case
/// folding (none has two names that differ only in case, and albums are
/// discovered in name order on every filesystem), and every one gives the
/// recorded result on a case-sensitive volume (Linux ext4) too. A volume of
/// the other kind is therefore only noted. A scenario added later that does
/// depend on case folding must skip itself on the other kind of volume
/// with a printed reason, as musaic-tags' golden_apply does.
pub fn temp_lib(golden: &Value) -> (tempfile::TempDir, PathBuf) {
    let td = tempfile::tempdir().unwrap();
    let root = py_resolve(td.path());
    fs::write(root.join("CaseProbe"), b"").unwrap();
    let ci = root.join("caseprobe").exists();
    fs::remove_file(root.join("CaseProbe")).unwrap();
    if Some(ci) != golden["case_insensitive_fs"].as_bool() {
        println!(
            "note: golden recorded with case_insensitive_fs={}, this temp volume differs \
             (no scenario depends on it)",
            golden["case_insensitive_fs"]
        );
    }
    let lib = root.join("lib");
    fs::create_dir(&lib).unwrap();
    (td, lib)
}

// ---------- FLAC recipes (the same bytes the golden generator wrote) ----------

pub fn flac_bytes(tags: &[(String, String)], seconds: u64, padding: usize) -> Vec<u8> {
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
    let mut blocks: Vec<(u8, Vec<u8>)> = vec![(0, si), (4, vc)];
    if padding > 0 {
        blocks.push((1, vec![0; padding]));
    }
    let mut out = b"fLaC".to_vec();
    let n = blocks.len();
    for (i, (code, body)) in blocks.into_iter().enumerate() {
        out.push(if i + 1 == n { code | 0x80 } else { code });
        out.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(&body);
    }
    out
}

/// Build one album of the golden's recipe (`{"dir", "files": [...]}`).
pub fn build_album(lib: &Path, album: &Value) {
    let d = lib.join(album["dir"].as_str().unwrap());
    fs::create_dir_all(&d).unwrap();
    for f in album["files"].as_array().unwrap() {
        let p = d.join(f["name"].as_str().unwrap());
        if let Some(t) = f.get("text") {
            fs::write(&p, t.as_str().unwrap()).unwrap();
            continue;
        }
        let tags: Vec<(String, String)> = f["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kv| {
                (
                    kv[0].as_str().unwrap().to_string(),
                    kv[1].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let bytes = flac_bytes(
            &tags,
            f["seconds"].as_u64().unwrap_or(3),
            f["padding"].as_u64().unwrap_or(0) as usize,
        );
        fs::write(&p, bytes).unwrap();
    }
}

// ---------- normalisation (mirror the generator's Norm) ----------

pub struct Norm {
    pub lib: String,
    pub ids: HashMap<String, String>,
}

fn is_now_like(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && b.iter()
            .enumerate()
            .all(|(i, c)| [4, 7, 10, 13, 16, 19].contains(&i) || c.is_ascii_digit())
}

impl Norm {
    pub fn new(lib: &Path) -> Self {
        Norm {
            lib: lib.to_string_lossy().into_owned(),
            ids: HashMap::new(),
        }
    }

    /// Learn the id of `<lib>/<rel>` as `<ID:rel>`.
    pub fn learn_dir(&mut self, rel: &str) {
        let p = py_resolve(&Path::new(&self.lib).join(rel));
        self.ids
            .insert(musaic_core::album_id(&p.to_string_lossy()), rel.to_string());
    }

    pub fn learn_all(&mut self) {
        fn walk(n: &mut Norm, root: &Path, dir: &Path) {
            let Ok(rd) = fs::read_dir(dir) else { return };
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    let rel = e
                        .path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned();
                    n.learn_dir(&rel);
                    walk(n, root, &e.path());
                }
            }
        }
        let root = PathBuf::from(&self.lib);
        walk(self, &root, &root);
    }

    pub fn str(&self, s: &str) -> String {
        if let Some(rel) = self.ids.get(s) {
            return format!("<ID:{rel}>");
        }
        if is_now_like(s) && !s.starts_with("2001-") {
            return "<NOW>".into();
        }
        s.replace(&self.lib, "<LIB>")
    }

    pub fn value(&self, v: &Value) -> Value {
        self.value_k(v, None)
    }

    fn value_k(&self, v: &Value, key: Option<&str>) -> Value {
        match v {
            Value::Number(_) if key == Some("dir_mtime") => json!("<MTIME>"),
            Value::String(s) => Value::String(self.str(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.value_k(x, None)).collect()),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| (k.clone(), self.value_k(x, Some(k))))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

// ---------- database ----------

/// A copy of testdata/db/v16-empty.db (the Python app's freshly bootstrapped
/// database), opened (and migrated to v17) in `dir`.
pub fn fresh_db(dir: &Path) -> PathBuf {
    let dst = dir.join("library.db");
    fs::copy(musaic_testdata::public("db/v16-empty.db"), &dst).unwrap();
    let (db, _) = Db::open(&dst).unwrap();
    drop(db);
    dst
}

pub fn connect(db: &Path) -> Connection {
    let c = Connection::open(db).unwrap();
    c.pragma_update(None, "foreign_keys", "ON").unwrap();
    c
}

fn sql_value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(n) => json!(n),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(_) => unreachable!(),
    }
}

/// The generator's `dump_db`.
pub fn dump_db(conn: &Connection, norm: &Norm, album_columns: &[String]) -> Value {
    let rows = |sql: &str| -> Vec<Value> {
        let mut stmt = conn.prepare(sql).unwrap();
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        stmt.query_map([], |r| {
            let mut o = Map::new();
            for (i, n) in names.iter().enumerate() {
                o.insert(n.clone(), sql_value(r.get_ref(i)?));
            }
            Ok(norm.value(&Value::Object(o)))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    };
    let sort_by = |mut v: Vec<Value>, keys: &[&str]| {
        v.sort_by_key(|r| {
            keys.iter()
                .map(|k| r[*k].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>()
        });
        v
    };
    json!({
        "albums": rows(&format!("SELECT {} FROM albums ORDER BY rowid", album_columns.join(", "))),
        "libraries": rows("SELECT path, scanned_at FROM libraries ORDER BY path"),
        "track_tags": sort_by(rows("SELECT album_id, filename, tag, created_at FROM track_tags"), &["album_id", "filename", "tag"]),
        "track_fingerprints": sort_by(rows("SELECT album_id, filename, fingerprint FROM track_fingerprints"), &["album_id", "filename"]),
    })
}

// ---------- bus ----------

pub fn event_json(ev: &Event) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), json!(ev.name()));
    for (k, v) in ev.fields() {
        o.insert(
            k.into(),
            match v {
                FieldValue::Str(s) => json!(s),
                FieldValue::Int(n) => json!(n),
                FieldValue::StrList(l) => json!(l),
            },
        );
    }
    Value::Object(o)
}

/// A bus with one recorder at the relay's priority (400), like the
/// generator's.
pub fn recording_bus() -> (Arc<Bus>, Arc<Mutex<Vec<Event>>>) {
    let bus = Arc::new(Bus::new());
    let log: Arc<Mutex<Vec<Event>>> = Arc::default();
    let l = Arc::clone(&log);
    bus.register(
        Topic::Event,
        Subscriber::from_fn(move |ev| {
            l.lock().unwrap().push(ev.clone());
            Ok(())
        })
        .priority(400)
        .name("golden_recorder"),
    );
    (bus, log)
}

// ---------- directory description (as the golden generator describes a directory) ----------

/// (ID3 prefix length, metadata blocks, audio offset).
type FlacLayout = (usize, Vec<(u8, Vec<u8>)>, usize);

fn split_flac(data: &[u8]) -> Option<FlacLayout> {
    let mut pos = 0;
    if data.starts_with(b"ID3") {
        let size = data[6..10]
            .iter()
            .fold(0usize, |a, b| (a << 7) | (*b as usize & 0x7f));
        pos = 10 + size;
    }
    if data.get(pos..pos + 4)? != b"fLaC" {
        return None;
    }
    let prefix = pos;
    pos += 4;
    let mut blocks = Vec::new();
    loop {
        let hdr = *data.get(pos)?;
        let n = u32::from_be_bytes([0, data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        blocks.push((hdr & 0x7f, data.get(pos + 4..pos + 4 + n)?.to_vec()));
        pos += 4 + n;
        if hdr & 0x80 != 0 {
            break;
        }
    }
    Some((prefix, blocks, pos))
}

fn le32(b: &[u8], at: usize) -> usize {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

/// The parsed layout of a FLAC, as the generator records it. Pictures carry
/// their data under "_data" (base64-free, test-only).
pub fn describe_flac(data: &[u8]) -> Value {
    let Some((prefix, blocks, audio_offset)) = split_flac(data) else {
        return Value::Null;
    };
    let mut vendor = Value::Null;
    let mut tags = Value::Null;
    let mut pictures = Vec::new();
    let mut md5 = String::new();
    for (code, body) in &blocks {
        match code {
            0 => md5 = body[18..34].iter().map(|x| format!("{x:02x}")).collect(),
            4 if tags.is_null() => {
                let vlen = le32(body, 0);
                vendor = json!(String::from_utf8_lossy(&body[4..4 + vlen]));
                let n = le32(body, 4 + vlen);
                let mut at = 8 + vlen;
                let mut list = Vec::new();
                for _ in 0..n {
                    let l = le32(body, at);
                    let e = String::from_utf8_lossy(&body[at + 4..at + 4 + l]).into_owned();
                    let (k, v) = e.split_once('=').unwrap();
                    list.push(json!([k, v]));
                    at += 4 + l;
                }
                tags = Value::Array(list);
            }
            6 => {
                let ptype = be32(body, 0);
                let ml = be32(body, 4) as usize;
                let mime = String::from_utf8_lossy(&body[8..8 + ml]).into_owned();
                let dl = be32(body, 8 + ml) as usize;
                let desc = String::from_utf8_lossy(&body[12 + ml..12 + ml + dl]).into_owned();
                let at = 12 + ml + dl;
                let (w, h, depth, colors) = (
                    be32(body, at),
                    be32(body, at + 4),
                    be32(body, at + 8),
                    be32(body, at + 12),
                );
                let len = be32(body, at + 16) as usize;
                let pic = &body[at + 20..at + 20 + len];
                pictures.push(
                    json!({"type": ptype, "mime": mime, "desc": desc, "width": w, "height": h,
                    "depth": depth, "colors": colors, "data_len": len, "sha256": sha256(pic)}),
                );
            }
            _ => {}
        }
    }
    json!({
        "id3_prefix_len": prefix,
        "blocks": blocks.iter().map(|(c, b)| json!([c, b.len()])).collect::<Vec<_>>(),
        "audio_offset": audio_offset,
        "audio_sha256": sha256(&data[audio_offset..]),
        "streaminfo_md5": md5,
        "vendor": vendor,
        "tags": tags,
        "pictures": pictures,
    })
}

/// The generator's `describe_lib`.
pub fn describe_lib(lib: &Path) -> Vec<Value> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<Value>) {
        let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if e.file_type().unwrap().is_dir() {
                out.push(json!({"path": rel, "kind": "dir"}));
                walk(root, &p, out);
            } else {
                let data = fs::read(&p).unwrap();
                let meta = fs::metadata(&p).unwrap();
                let name = e.file_name().to_string_lossy().into_owned();
                let mut rec = json!({"path": rel, "kind": "file", "size": data.len(),
                    "sha256": sha256(&data), "readonly": meta.permissions().readonly()});
                if name.to_lowercase().ends_with(".flac") && !name.starts_with("._") {
                    rec["flac"] = describe_flac(&data);
                }
                out.push(rec);
            }
        }
    }
    let mut out = Vec::new();
    walk(lib, lib, &mut out);
    out.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    out
}

// ---------- images ----------

/// PSNR (dB) of two images decoded by the same decoder, both as RGB8.
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let a = image::load_from_memory(a).unwrap().to_rgb8();
    let b = image::load_from_memory(b).unwrap().to_rgb8();
    assert_eq!(a.dimensions(), b.dimensions());
    let mse: f64 = a
        .as_raw()
        .iter()
        .zip(b.as_raw())
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
        .sum::<f64>()
        / a.as_raw().len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

/// Source image bytes by name, from the tags golden.
pub fn image_bytes(name: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(tags_golden()["images"][name]["b64"].as_str().unwrap())
        .unwrap()
}

/// The Python app's normalised JPEG with this sha256, from the tags golden.
pub fn python_normalized(sha: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(tags_golden()["normalized_outputs"][sha].as_str().unwrap())
        .unwrap()
}
