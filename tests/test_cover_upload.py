"""Tests for the manual cover-upload endpoint.

Generates a real album with silent FLACs, posts a tiny PNG via
multipart, and verifies the bytes were normalized to JPEG and embedded
across every track.
"""
from __future__ import annotations

import io
import json
from pathlib import Path

import numpy as np
import soundfile as sf
from mutagen.flac import FLAC
from PIL import Image


def _make_album(library: Path, *, dir_name: str, track_count: int) -> Path:
    album_dir = library / dir_name
    album_dir.mkdir()
    sr = 22050
    samples = np.zeros((sr, 2), dtype="float32")
    for i in range(1, track_count + 1):
        path = album_dir / f"{i:02d} - Track {i}.flac"
        sf.write(str(path), samples, sr, format="FLAC")
    return album_dir


def _seed_cache(client, library: Path) -> dict:
    from musaic import cache as cache_module
    from musaic.scanner import album_to_dict, scan_albums_list

    r = client.post("/api/config", json={"library_dir": str(library)})
    assert r.status_code == 200, r.text

    albums = scan_albums_list(str(library))
    assert len(albums) >= 1
    albums_by_path = {a.path: album_to_dict(a) for a in albums}
    snap = cache_module.LibrarySnapshot(
        library_dir=str(library),
        scanned_at="2026-05-08T00:00:00Z",
        albums_by_path=albums_by_path,
    )
    cache_module.save_snapshot(snap)

    r = client.get("/api/albums")
    return r.json()["albums"][0]


def _png_bytes(*, width: int = 64, height: int = 64, color: tuple[int, int, int] = (200, 50, 50)) -> bytes:
    """Build a tiny PNG in memory so tests don't need a fixture file."""
    img = Image.new("RGB", (width, height), color=color)
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def _read_sse_events(resp) -> list[tuple[str, dict]]:
    events: list[tuple[str, dict]] = []
    pending_event = None
    for line in resp.iter_lines():
        if line.startswith("event: "):
            pending_event = line.removeprefix("event: ")
        elif line.startswith("data: "):
            data = json.loads(line.removeprefix("data: "))
            assert pending_event is not None
            events.append((pending_event, data))
            pending_event = None
    return events


def test_cover_upload_embeds_jpeg_across_all_tracks(client, empty_library):
    _make_album(empty_library, dir_name="Test Artist - Cover Upload [FLAC]", track_count=3)
    album = _seed_cache(client, empty_library)
    album_id = album["id"]

    png_bytes = _png_bytes()
    files = {"cover": ("eBay-grab.png", png_bytes, "image/png")}

    with client.stream(
        "POST",
        f"/api/albums/{album_id}/cover/upload",
        files=files,
    ) as resp:
        assert resp.status_code == 200
        events = _read_sse_events(resp)

    names = [n for n, _ in events]
    assert "cover_downloaded" in names  # the normalize/decode event
    assert "cover_saved" in names        # cover.jpg sidecar
    assert names.count("file_done") == 3
    assert "updated_album" in names

    # Sidecar landed on disk — pull the actual path from the
    # cover_saved event since it's the authoritative source.
    saved_event = next((p for n, p in events if n == "cover_saved"), None)
    assert saved_event is not None
    sidecar = Path(saved_event["path"])
    parent_listing = sorted(p.name for p in sidecar.parent.iterdir())
    assert sidecar.is_file(), (
        f"cover.jpg missing at {sidecar}; dir contents: {parent_listing}"
    )
    sidecar_bytes = sidecar.read_bytes()
    # JPEG magic — confirms we normalized away from PNG.
    assert sidecar_bytes[:3] == b"\xff\xd8\xff"

    # Every FLAC has the embedded picture and it's our normalized JPEG.
    for path in sorted(Path(album["path"]).glob("*.flac")):
        audio = FLAC(str(path))
        pics = audio.pictures
        assert len(pics) == 1
        assert pics[0].mime == "image/jpeg"
        assert pics[0].data[:3] == b"\xff\xd8\xff"


def test_cover_upload_rejects_non_image(client, empty_library):
    _make_album(empty_library, dir_name="Test Artist - Reject [FLAC]", track_count=1)
    album = _seed_cache(client, empty_library)

    files = {"cover": ("not-an-image.txt", b"hello", "text/plain")}
    r = client.post(
        f"/api/albums/{album['id']}/cover/upload",
        files=files,
    )
    assert r.status_code == 400
    assert "image" in r.json()["detail"].lower()


def test_cover_upload_rejects_empty_file(client, empty_library):
    _make_album(empty_library, dir_name="Test Artist - Empty [FLAC]", track_count=1)
    album = _seed_cache(client, empty_library)

    files = {"cover": ("empty.png", b"", "image/png")}
    r = client.post(
        f"/api/albums/{album['id']}/cover/upload",
        files=files,
    )
    assert r.status_code == 400
    assert "empty" in r.json()["detail"].lower()


def test_cover_upload_404_for_unknown_album(client, empty_library):
    # Configure library so we get past the no-library guard.
    _make_album(empty_library, dir_name="Test Artist - 404 [FLAC]", track_count=1)
    _seed_cache(client, empty_library)

    files = {"cover": ("c.png", _png_bytes(), "image/png")}
    r = client.post(
        "/api/albums/nonexistent_album/cover/upload",
        files=files,
    )
    assert r.status_code == 404
