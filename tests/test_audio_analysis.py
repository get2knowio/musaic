"""Tests for the audio-analysis pipeline.

Generates real (silent) FLAC files via soundfile so mutagen can read +
write Vorbis comments. The expensive bits (`librosa.beat.beat_track`,
`pyloudnorm.Meter.integrated_loudness`) are stubbed via the
`_detect_bpm` / `_measure_track_loudness` seams in the service module
— the tests verify the surrounding tag-writing + event-yielding glue,
not the underlying DSP.
"""
from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pytest
import soundfile as sf
from mutagen.flac import FLAC

from musaic import config as config_module
from musaic.services import audio_analysis


def _make_album(library: Path, *, dir_name: str, track_count: int) -> Path:
    """Create an album directory with `track_count` short silent FLACs."""
    album_dir = library / dir_name
    album_dir.mkdir()
    sr = 22050
    samples = np.zeros((sr, 2), dtype="float32")  # 1 second silent stereo
    for i in range(1, track_count + 1):
        path = album_dir / f"{i:02d} - Track {i}.flac"
        sf.write(str(path), samples, sr, format="FLAC")
    return album_dir


def _read_sse(stream_resp) -> list[tuple[str, dict]]:
    """Decode an httpx StreamingResponse into a list of (event, data) tuples."""
    events: list[tuple[str, dict]] = []
    pending_event = None
    for line in stream_resp.iter_lines():
        # httpx already strips the trailing \n. iter_lines yields strings.
        if line.startswith("event: "):
            pending_event = line.removeprefix("event: ")
        elif line.startswith("data: "):
            data = json.loads(line.removeprefix("data: "))
            assert pending_event is not None, "data: without event:"
            events.append((pending_event, data))
            pending_event = None
    return events


def _scan_album(client, library: Path) -> dict:
    """Configure library + populate the cache directly so the analyze
    endpoint resolves the album by id. Bypasses the SSE scan stream
    (which runs in a background task and would race with the test)."""
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
    body = r.json()
    assert body["count"] >= 1
    return body["albums"][0]


def test_bpm_analysis_writes_tag(client, empty_library, monkeypatch):
    _make_album(empty_library, dir_name="Test Artist - Test Album [FLAC]", track_count=2)
    album = _scan_album(client, empty_library)
    album_id = album["id"]

    # Replace the librosa-dependent inner so tests don't pay the
    # several-second cost-per-track of real beat tracking.
    monkeypatch.setattr(
        audio_analysis,
        "_detect_bpm",
        lambda _librosa, _path: 123.45,
    )

    with client.stream(
        "POST",
        f"/api/albums/{album_id}/analyze/stream",
        json={"what": "bpm"},
    ) as resp:
        assert resp.status_code == 200
        events = _read_sse(resp)

    names = [n for n, _ in events]
    assert "start" in names
    assert names.count("file_started") == 2
    assert names.count("file_done") == 2
    assert "done" in names

    # Final summary
    done_payload = next(d for n, d in events if n == "done")
    assert done_payload == {"what": "bpm", "analyzed_count": 2, "errors": 0}

    # Tags actually landed on disk
    album_dir = Path(album["path"])
    for path in sorted(album_dir.glob("*.flac")):
        audio = FLAC(str(path))
        assert audio.tags is not None
        assert audio.tags["bpm"] == ["123.45"]


def test_replaygain_analysis_writes_track_and_album_tags(client, empty_library, monkeypatch):
    _make_album(empty_library, dir_name="Test Artist - RG Album [FLAC]", track_count=3)
    album = _scan_album(client, empty_library)
    album_id = album["id"]

    # All three tracks return the same loudness so the energy-weighted
    # album LUFS equals the per-track LUFS — makes the assertion below
    # trivial without re-deriving the formula.
    monkeypatch.setattr(
        audio_analysis,
        "_measure_track_loudness",
        lambda _np, _pyln, _sf, _path: (-22.0, 0.5),
    )

    with client.stream(
        "POST",
        f"/api/albums/{album_id}/analyze/stream",
        json={"what": "replaygain"},
    ) as resp:
        assert resp.status_code == 200
        events = _read_sse(resp)

    names = [n for n, _ in events]
    assert names.count("file_done") == 3
    assert "album_done" in names

    album_done = next(d for n, d in events if n == "album_done")
    # Reference is -18 LUFS, measured -22 LUFS → +4 dB album gain.
    assert album_done["replaygain_album_gain"] == "+4.00 dB"
    assert album_done["replaygain_album_peak"] == "0.500000"

    # Tags landed on every track
    for path in sorted(Path(album["path"]).glob("*.flac")):
        audio = FLAC(str(path))
        assert audio.tags["replaygain_track_gain"] == ["+4.00 dB"]
        assert audio.tags["replaygain_track_peak"] == ["0.500000"]
        assert audio.tags["replaygain_album_gain"] == ["+4.00 dB"]
        assert audio.tags["replaygain_album_peak"] == ["0.500000"]
        assert audio.tags["replaygain_reference_loudness"] == ["-18.00 LUFS"]


def test_invalid_what_returns_400(client, empty_library):
    _make_album(empty_library, dir_name="Test Artist - 400 Album [FLAC]", track_count=1)
    album = _scan_album(client, empty_library)

    r = client.post(
        f"/api/albums/{album['id']}/analyze/stream",
        json={"what": "loudness"},  # not allowed
    )
    # Pydantic Literal validation rejects unknown values with 422.
    assert r.status_code in (400, 422)


def test_error_event_when_librosa_missing(client, empty_library, monkeypatch):
    _make_album(empty_library, dir_name="Test Artist - No Lib Album [FLAC]", track_count=1)
    album = _scan_album(client, empty_library)

    monkeypatch.setattr(audio_analysis, "_try_import_librosa", lambda: None)

    with client.stream(
        "POST",
        f"/api/albums/{album['id']}/analyze/stream",
        json={"what": "bpm"},
    ) as resp:
        assert resp.status_code == 200
        events = _read_sse(resp)

    names = [n for n, _ in events]
    assert "error" in names
    err_payload = next(d for n, d in events if n == "error")
    assert "librosa" in err_payload["detail"].lower()


def test_aggregate_album_energy_weighted():
    """Energy-weighted mean: a -10 LUFS track for 60s + a -30 LUFS track
    for 60s should land closer to -10 LUFS than a simple arithmetic mean
    (-20 LUFS) — the louder one carries more energy weight."""
    result = audio_analysis._aggregate_album([
        (60.0, -10.0, 0.9),
        (60.0, -30.0, 0.5),
    ])
    assert result is not None
    album_lufs, album_peak = result
    # 10·log10((60·10^-1 + 60·10^-3) / 120) = 10·log10((6.06)/120) ≈ -13.0 LUFS
    assert album_lufs == pytest.approx(-13.0, abs=0.05)
    assert album_peak == 0.9


def test_aggregate_album_drops_non_finite():
    """Tracks with -inf LUFS (silence) must NOT poison the album mean —
    they're dropped from the weighted average but still considered for
    peak."""
    import math
    result = audio_analysis._aggregate_album([
        (60.0, -20.0, 0.8),
        (60.0, -math.inf, 0.0),
    ])
    assert result is not None
    album_lufs, album_peak = result
    assert album_lufs == pytest.approx(-20.0, abs=0.01)
    assert album_peak == 0.8
