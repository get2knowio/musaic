"""Tests for the AI-job failure-excerpt capture path.

The worker installs a transcript buffer before running an AI suggestion
call (`ai.install_transcript_buffer`); anything Claude streams gets
recorded via `_record_transcript`. On failure the worker reads the
buffer back and persists it under `result_json.failure_excerpt` so the
Jobs UI can show the user what Claude said before the call blew up.

These tests bypass the actual SDK via `ai.set_sdk_override` so we
exercise the capture pipeline without a real Claude subprocess.
"""
from __future__ import annotations

import asyncio
import json

import pytest

from musaic import ai, db as db_module, jobs as jobs_module
from musaic.services import worker


# ---------- Pure helpers ----------


def test_format_transcript_empty_buffer_returns_none():
    assert worker._format_transcript([], max_bytes=1000) is None
    assert worker._format_transcript(["", "  "], max_bytes=1000) is None


def test_format_transcript_joins_pieces():
    out = worker._format_transcript(["first", "second", "third"], max_bytes=1000)
    assert out is not None
    assert "first" in out and "second" in out and "third" in out
    # Per-entry newline separator. The structured `_record_message`
    # capture in ai.py emits one short line per block (text, tool
    # call, tool result), so the transcript reads like a chrono
    # log — no need for the `---` block separator the old text-only
    # capture used.
    assert out == "first\nsecond\nthird"


def test_format_transcript_truncates_to_max_bytes():
    big = "x" * 10_000
    out = worker._format_transcript([big], max_bytes=1_000)
    assert out is not None
    assert len(out.encode("utf-8")) <= 1_100  # truncated + small prefix
    assert "truncated" in out


def test_record_transcript_appends_to_active_buffer():
    buf = ai.install_transcript_buffer()
    ai._record_transcript("hello")
    ai._record_transcript("world")
    ai._record_transcript("")  # empty pieces dropped
    assert buf == ["hello", "world"]


# ============================================================
# _record_message — structured per-block transcript capture
# ============================================================
#
# The load-bearing diagnostic for failed / timed-out jobs: when a
# job hangs on a stuck WebFetch, the transcript shown in the Jobs
# UI must include WHICH tool call / URL was last in flight.
# Without this, the failure expand surfaces "subprocess exited"
# with zero breadcrumbs.


def test_record_message_captures_text_block():
    from claude_agent_sdk import AssistantMessage, TextBlock
    buf = ai.install_transcript_buffer()
    ai._record_message(AssistantMessage(
        content=[TextBlock(text="Searching MusicBrainz for the album")],
        model="claude-sonnet-4-6",
    ))
    assert buf == ["Searching MusicBrainz for the album"]


def test_record_message_captures_tool_use_with_url():
    """WebFetch is the most diagnostically-valuable tool call to
    record — when a job hangs, the last → WebFetch entry tells us
    which upstream is wedged."""
    from claude_agent_sdk import AssistantMessage, ToolUseBlock
    buf = ai.install_transcript_buffer()
    ai._record_message(AssistantMessage(
        content=[ToolUseBlock(
            id="t1", name="WebFetch",
            input={"url": "https://musicbrainz.org/release-group/abc-123"},
        )],
        model="claude-sonnet-4-6",
    ))
    assert len(buf) == 1
    line = buf[0]
    assert line.startswith("→ WebFetch")
    assert "musicbrainz.org/release-group/abc-123" in line


def test_record_message_captures_tool_use_with_query():
    from claude_agent_sdk import AssistantMessage, ToolUseBlock
    buf = ai.install_transcript_buffer()
    ai._record_message(AssistantMessage(
        content=[ToolUseBlock(
            id="t1", name="WebSearch",
            input={"query": "101 Strings Country Greats"},
        )],
        model="claude-sonnet-4-6",
    ))
    assert "→ WebSearch" in buf[0]
    assert "101 Strings Country Greats" in buf[0]


def test_record_message_captures_tool_result_string():
    from claude_agent_sdk import ToolResultBlock, UserMessage
    buf = ai.install_transcript_buffer()
    ai._record_message(UserMessage(
        content=[ToolResultBlock(
            tool_use_id="t1",
            content="Found 4 candidate release-groups",
            is_error=False,
        )],
    ))
    assert buf == ["←  Found 4 candidate release-groups"]


def test_record_message_marks_tool_result_errors_distinctly():
    """Errors get '← ERROR' so they jump out visually when scanning
    a long transcript looking for what went wrong."""
    from claude_agent_sdk import ToolResultBlock, UserMessage
    buf = ai.install_transcript_buffer()
    ai._record_message(UserMessage(
        content=[ToolResultBlock(
            tool_use_id="t1",
            content="HTTP 503 Service Unavailable",
            is_error=True,
        )],
    ))
    assert buf[0].startswith("← ERROR")
    assert "503" in buf[0]


