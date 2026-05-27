"""Tests for the duplicate-tracks cluster-preview path.

Mirrors the filename-mismatch plan-preview tests: the Resolve dialog
on the frontend needs a fresh view of the cluster every time it opens,
not the snapshot from when the check ran. These tests pin two
contracts:

  - `derive_cluster` recomputes the cluster against current
    `track_fingerprints`, handling deleted files, partial overlaps,
    and the "no cluster remains" case.
  - `GET /api/checks/duplicate-tracks/findings/{id}/cluster-preview`
    surfaces those branches via stable wire fields the frontend can
    switch on (`is_empty`, `removed_filenames`).
"""
from __future__ import annotations

import json

import pytest

from musaic import db as db_module
from musaic.checks import duplicate_tracks


LIBRARY_PATH = "/fake/library"


@pytest.fixture(autouse=True)
def _clean_db():
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute("DELETE FROM albums")
            conn.execute("DELETE FROM libraries")
            conn.execute("DELETE FROM track_fingerprints")
            conn.execute("DELETE FROM check_findings")
            conn.execute("DELETE FROM check_runs")
    yield


def _ensure_library() -> None:
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                "INSERT OR IGNORE INTO libraries(path) VALUES (?)",
                (LIBRARY_PATH,),
            )


def _insert_album(album_id: str, path: str = "/fake/library/A") -> None:
    _ensure_library()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                """
                INSERT INTO albums (
                    id, library_path, path, dir_name,
                    title, artist, album_artist, year,
                    track_count, sample_track, is_compilation
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    album_id, LIBRARY_PATH, path, "Test Album [FLAC]",
                    "Test Album", "Test Artist", "Test Artist", "1979",
                    3, f"{path}/01.flac", 0,
                ),
            )


def _insert_fingerprint(
    album_id: str,
    filename: str,
    fp: str = "x",
    duration_s: float = 180.0,
    mbid: str | None = None,
) -> None:
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                """
                INSERT INTO track_fingerprints (
                    album_id, filename, fingerprint, duration_s,
                    acoustid_recording_mbid, acoustid_score, last_computed_at
                ) VALUES (?, ?, ?, ?, ?, ?, datetime('now'))
                """,
                (album_id, filename, fp, duration_s, mbid, 0.9),
            )


def _insert_finding(album_id: str, filenames: list[str]) -> int:
    detail = {
        "filenames":       filenames,
        "durations_s":     [180.0] * len(filenames),
        "duplicate_count": len(filenames),
    }
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                """
                INSERT INTO check_findings (
                    check_id, album_id, severity, summary, detail_json,
                    first_seen_at, last_seen_at
                ) VALUES (
                    'duplicate-tracks', ?, 'warn', 'stub', ?,
                    datetime('now'), datetime('now')
                )
                """,
                (album_id, json.dumps(detail)),
            )
            return cur.lastrowid


# ---------- derive_cluster ----------


def test_derive_cluster_returns_none_when_no_fingerprints():
    _insert_album("a1")
    assert duplicate_tracks.derive_cluster("a1", original_filenames=["x.flac"]) is None


def test_derive_cluster_returns_current_cluster_after_one_deleted(monkeypatch):
    """The most common stale case: user trashed one copy via another
    tool, finding still lists 3 names, dialog should show the 2 that
    remain (plus removed_filenames=[the trashed one]).

    monkeypatch chromaprint_similarity so the test doesn't depend on
    real Chromaprint encoding."""
    _insert_album("a1")
    # Originally 3 duplicates → user deleted "03.flac" out of band.
    _insert_fingerprint("a1", "01.flac", fp="aaa", duration_s=180.0, mbid="rec-1")
    _insert_fingerprint("a1", "02.flac", fp="bbb", duration_s=180.0, mbid="rec-1")
    # 03.flac is gone — no row.

    # All fingerprints "match" each other (1.0 similarity).
    monkeypatch.setattr(
        duplicate_tracks.fingerprint_module,
        "chromaprint_similarity",
        lambda a, b: 1.0,
    )

    cluster = duplicate_tracks.derive_cluster(
        "a1", original_filenames=["01.flac", "02.flac", "03.flac"],
    )

    assert cluster is not None
    assert sorted(cluster["filenames"]) == ["01.flac", "02.flac"]
    assert cluster["duplicate_count"] == 2
    assert cluster["recording_mbid"] == "rec-1"


def test_derive_cluster_returns_none_when_cluster_fully_resolved(monkeypatch):
    """User trashed every copy but one — no cluster remains because the
    single survivor isn't a duplicate of anything."""
    _insert_album("a1")
    _insert_fingerprint("a1", "01.flac", fp="aaa", duration_s=180.0)
    monkeypatch.setattr(
        duplicate_tracks.fingerprint_module,
        "chromaprint_similarity",
        lambda a, b: 1.0,
    )
    cluster = duplicate_tracks.derive_cluster(
        "a1", original_filenames=["01.flac", "02.flac", "03.flac"],
    )
    assert cluster is None


