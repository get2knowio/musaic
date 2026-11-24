//! The kept `/api/ai/jobs` routes over the single research store (FR-029):
//! list, enqueue, get, active-by-album, delete one and Clear by status
//! answer from research requests and the history of old jobs, in
//! `job_to_py`'s shape, and the request bodies `POST /api/ai/jobs` and
//! `DELETE /api/ai/jobs` refuse with 422 are the Python app's bytes
//! (golden/ai/http.json, recorded from the Python app's queue routes).
//! A library built from that golden's recipes, network off.

mod common;
mod http_world;

use axum::body::Body;
use axum::http::Request;
use http_world::World;
use musaic_db::research_history::{self, JobRow};
use serde_json::{Value, json};

fn golden() -> Value {
    musaic_testdata::golden("ai/http.json")
}

/// The recorded case with this label.
fn case<'g>(g: &'g Value, label: &str) -> &'g Value {
    g["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["label"] == label)
        .unwrap_or_else(|| panic!("no case {label}"))
}

fn keys(v: &Value) -> Vec<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

async fn world() -> World {
    // The recipes' cover images (`b64` files) play no part here.
    let mut g = golden();
    for a in g["albums"].as_array_mut().unwrap() {
        a["files"]
            .as_array_mut()
            .unwrap()
            .retain(|f| f.get("b64").is_none());
    }
    let w = World::new(&g, &[], |b| b);
    w.configure_and_scan().await;
    w
}