def test_record_message_truncates_long_tool_results():
    """Tool results can be entire HTML pages. The per-entry cap
    keeps the transcript scannable and the overall buffer bounded."""
    from claude_agent_sdk import ToolResultBlock, UserMessage
    buf = ai.install_transcript_buffer()
    huge = "x" * 5000
    ai._record_message(UserMessage(
        content=[ToolResultBlock(
            tool_use_id="t1",
            content=huge,
            is_error=False,
        )],
    ))
    assert len(buf) == 1
    # Per-block cap is 600 chars (with ellipsis marker around it).
    assert len(buf[0]) < 700
    assert "[" in buf[0] and "ch]" in buf[0]  # truncation marker


def test_record_message_ignores_unknown_message_types():
    """Forward compat: SDK adds new message types over time. We
    shouldn't raise on ones we don't recognize."""
    buf = ai.install_transcript_buffer()
    # Pass something that isn't a known message type.
    ai._record_message(object())  # asserts: doesn't raise
    assert buf == []


def test_full_transcript_pipeline_for_a_failed_call():
    """End-to-end shape: a sequence of messages that looks like a
    real (failed) identification attempt. Confirms the transcript
    that lands in `failure_excerpt` would actually be diagnostic."""
    from claude_agent_sdk import (
        AssistantMessage, TextBlock, ToolResultBlock, ToolUseBlock, UserMessage,
    )
    buf = ai.install_transcript_buffer()

    # 1. Model thinks out loud
    ai._record_message(AssistantMessage(
        content=[TextBlock(text="Let me search MusicBrainz")],
        model="claude-sonnet-4-6",
    ))
    # 2. Model calls WebFetch
    ai._record_message(AssistantMessage(
        content=[ToolUseBlock(
            id="t1", name="WebFetch",
            input={"url": "https://musicbrainz.org/release-group/abc"},
        )],
        model="claude-sonnet-4-6",
    ))
    # 3. Tool result comes back as an error
    ai._record_message(UserMessage(
        content=[ToolResultBlock(
            tool_use_id="t1",
            content="HTTP 503 Service Unavailable",
            is_error=True,
        )],
    ))

    out = worker._format_transcript(buf, max_bytes=10_000)
    assert out is not None
    # Must contain all three steps in chronological order.
    assert "Let me search MusicBrainz" in out
    assert "→ WebFetch" in out
    assert "musicbrainz.org/release-group/abc" in out
    assert "← ERROR" in out
    assert "503" in out
    # And in the right order.
    assert out.index("Let me search") < out.index("→ WebFetch") < out.index("← ERROR")


def test_record_transcript_no_op_when_no_buffer():
    # Reset to None so any prior test's buffer is gone.
    ai._transcript_buffer.set(None)
    # Should not raise.
    ai._record_transcript("ignored")
    assert ai._transcript_buffer.get() is None


# ---------- Worker integration ----------


@pytest.fixture(autouse=True)
def _clean_jobs_table():
    with db_module.connect() as conn:
        with db_module.transaction(conn):
            conn.execute("DELETE FROM ai_jobs")
            conn.execute("DELETE FROM albums")
            conn.execute("DELETE FROM libraries")
    yield


def _stub_album_lookup(monkeypatch, album_id: str) -> dict:
    """Replace `cached_album_dict` so the worker resolves our fake album
    without needing a real library snapshot. Also stubs `read_tracks`
    since we don't have FLACs on disk to read."""
    album = {
        "id": album_id,
        "path": f"/fake/library/{album_id}",
        "dir_name": "Test Artist - Test Album [FLAC]",
        "title": "Test Album",
        "artist": "Test Artist",
        "album_artist": "Test Artist",
        "year": "1979",
    }
    from musaic.services import albums as albums_service
    monkeypatch.setattr(
        albums_service, "cached_album_dict",
        lambda _id: album if _id == album_id else None,
    )
    # The worker imports cached_album_dict at module load time, so patch
    # the worker's bound reference too.
    monkeypatch.setattr(worker, "cached_album_dict", lambda _id: album if _id == album_id else None)
    monkeypatch.setattr(worker, "read_tracks", lambda _path: [])
    return album


def test_worker_persists_failure_excerpt_on_ai_failure(monkeypatch):
    """When suggest_metadata raises, the worker reads the transcript
    buffer back and stores it under result_json.failure_excerpt."""
    _stub_album_lookup(monkeypatch, "alb-fail-meta")
    job = jobs_module.enqueue(
        album_id="alb-fail-meta",
        album_path="/fake/library/alb-fail-meta",
        album_title="Test Album",
        job_type="metadata",
    )

    # Stub the resolver to return "no anchor" so the worker takes the
    # AI path (rather than synthesizing).
    from musaic.services import resolver as resolver_service
    from musaic.services.resolver import AlbumResolution

    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda *_a, **_kw: AlbumResolution(album_id="alb-fail-meta"),
    )

    # Replace suggest_metadata with a stub that records two transcript
    # pieces (mimicking Claude streaming) and then raises. Real call
    # path uses _record_transcript inside _query_async; we mirror that
    # contract here so the worker's buffer captures the same shape.
    async def fake_suggest_metadata(*_a, **_kw):
        ai._record_transcript("Looking up the album on MusicBrainz...")
        ai._record_transcript("Based on my search I think this is the 1979 release.")
        raise RuntimeError("Claude Code subprocess exited with code 1")

    monkeypatch.setattr(ai, "suggest_metadata", fake_suggest_metadata)

    # Don't touch the real settings file when we auto-pause on failure.
    from musaic import settings as settings_module
    monkeypatch.setattr(settings_module, "set_ai_queue_paused", lambda _v: None)

    asyncio.run(worker.run_one_job(job))

    # Pull the persisted job back out and inspect the result_json.
    saved = jobs_module.get_job(job.id)
    assert saved is not None
    assert saved.status == "failed"
    assert saved.error and "code 1" in saved.error
    assert saved.result_json is not None
    parsed = json.loads(saved.result_json)
    excerpt = parsed.get("failure_excerpt")
    assert excerpt is not None
    assert "MusicBrainz" in excerpt
    assert "1979 release" in excerpt


