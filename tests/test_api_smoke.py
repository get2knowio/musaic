"""End-to-end smoke coverage for the API surface.

Each test exercises a router slice through the real FastAPI app +
SQLite cache, with no network or filesystem I/O beyond the per-session
tmp config dir. FLAC-content paths (scan, apply, AI worker) are out of
scope — they need binary fixtures and live separately.
"""
from __future__ import annotations


def _all_route_paths(routes) -> set[str]:
    """Collect route paths, including nested FastAPI _IncludedRouter wrappers."""
    paths: set[str] = set()
    for route in routes:
        path = getattr(route, "path", None)
        if path:
            paths.add(path)
        nested = getattr(route, "original_router", None)
        if nested is not None:
            paths.update(_all_route_paths(getattr(nested, "routes", [])))
    return paths


def test_config_roundtrip(client, empty_library):
    # Pre-state varies (config dir is session-shared); just verify the
    # endpoint responds. Then point it at a known good directory.
    r = client.get("/api/config")
    assert r.status_code == 200

    r = client.post("/api/config", json={"library_dir": str(empty_library)})
    assert r.status_code == 200
    body = r.json()
    assert body["library_dir"] == str(empty_library.resolve())
    assert body["exists"] is True

    # Bad path → 400
    r = client.post("/api/config", json={"library_dir": "/no/such/dir/here"})
    assert r.status_code == 400


def test_albums_listing_empty(client, empty_library):
    client.post("/api/config", json={"library_dir": str(empty_library)})
    r = client.get("/api/albums")
    assert r.status_code == 200
    body = r.json()
    assert body["count"] == 0
    assert body["albums"] == []


def test_settings_patch_roundtrip(client):
    r = client.get("/api/settings")
    assert r.status_code == 200
    initial = r.json()
    assert "ai_max_parallel" in initial

    # Patch + verify
    new_max = 1 if initial.get("ai_max_parallel") != 1 else 2
    r = client.patch("/api/settings", json={"ai_max_parallel": new_max})
    assert r.status_code == 200
    assert r.json()["ai_max_parallel"] == new_max

    # Out-of-range rejected
    r = client.patch("/api/settings", json={"ai_max_parallel": 99})
    assert r.status_code == 400

    # Restore
    client.patch("/api/settings", json={"ai_max_parallel": initial["ai_max_parallel"]})


def test_vocabulary_crud(client):
    tag = "smoke-test-tag-vocab-crud"

    # Add
    r = client.post("/api/vocabulary", json={"tag": tag, "description": "test"})
    assert r.status_code == 200
    vocab = {v["tag"] for v in r.json()["vocabulary"]}
    assert tag in vocab

    # Patch description
    r = client.patch(f"/api/vocabulary/{tag}", json={"description": "updated"})
    assert r.status_code == 200
    entry = next(v for v in r.json()["vocabulary"] if v["tag"] == tag)
    assert entry["description"] == "updated"

    # Rename
    new_tag = f"{tag}-renamed"
    r = client.post(f"/api/vocabulary/{tag}/rename", json={"new_tag": new_tag})
    assert r.status_code == 200
    vocab = {v["tag"] for v in r.json()["vocabulary"]}
    assert new_tag in vocab and tag not in vocab

    # Delete
    r = client.delete(f"/api/vocabulary/{new_tag}")
    assert r.status_code == 200
    vocab = {v["tag"] for v in r.json()["vocabulary"]}
    assert new_tag not in vocab


def test_vocabulary_validation(client):
    r = client.post("/api/vocabulary", json={"tag": ""})
    assert r.status_code == 400
    r = client.post("/api/vocabulary/missing-tag/rename", json={"new_tag": ""})
    assert r.status_code == 400


def test_db_info_and_snapshots(client):
    r = client.get("/api/db/info")
    assert r.status_code == 200
    info = r.json()
    assert "size_bytes" in info or "path" in info  # whatever db_info returns

    r = client.get("/api/db/snapshots")
    assert r.status_code == 200
    assert isinstance(r.json()["snapshots"], list)


def test_health_checks_index(client):
    r = client.get("/api/checks")
    assert r.status_code == 200
    body = r.json()
    assert isinstance(body["checks"], list)
    # Sanity: known checks are registered. id lives under meta.
    ids = {c["meta"]["id"] for c in body["checks"]}
    assert "tags-out-of-sync" in ids
    assert "unidentified-album" in ids
    assert "no-cover-art" in ids


def test_health_findings_endpoint(client):
    # Open findings list — should respond, may be empty.
    r = client.get("/api/checks/findings")
    assert r.status_code == 200
    assert "findings" in r.json()


def test_health_check_detail_404(client):
    r = client.get("/api/checks/this-check-does-not-exist")
    assert r.status_code == 404


def test_jobs_listing_empty(client):
    r = client.get("/api/ai/jobs")
    assert r.status_code == 200
    assert isinstance(r.json()["jobs"], list)