/// A recorded request, raw bodies included.
async fn send(w: &World, req: &Value) -> common::Got {
    let Some(raw) = req.get("raw").and_then(Value::as_str) else {
        return w.send(req).await;
    };
    let r = Request::builder()
        .method(req["method"].as_str().unwrap())
        .uri(req["path"].as_str().unwrap())
        .header("content-type", "application/json")
        .body(Body::from(raw.to_string()))
        .unwrap();
    common::send(&w.app, r).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_bodies_are_the_python_apps_bytes() {
    let g = golden();
    let w = world().await;
    let mut seen = 0;
    for c in g["cases"].as_array().unwrap() {
        let Some(req) = c.get("request") else {
            continue;
        };
        if c["response"]["status"] != 422 || req["path"] != "/api/ai/jobs" {
            continue;
        }
        let got = send(&w, req).await;
        assert_eq!(got.status, 422, "{}: {}", c["label"], got.text());
        assert_eq!(
            got.text(),
            c["response"]["text"].as_str().unwrap(),
            "{}",
            c["label"]
        );
        seen += 1;
    }
    // Seven enqueue bodies and the bulk delete with no status.
    assert_eq!(seen, 7);
    // Nothing was made by any of them.
    let list = w.get("/api/ai/jobs").await;
    assert_eq!(list.text(), r#"{"jobs":[]}"#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_kept_routes_answer_from_the_single_store() {
    let g = golden();
    let w = world().await;
    let alpha = w.id("Alpha - One [FLAC]");
    let recorded = &case(&g, "enqueue A metadata")["response"]["json"];

    // Enqueue: a research request, shown as one job in the old shape.
    let body = json!({"album_id": alpha, "job_type": "metadata"});
    let made = w.post("/api/ai/jobs", Some(&body)).await;
    assert_eq!(made.status, 200, "{}", made.text());
    let job = made.json();
    assert_eq!(keys(&job), keys(recorded));
    for k in ["album_id", "album_title", "job_type", "hint", "status"] {
        let want = w.unnorm_value(&recorded[k]);
        assert_eq!(job[k], want, "{k}");
    }
    let id = job["id"].as_str().unwrap().to_string();
    {
        let conn = w.conn();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM delegated_work WHERE group_id=?1 AND album_id=?2",
                [&id, &alpha],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "one metadata request");
    }
    // The same research again joins the request already there.
    let again = w.post("/api/ai/jobs", Some(&body)).await;
    assert_eq!((again.status, again.json()["id"].clone()), (200, json!(id)));

    // Get, active by album, list.
    let got = w.get(&format!("/api/ai/jobs/{id}")).await;
    assert_eq!((got.status, got.json()), (200, job.clone()));
    let active = w.get(&format!("/api/ai/jobs/active/{alpha}")).await;
    assert_eq!(active.json(), job);
    let none = w.get("/api/ai/jobs/active/nope").await;
    assert_eq!(none.text(), case(&g, "active unknown")["response"]["text"]);
    let list = w.get("/api/ai/jobs").await.json();
    assert_eq!(list, json!({"jobs": [job.clone()]}));
    let unknown = w.get("/api/ai/jobs/nope").await;
    assert_eq!(
        (unknown.status, unknown.text()),
        (
            404,
            case(&g, "get unknown")["response"]["text"]
                .as_str()
                .unwrap()
                .to_string()
        )
    );

    // An unresolved request is cancelled, never deleted.
    let del = common::call(&w.app, "DELETE", &format!("/api/ai/jobs/{id}"), None).await;
    assert_eq!(del.status, 409, "{}", del.text());
    for status in ["queued", "completed"] {
        let r = common::call(
            &w.app,
            "DELETE",
            &format!("/api/ai/jobs?status={status}"),
            None,
        )
        .await;
        assert_eq!(r.status, 409, "{status}: {}", r.text());
    }
    for label in ["bulk delete running", "bulk delete empty status"] {
        let c = case(&g, label);
        let status = c["request"]["params"]["status"].as_str().unwrap();
        let path = format!("/api/ai/jobs?status={}", status.replace(' ', "%20"));
        let r = common::call(&w.app, "DELETE", &path, None).await;
        assert_eq!(
            (r.status, r.text().as_str()),
            (
                c["response"]["status"].as_u64().unwrap() as u16,
                c["response"]["text"].as_str().unwrap()
            ),
            "{label}"
        );
    }

    // Old jobs as the v24 step folds them: one a request names, one
    // history-only record of a type no request kind matches.
    let bravo = w.id("Bravo - Two [FLAC]");
    {
        let conn = w.conn();
        for (id, job_type) in [("old-1", "metadata"), ("old-2", "lyrics")] {
            let mut j = JobRow::seed(id, &bravo, job_type, "failed", "2026-01-01T00:00:00Z");
            j.album_title = "Two".into();
            j.error = Some("boom".into());
            research_history::import_legacy_job(&conn, &j).unwrap();
        }
    }
    let failed = w.get("/api/ai/jobs?status=failed").await.json();
    let jobs = failed["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 2, "{failed}");
    for j in jobs {
        assert_eq!(keys(j), keys(recorded));
        assert_eq!(j["album_id"], json!(bravo));
    }
    let history = jobs.iter().find(|j| j["id"] == "old-2").expect("history");
    assert_eq!(
        (history["job_type"].clone(), history["error"].clone()),
        (json!("lyrics"), json!("boom"))
    );
    let old = w.get("/api/ai/jobs/old-1").await.json();
    assert_eq!(
        (old["status"].clone(), old["album_title"].clone()),
        (json!("failed"), json!("Two"))
    );

    // Clear failed: the request and the history-only record.
    let r = common::call(&w.app, "DELETE", "/api/ai/jobs?status=failed", None).await;
    assert_eq!(
        (r.status, r.text()),
        (
            200,
            r#"{"deleted_count":2,"statuses":["failed"]}"#.to_string()
        )
    );
    let gone = w.get("/api/ai/jobs/old-2").await;
    assert_eq!(gone.status, 404);
    let list = w.get("/api/ai/jobs").await.json();
    assert_eq!(list, json!({"jobs": [job]}));
    let unknown = common::call(&w.app, "DELETE", "/api/ai/jobs/nope", None).await;
    assert_eq!(
        unknown.text(),
        case(&g, "delete unknown")["response"]["text"]
            .as_str()
            .unwrap()
    );
}

// FR-036: Cancel on a card cancels the research behind it, through
// `POST /api/ai/jobs/{job_id}/cancel`; a proposal awaiting review is left.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_card_cancels_its_waiting_research() {
    let w = world().await;
    let alpha = w.id("Alpha - One [FLAC]");
    let body = json!({"album_id": alpha, "job_type": "metadata+cover"});
    let made = w.post("/api/ai/jobs", Some(&body)).await;
    assert_eq!(made.status, 200, "{}", made.text());
    let id = made.json()["id"].as_str().unwrap().to_string();
    let review: String = {
        let conn = w.conn();
        let first: String = conn
            .query_row(
                "SELECT id FROM delegated_work WHERE group_id=?1 ORDER BY kind LIMIT 1",
                [&id],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE delegated_work SET state='awaiting_review' WHERE id=?1",
            [&first],
        )
        .unwrap();
        first
    };
    let cancelled = w.post(&format!("/api/ai/jobs/{id}/cancel"), None).await;
    assert_eq!(cancelled.status, 200, "{}", cancelled.text());
    assert_eq!(cancelled.json(), json!({"id": id, "cancelled": 1}));
    let states: Vec<(String, String)> = {
        let conn = w.conn();
        let mut stmt = conn
            .prepare("SELECT id, state FROM delegated_work WHERE group_id=?1")
            .unwrap();
        stmt.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    for (work, state) in &states {
        let want = if *work == review {
            "awaiting_review"
        } else {
            "cancelled"
        };
        assert_eq!(state, want, "{work}");
    }
    // Nothing waiting or held now; an unknown card.
    let again = w.post(&format!("/api/ai/jobs/{id}/cancel"), None).await;
    assert_eq!(again.status, 409, "{}", again.text());
    let unknown = w.post("/api/ai/jobs/nope/cancel", None).await;
    assert_eq!(unknown.status, 404, "{}", unknown.text());
    assert_eq!(unknown.json(), json!({"detail": "Job not found"}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_batch_routes_list_read_and_cancel() {
    let w = world().await;
    let none = w.get("/api/research/batches").await;
    assert_eq!((none.status, none.json()), (200, json!([])));
    let missing = w
        .get("/api/research/batches/00000000-0000-4000-8000-000000000000")
        .await;
    assert_eq!(missing.status, 404);
    assert_eq!(missing.json(), json!({"detail": "Batch not found"}));
    let cancel = w
        .post(
            "/api/research/batches/00000000-0000-4000-8000-000000000000/cancel",
            None,
        )
        .await;
    assert_eq!(cancel.status, 404);

    // A batch, as a bulk run makes it.
    let alpha = w.id("Alpha - One [FLAC]");
    let batch = "6f0c2a3e-1b2c-4d5e-8f90-0123456789ab";
    {
        let conn = w.conn();
        let epoch = musaic_db::delegated_work::epoch(&conn).unwrap();
        conn.execute(
            "INSERT INTO work_batches(id,library_epoch,check_id,started_at,finished_at)
             VALUES (?1,?2,'unidentified-album',1800000000000,1800000060000)",
            rusqlite::params![batch, epoch],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO delegated_work
               (id,group_id,kind,album_id,library_epoch,base_revision,state,input_json,
                input_digest,created_at,updated_at,available_at,max_attempts,batch_id)
             VALUES ('0b1c2d3e-0000-4000-8000-000000000001','g1','metadata.identify',?1,?2,
                     'rev','queued','{}','d1',1800000000000,1800000000000,1800000000000,3,?3)",
            rusqlite::params![alpha, epoch, batch],
        )
        .unwrap();
    }
    let list = w.get("/api/research/batches").await.json();
    assert_eq!(
        list,
        json!([{
            "batchId": batch, "checkId": "unidentified-album",
            "startedAt": "2027-01-15T08:00:00Z", "finishedAt": "2027-01-15T08:01:00Z",
            "unfinished": false, "size": 1,
            "counts": {"queued": 1, "claimed": 0, "running": 0, "awaiting_review": 0,
                       "applied": 0, "discarded": 0, "failed": 0, "cancelled": 0}
        }])
    );
    let detail = w
        .get(&format!("/api/research/batches/{batch}"))
        .await
        .json();
    assert_eq!(detail["items"][0]["batchId"], json!(batch));
    assert_eq!(detail["items"][0]["groupId"], json!("g1"));
    assert_eq!(detail["items"][0]["albumId"], json!(alpha));
    let work = w.get("/api/research/work").await.json();
    assert_eq!(work[0]["batchId"], json!(batch));
    // A card made only of batched requests is not in the jobs list.
    assert_eq!(w.get("/api/ai/jobs").await.json(), json!({"jobs": []}));
    let done = w
        .post(&format!("/api/research/batches/{batch}/cancel"), None)
        .await;
    assert_eq!(done.status, 200, "{}", done.text());
    let done = done.json();
    assert_eq!(
        (
            done["cancelled"].clone(),
            done["counts"]["cancelled"].clone()
        ),
        (json!(1), json!(1))
    );
}