def test_worker_no_excerpt_when_buffer_empty(monkeypatch):
    """If Claude failed before any messages streamed (e.g. exit-1 from
    the very first turn), failure_excerpt is omitted entirely so the
    UI doesn't show an empty disclosure."""
    _stub_album_lookup(monkeypatch, "alb-fail-cold")
    # Reset transcript buffer in case a prior test left state behind.
    ai._transcript_buffer.set(None)
    job = jobs_module.enqueue(
        album_id="alb-fail-cold",
        album_path="/fake/library/alb-fail-cold",
        album_title="Cold Failure",
        job_type="metadata",
    )

    from musaic.services import resolver as resolver_service
    from musaic.services.resolver import AlbumResolution

    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda *_a, **_kw: AlbumResolution(album_id="alb-fail-cold"),
    )

    async def fake_suggest_metadata(*_a, **_kw):
        # No _record_transcript calls — simulating a hard exit before
        # any assistant message was emitted.
        raise RuntimeError("Claude Code subprocess exited with code 1")

    monkeypatch.setattr(ai, "suggest_metadata", fake_suggest_metadata)

    from musaic import settings as settings_module
    monkeypatch.setattr(settings_module, "set_ai_queue_paused", lambda _v: None)

    asyncio.run(worker.run_one_job(job))

    saved = jobs_module.get_job(job.id)
    assert saved is not None
    assert saved.status == "failed"
    # result_json is None (or has no failure_excerpt key) — either
    # representation tells the frontend "no transcript to show."
    if saved.result_json is None:
        return
    parsed = json.loads(saved.result_json)
    assert parsed.get("failure_excerpt") is None or parsed.get("failure_excerpt") == ""


def test_worker_timeout_writes_fallback_excerpt_when_buffer_empty(monkeypatch):
    """When the wall-clock cap fires AND the buffer is empty (SDK
    streamed nothing — subprocess wedged before any output), the
    worker writes a fallback "(no SDK messages received)" excerpt
    so the chevron in the Jobs UI always appears on timeouts. The
    user can then see "nothing back" IS the diagnostic, rather
    than wondering if the UI is broken.

    Distinct from `test_worker_no_excerpt_when_buffer_empty`: that
    covers a Claude SDK exception path (subprocess exited with an
    error); this covers the wall-clock TimeoutError path
    specifically, where empty buffer means "subprocess didn't
    even produce an error — it just sat there." Different
    failure modes, different user-facing messages."""
    _stub_album_lookup(monkeypatch, "alb-timeout-silent")
    ai._transcript_buffer.set(None)
    job = jobs_module.enqueue(
        album_id="alb-timeout-silent",
        album_path="/fake/library/alb-timeout-silent",
        album_title="Silent Timeout",
        job_type="metadata",
    )

    from musaic.services import resolver as resolver_service
    from musaic.services.resolver import AlbumResolution

    monkeypatch.setattr(
        resolver_service, "resolve_album",
        lambda *_a, **_kw: AlbumResolution(album_id="alb-timeout-silent"),
    )

    # Stub suggest_metadata to raise TimeoutError directly (simulates
    # the wall-clock cap firing on an SDK call that never streamed).
    async def fake_suggest_metadata(*_a, **_kw):
        raise asyncio.TimeoutError()

    monkeypatch.setattr(ai, "suggest_metadata", fake_suggest_metadata)

    from musaic import settings as settings_module
    monkeypatch.setattr(settings_module, "set_ai_queue_paused", lambda _v: None)

    asyncio.run(worker.run_one_job(job))

    saved = jobs_module.get_job(job.id)
    assert saved is not None
    assert saved.status == "failed"
    # Top-level error message reflects the timeout.
    assert "wall-clock cap" in (saved.error or "")
    # And failure_excerpt is populated with the fallback even though
    # the SDK streamed nothing — the chevron must appear.
    assert saved.result_json is not None, \
        "timeout failure must persist a failure_excerpt so the chevron renders"
    parsed = json.loads(saved.result_json)
    excerpt = parsed.get("failure_excerpt")
    assert excerpt, \
        "empty transcript on timeout must fall back to a diagnostic placeholder"
    assert "no SDK messages" in excerpt
