"""Tests for the filename-mismatch plan-preview path.

The preview is the read-side counterpart to `execute_renames`: a
Resolve dialog calls it on open so the rename plan reflects the
current state of disk + DB + settings, not whatever the check
happened to capture at finding-creation time.

These tests pin two contracts:
  - `derive_plan` returns the right shape across "album missing",
    "album dir missing on disk", "empty plan", and "real renames"
    branches.
  - `GET /api/checks/filename-mismatch/findings/{id}/plan-preview`
    surfaces those branches via stable wire fields the frontend
    can switch on (`is_empty`, `album_missing`, `error`).
"""
from __future__ import annotations

from typing import Any

import pytest

from musaic import db as db_module
from musaic.checks import filename_mismatch

LIBRARY_PATH = "/fake/library"


@pytest.fixture(autouse=True)
def _clean_db():
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute("DELETE FROM albums")
            conn.execute("DELETE FROM libraries")
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


def _insert_album(album_id: str, path: str, *, title: str = "Foo") -> None:
    _ensure_library()
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute(
                """
                INSERT INTO albums (
                    id, library_path, path, dir_name,
                    title, artist, album_artist, year,
                    track_count, sample_track, is_compilation,
                    disc_number, disc_total
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    album_id, LIBRARY_PATH, path, "Test Artist - Foo [FLAC]",
                    title, "Test Artist", "Test Artist", "1979",
                    3, f"{path}/01.flac", 0,
                    None, None,
                ),
            )


def _insert_finding(album_id: str, detail: dict[str, Any] | None = None) -> int:
    """Insert a filename-mismatch finding and return its id."""
    import json
    detail_json = json.dumps(detail or {"renames": []})
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            cur = conn.execute(
                """
                INSERT INTO check_findings (
                    check_id, album_id, severity, summary, detail_json,
                    first_seen_at, last_seen_at
                ) VALUES (
                    'filename-mismatch', ?, 'info', 'stub', ?,
                    datetime('now'), datetime('now')
                )
                """,
                (album_id, detail_json),
            )
            return cur.lastrowid


# ---------- derive_plan ----------


def test_derive_plan_album_not_in_cache_returns_none():
    assert filename_mismatch.derive_plan("nonexistent") is None


def test_derive_plan_album_dir_missing_on_disk(tmp_path):
    album_id = "abc123"
    missing_path = str(tmp_path / "does-not-exist")
    _insert_album(album_id, missing_path)

    plan = filename_mismatch.derive_plan(album_id)

    assert plan is not None
    assert plan["album_missing"] is True
    assert plan["renames"] == []
    assert plan["dir_rename"] is None
    assert plan["headline"]  # has *something* identifying the album


def test_derive_plan_empty_when_files_already_match(tmp_path, monkeypatch):
    album_id = "abc123"
    album_path = tmp_path / "Test Artist - Foo [FLAC]"
    album_path.mkdir()
    _insert_album(album_id, str(album_path))

    # No FLAC files at all → _check_album returns [], _check_dir compares
    # the current dir name to the rendered template (which depends on
    # settings — stub it to match the current name so dir_rename is None).
    monkeypatch.setattr(filename_mismatch, "_check_album", lambda *a, **kw: [])
    monkeypatch.setattr(filename_mismatch, "_check_dir", lambda *a, **kw: None)

    plan = filename_mismatch.derive_plan(album_id)

    assert plan is not None
    assert plan["renames"] == []
    assert plan["dir_rename"] is None
    # Sanity: the template strings come through so the dialog can
    # still render the "this is the template we're checking against"
    # context even on an empty preview.
    assert "template" in plan
    assert "dir_template" in plan


def test_derive_plan_propagates_real_renames(tmp_path, monkeypatch):
    album_id = "abc123"
    album_path = tmp_path / "Test Artist - Foo [FLAC]"
    album_path.mkdir()
    _insert_album(album_id, str(album_path))

    fake_renames = [{
        "filename": "01-bad-name.flac",
        "expected": "01 - Track One.flac",
        "tracknumber": "1",
        "title": "Track One",
        "artist": "Test Artist",
        "collides_with": None,
    }]
    fake_dir_rename = {"old": "wrong dir", "new": "right dir"}
    monkeypatch.setattr(
        filename_mismatch, "_check_album", lambda *a, **kw: fake_renames,
    )
    monkeypatch.setattr(
        filename_mismatch, "_check_dir", lambda *a, **kw: fake_dir_rename,
    )

    plan = filename_mismatch.derive_plan(album_id)

    assert plan is not None
    assert plan["renames"] == fake_renames
    assert plan["dir_rename"] == fake_dir_rename
    assert plan["headline"]


# ---------- /plan-preview endpoint ----------


def test_plan_preview_endpoint_404_for_missing_finding(client):
    r = client.get("/api/checks/filename-mismatch/findings/99999/plan-preview")
    assert r.status_code == 404


def test_plan_preview_endpoint_404_for_wrong_check(client):
    """If a finding id belongs to a different check, plan-preview must
    refuse — otherwise a misrouted request could leak internal state."""
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

    r = client.get(f"/api/checks/filename-mismatch/findings/{other_fid}/plan-preview")
    assert r.status_code == 404


def test_plan_preview_endpoint_returns_is_empty_when_no_work(
    client, tmp_path, monkeypatch,
):
    album_id = "abc123"
    album_path = tmp_path / "Test Artist - Foo [FLAC]"
    album_path.mkdir()
    _insert_album(album_id, str(album_path))
    fid = _insert_finding(album_id)

    monkeypatch.setattr(filename_mismatch, "_check_album", lambda *a, **kw: [])
    monkeypatch.setattr(filename_mismatch, "_check_dir", lambda *a, **kw: None)

    r = client.get(f"/api/checks/filename-mismatch/findings/{fid}/plan-preview")
    assert r.status_code == 200
    body = r.json()
    assert body["is_empty"] is True
    assert body["album_id"] == album_id
    assert body["renames"] == []
    assert body["dir_rename"] is None


def test_plan_preview_endpoint_returns_album_missing_for_cache_drop(client):
    """When the finding still exists but the album row has been deleted
    (renamed away, library reconfigured, etc.), surface
    album_missing+is_empty so the dialog clears the stale finding."""
    fid = _insert_finding("ghost-album-id")  # no matching albums row

    r = client.get(f"/api/checks/filename-mismatch/findings/{fid}/plan-preview")
    assert r.status_code == 200
    body = r.json()
    assert body["is_empty"] is True
    assert body["album_missing"] is True
    assert body["album_id"] == "ghost-album-id"


def test_plan_preview_endpoint_returns_fresh_plan_not_finding_detail(
    client, tmp_path, monkeypatch,
):
    """The whole point of the preview: it must re-derive against disk,
    NOT echo back whatever was in finding.detail at check-run time.

    Set the finding's stored detail to something obviously wrong, then
    verify the response carries the freshly-derived plan instead."""
    album_id = "abc123"
    album_path = tmp_path / "Test Artist - Foo [FLAC]"
    album_path.mkdir()
    _insert_album(album_id, str(album_path))

    stale_detail = {
        "renames": [{
            "filename":  "STALE.flac",
            "expected":  "STALE-EXPECTED.flac",
            "tracknumber": "99",
            "title":     "stale",
            "artist":    "stale",
            "collides_with": None,
        }],
        "dir_rename": {"old": "stale-old", "new": "stale-new"},
    }
    fid = _insert_finding(album_id, detail=stale_detail)

    fresh_renames = [{
        "filename": "fresh.flac",
        "expected": "01 - Fresh.flac",
        "tracknumber": "1",
        "title": "Fresh",
        "artist": "Test Artist",
        "collides_with": None,
    }]
    monkeypatch.setattr(
        filename_mismatch, "_check_album", lambda *a, **kw: fresh_renames,
    )
    monkeypatch.setattr(filename_mismatch, "_check_dir", lambda *a, **kw: None)

    r = client.get(f"/api/checks/filename-mismatch/findings/{fid}/plan-preview")
    assert r.status_code == 200
    body = r.json()
    # The preview reflects what's on disk RIGHT NOW (the mock), not the
    # finding's frozen detail snapshot.
    assert body["renames"] == fresh_renames
    assert body["dir_rename"] is None
    assert body["is_empty"] is False