def test_jobs_enqueue_unknown_album(client, empty_library):
    client.post("/api/config", json={"library_dir": str(empty_library)})
    # Album isn't in cache → 404
    r = client.post(
        "/api/ai/jobs",
        json={"album_id": "nonexistent", "job_type": "metadata"},
    )
    assert r.status_code == 404


def test_jobs_enqueue_invalid_type(client, empty_library):
    client.post("/api/config", json={"library_dir": str(empty_library)})
    r = client.post(
        "/api/ai/jobs",
        json={"album_id": "anything", "job_type": "not-a-real-type"},
    )
    assert r.status_code == 400


def test_tags_endpoints_basic(client):
    r = client.get("/api/tags")
    assert r.status_code == 200
    assert "tags" in r.json()

    r = client.get("/api/tags/no-such-tag/tracks")
    assert r.status_code == 200
    assert r.json()["tracks"] == []


def test_resolve_album_without_library(client):
    """Hitting an album endpoint with no library configured returns 409
    via resolve_album_dict."""
    # Wipe library config so we hit the no-library branch.
    from musaic import config as config_module
    cfg_path = config_module.CONFIG_FILE
    backed_up = cfg_path.read_text() if cfg_path.exists() else None
    try:
        if cfg_path.exists():
            cfg_path.unlink()
        r = client.get("/api/albums/some-id/tracks")
        assert r.status_code == 409
    finally:
        if backed_up is not None:
            cfg_path.write_text(backed_up)


def test_spa_fallback_for_unknown_path(client):
    """Unknown non-/api paths fall back to index.html (the SPA router
    handles them). Skipped gracefully if the React build isn't present."""
    from musaic.main import FRONTEND_DIR
    if not (FRONTEND_DIR / "index.html").is_file():
        import pytest
        pytest.skip("frontend/dist not built")
    r = client.get("/library/some-album-id")
    assert r.status_code == 200
    assert b"<div id=\"root\">" in r.content


def test_unknown_api_path_returns_404(client):
    r = client.get("/api/this-does-not-exist")
    assert r.status_code == 404


def test_scan_status_endpoint(client):
    r = client.get("/api/scan/status")
    assert r.status_code == 200
    body = r.json()
    assert body["status"] in ("idle", "running", "done", "error")


def test_scan_stream_against_empty_library(client, empty_library):
    """End-to-end scan smoke. The scan now runs as a background task so
    a client disconnect doesn't kill it; this test verifies the stream
    still delivers a complete event log to a connected client."""
    import json as _json
    client.post("/api/config", json={"library_dir": str(empty_library)})

    events: list[tuple[str, dict]] = []
    with client.stream("GET", "/api/scan/stream?force=true") as resp:
        assert resp.status_code == 200
        current_event = None
        for raw_line in resp.iter_lines():
            line = raw_line if isinstance(raw_line, str) else raw_line.decode()
            if line.startswith("event: "):
                current_event = line[len("event: "):].strip()
            elif line.startswith("data: ") and current_event:
                events.append((current_event, _json.loads(line[len("data: "):])))
                if current_event in ("done", "error"):
                    break
                current_event = None

    names = [e[0] for e in events]
    assert "start" in names
    assert names[-1] == "done", f"got events: {names}"


def test_delete_album_endpoint_404_for_unknown(client, empty_library):
    """DELETE /api/albums/{id} returns 404 when the album isn't in the
    cache (no row to delete). The endpoint is the only post-v15 path
    that reaches `cache.delete_album` + its FK cascade — 404 is the
    safe answer for a misspelled / already-removed id."""
    client.post("/api/config", json={"library_dir": str(empty_library)})
    r = client.delete("/api/albums/nonexistent-album-id")
    assert r.status_code == 404


def test_fingerprint_pool_status_endpoint(client):
    r = client.get("/api/fingerprint-pool/status")
    assert r.status_code == 200
    body = r.json()
    assert body["status"] in (
        "idle", "running", "done", "cancelled", "unavailable",
    )


def test_library_events_stream_endpoint_registered(client):
    """The /api/library-events/stream route exists. Full stream
    behaviour is covered by test_library_events_relay (relay + queue
    contract) — the TestClient + httpx + persistent SSE combo hangs
    in pytest when there's no terminal event the server can send to
    cleanly close, so we keep this layer to wire-presence only."""
    from musaic.main import app
    paths = _all_route_paths(app.routes)
    assert "/api/library-events/stream" in paths


def test_fingerprint_pool_stream_endpoint_registered(client):
    """Same wire-presence check for /api/fingerprint-pool/stream —
    state-change semantics are pinned in the fingerprint_pool unit
    tests; this guards against router de-registration."""
    from musaic.main import app
    paths = _all_route_paths(app.routes)
    assert "/api/fingerprint-pool/stream" in paths
