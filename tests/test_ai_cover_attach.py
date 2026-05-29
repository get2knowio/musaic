"""Tests for ai._album_cover_for_prompt + suggest_metadata's
cover-attach plumbing.

The bet: attaching the album's cover image to the suggest_metadata
prompt boosts identification quality for cases where text-only
signals are weak (compilations, soundtracks, foreign-language,
box sets) by giving Claude the canonical artist/title rendered
visually. These tests pin the plumbing:

  - `_album_cover_for_prompt` returns None for albums with no
    cover (no thumb, no sidecar, no embedded picture)
  - `_album_cover_for_prompt` returns base64+mime when a thumb
    exists at the canonical path
  - `suggest_metadata` wraps the prompt in a content-block list
    (image + text) when a cover is available
  - `suggest_metadata` falls back to a bare-string prompt when no
    cover exists, preserving the pre-feature behavior

We don't exercise the actual SDK round-trip — that requires
network + credentials. Instead we monkeypatch `_run_sdk` to
capture its `prompt` argument and assert on the shape.
"""
from __future__ import annotations

import asyncio
import base64
import io
from pathlib import Path

import pytest

from musaic import ai
from musaic import thumbs as thumbs_module


def _make_tiny_jpeg() -> bytes:
    """Build a 16x16 black JPEG so we can write a real thumb to disk.
    Pillow is already a project dep (used by the thumbs module)."""
    from PIL import Image
    img = Image.new("RGB", (16, 16), (0, 0, 0))
    buf = io.BytesIO()
    img.save(buf, format="JPEG", quality=70)
    return buf.getvalue()


@pytest.fixture
def write_thumb_for():
    """Helper to drop a JPEG at the canonical thumb path for an album_id.
    Cleans up after the test."""
    written: list[Path] = []

    def _write(album_id: str) -> Path:
        thumbs_module.THUMBS_DIR.mkdir(parents=True, exist_ok=True)
        p = thumbs_module.thumb_path(album_id)
        p.write_bytes(_make_tiny_jpeg())
        written.append(p)
        return p

    yield _write
    for p in written:
        p.unlink(missing_ok=True)


# ============================================================
# _album_cover_for_prompt
# ============================================================


def test_album_cover_for_prompt_returns_none_when_no_cover():
    """No thumb on disk, no cover_file, no embedded picture → None.
    Callers should fall back to text-only prompts."""
    result = ai._album_cover_for_prompt({
        "id": "no-cover-album-id-12345",
        "path": "/nonexistent/path",
        "has_embedded_cover": False,
        "cover_file": None,
        "sample_track": "",
    })
    assert result is None


def test_album_cover_for_prompt_returns_none_without_album_id():
    """Defensive: malformed input shouldn't raise. The helper needs
    an id to know where to look for the thumb."""
    assert ai._album_cover_for_prompt({}) is None
    assert ai._album_cover_for_prompt({"id": ""}) is None


def test_album_cover_for_prompt_reads_pre_extracted_thumb(write_thumb_for):
    """When a pre-extracted thumb exists at the canonical path, the
    helper returns its base64-encoded bytes + JPEG mime. This is the
    common path — scan service warms thumbs for every album with
    cover art."""
    album_id = "test-album-with-thumb-001"
    thumb_path = write_thumb_for(album_id)

    result = ai._album_cover_for_prompt({"id": album_id})
    assert result is not None
    b64, mime = result
    assert mime == "image/jpeg"
    # Round-trip: decoded base64 == thumb bytes on disk
    assert base64.standard_b64decode(b64) == thumb_path.read_bytes()


# ============================================================
# suggest_metadata — structured prompt build
# ============================================================


def _capture_prompt() -> tuple[list, callable]:
    """Build a list + async monkeypatch fn. The fn replaces _run_sdk
    so we can intercept the SDK prompt without making a real call."""
    captured: list = []

    async def fake_run_sdk(prompt, **_kwargs):
        captured.append(prompt)
        # Return a minimal valid suggestion JSON so _extract_json
        # succeeds and suggest_metadata returns a MetadataSuggestion
        # rather than blowing up.
        return '{"suggestions": {}, "tracks": [], "notes": "", "confidence": "low"}'

    return captured, fake_run_sdk


def test_suggest_metadata_attaches_cover_when_available(
    monkeypatch, write_thumb_for,
):
    """When the album has a cover (thumb present), suggest_metadata
    passes a list-of-content-blocks prompt to the SDK: image block
    first, then the text user message. This is the structured form
    the Anthropic API expects for multimodal calls."""
    album_id = "test-album-cover-attach-001"
    write_thumb_for(album_id)

    captured, fake = _capture_prompt()
    monkeypatch.setattr(ai, "_run_sdk", fake)
    # Disable the override path so the SDK path runs.
    monkeypatch.setattr(ai, "_sdk_override", None)

    asyncio.run(ai.suggest_metadata(
        album_dict={
            "id": album_id,
            "dir_name": "Test - Album [FLAC]",
            "path": "/nonexistent",
            "title": "Test",
            "artist": "Test Artist",
            "album_artist": "Test Artist",
            "year": "2020",
            "genre": "Test",
            "track_count": 0,
            "total_seconds": 0,
            "has_embedded_cover": False,
            "cover_file": None,
            "sample_track": "",
        },
        tracks=[],
    ))

    assert len(captured) == 1
    prompt = captured[0]
    assert isinstance(prompt, list), \
        "cover-attached prompt must be a content-block list, not a bare string"
    assert len(prompt) == 2, "expected exactly 2 blocks: image + text"

    img_block, text_block = prompt
    assert img_block["type"] == "image"
    assert img_block["source"]["type"] == "base64"
    assert img_block["source"]["media_type"] == "image/jpeg"
    assert img_block["source"]["data"]  # non-empty base64
    assert text_block["type"] == "text"
    assert "<album>" in text_block["text"], \
        "text block should carry the existing XML-tagged payload"


def test_suggest_metadata_uses_bare_string_when_no_cover(monkeypatch):
    """No cover → bare-string prompt. Preserves the pre-feature
    behavior for coverless albums so we don't accidentally regress
    the text-only path."""
    captured, fake = _capture_prompt()
    monkeypatch.setattr(ai, "_run_sdk", fake)
    monkeypatch.setattr(ai, "_sdk_override", None)

    asyncio.run(ai.suggest_metadata(
        album_dict={
            "id": "test-album-no-cover-001",
            "dir_name": "Test - Album [FLAC]",
            "path": "/nonexistent",
            "title": "Test",
            "artist": "Test Artist",
            "album_artist": "Test Artist",
            "year": "2020",
            "genre": "Test",
            "track_count": 0,
            "total_seconds": 0,
            "has_embedded_cover": False,
            "cover_file": None,
            "sample_track": "",
        },
        tracks=[],
    ))

    assert len(captured) == 1
    prompt = captured[0]
    assert isinstance(prompt, str), \
        "coverless prompt must stay a bare string (no behavior change for the no-cover path)"
    assert "<album>" in prompt