def test_derive_cluster_refuses_unrelated_cluster(monkeypatch):
    """If the only current cluster is for a totally different set of
    files (no overlap with the original finding), refuse to silently
    take over the finding's identity — that's a different problem and
    should produce its own finding on the next check run."""
    _insert_album("a1")
    _insert_fingerprint("a1", "alpha.flac", fp="aaa", duration_s=200.0)
    _insert_fingerprint("a1", "beta.flac", fp="bbb", duration_s=200.0)
    monkeypatch.setattr(
        duplicate_tracks.fingerprint_module,
        "chromaprint_similarity",
        lambda a, b: 1.0,
    )
    cluster = duplicate_tracks.derive_cluster(
        "a1", original_filenames=["01.flac", "02.flac"],  # totally different
    )
    assert cluster is None


# ---------- /cluster-preview endpoint ----------


def test_cluster_preview_404_for_missing_finding(client):
    r = client.get("/api/checks/duplicate-tracks/findings/99999/cluster-preview")
    assert r.status_code == 404


def test_cluster_preview_404_for_wrong_check(client):
    _ensure_library()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                """
                INSERT INTO check_findings (
                    check_id, album_id, severity, summary, detail_json,
                    first_seen_at, last_seen_at
                ) VALUES (
                    'no-cover-art', 'a1', 'warn', 'no cover', '{}',
                    datetime('now'), datetime('now')
                )
                """,
            )
            other_fid = cur.lastrowid
    r = client.get(
        f"/api/checks/duplicate-tracks/findings/{other_fid}/cluster-preview"
    )
    assert r.status_code == 404


def test_cluster_preview_returns_is_empty_when_cluster_gone(client, monkeypatch):
    """User trashed every duplicate copy. Preview reports is_empty=True
    and lists every originally-named file under removed_filenames so
    the UI can show a "all gone" hint."""
    _insert_album("a1")
    fid = _insert_finding("a1", ["01.flac", "02.flac", "03.flac"])
    # No track_fingerprints rows for the album → derive_cluster returns
    # None → endpoint reports is_empty.

    r = client.get(f"/api/checks/duplicate-tracks/findings/{fid}/cluster-preview")
    assert r.status_code == 200
    body = r.json()
    assert body["is_empty"] is True
    assert body["album_id"] == "a1"
    assert body["filenames"] == []
    assert sorted(body["removed_filenames"]) == ["01.flac", "02.flac", "03.flac"]


def test_cluster_preview_returns_shrunken_cluster(client, monkeypatch):
    """One sibling already trashed — the preview lists the remaining
    cluster members and notes the trashed file in removed_filenames."""
    _insert_album("a1")
    _insert_fingerprint("a1", "01.flac", fp="aaa", duration_s=180.0, mbid="rec-1")
    _insert_fingerprint("a1", "02.flac", fp="bbb", duration_s=180.0, mbid="rec-1")
    fid = _insert_finding("a1", ["01.flac", "02.flac", "03.flac"])

    monkeypatch.setattr(
        duplicate_tracks.fingerprint_module,
        "chromaprint_similarity",
        lambda a, b: 1.0,
    )

    r = client.get(f"/api/checks/duplicate-tracks/findings/{fid}/cluster-preview")
    assert r.status_code == 200
    body = r.json()
    assert body["is_empty"] is False
    assert sorted(body["filenames"]) == ["01.flac", "02.flac"]
    assert body["duplicate_count"] == 2
    assert body["removed_filenames"] == ["03.flac"]
    assert body["recording_mbid"] == "rec-1"


def test_cluster_preview_handles_finding_with_no_filenames_in_detail(
    client, monkeypatch,
):
    """Defensive: a finding from an old schema (or one whose detail was
    truncated) shouldn't crash the endpoint — it should re-derive
    against whatever's in track_fingerprints and pick the largest
    cluster as a best-effort survivor."""
    _insert_album("a1")
    _insert_fingerprint("a1", "01.flac", fp="aaa", duration_s=180.0)
    _insert_fingerprint("a1", "02.flac", fp="bbb", duration_s=180.0)
    monkeypatch.setattr(
        duplicate_tracks.fingerprint_module,
        "chromaprint_similarity",
        lambda a, b: 1.0,
    )
    fid = _insert_finding("a1", [])  # no original filenames

    r = client.get(f"/api/checks/duplicate-tracks/findings/{fid}/cluster-preview")
    assert r.status_code == 200
    body = r.json()
    assert body["is_empty"] is False
    assert sorted(body["filenames"]) == ["01.flac", "02.flac"]
