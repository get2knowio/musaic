#!/usr/bin/env python3
"""Diagnostic for the AI integration.

Run from the project directory:

    uv run python scripts/diagnose_claude.py
    uv run python scripts/diagnose_claude.py <album_id>

Prints the resolved environment, locates the `claude` CLI, reports the
airframe provider/model musaic dispatches through (and which hosted web
tools it serves), then makes a minimal SDK call against Haiku. Surfaces
the real exception (with traceback) so we can see what the underlying SDK
is actually choking on before it gets wrapped by our /api/ai endpoint.
"""
from __future__ import annotations

import asyncio
import os
import shutil
import subprocess
import sys
import traceback


def main() -> None:
    # Load .env the same way the FastAPI app does, so the diagnostic
    # matches the server's environment.
    try:
        import musaic  # noqa: F401  (triggers bootstrap_env)
    except Exception as e:
        print(f"WARN: couldn't import musaic package: {e}")

    print("=" * 60)
    print("Environment")
    print("=" * 60)
    for var in ("CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"):
        v = os.environ.get(var)
        if v:
            print(f"  {var:<28} set ({len(v)} chars, ends '…{v[-4:]}')")
        else:
            print(f"  {var:<28} NOT SET")

    print("\n" + "=" * 60)
    print("`claude` CLI")
    print("=" * 60)
    claude_path = shutil.which("claude")
    if claude_path:
        print(f"  found at: {claude_path}")
        try:
            v = subprocess.run(
                [claude_path, "--version"],
                capture_output=True, text=True, timeout=10,
            )
            print(f"  --version stdout: {v.stdout.strip()}")
            if v.stderr.strip():
                print(f"  --version stderr: {v.stderr.strip()}")
            print(f"  --version exit:   {v.returncode}")
        except Exception as e:
            print(f"  could not run --version: {e}")
    else:
        print("  NOT FOUND in PATH")
        print(f"  PATH: {os.environ.get('PATH', '(empty)')}")

    print("\n" + "=" * 60)
    print("SDK import")
    print("=" * 60)
    try:
        from claude_agent_sdk import query, ClaudeAgentOptions, AssistantMessage, ResultMessage  # noqa
        print("  claude_agent_sdk imports OK")
    except ImportError as e:
        print(f"  IMPORT FAILED: {e}")
        sys.exit(1)

    print("\n" + "=" * 60)
    print("airframe (agent runtime musaic dispatches through)")
    print("=" * 60)
    try:
        from airframe import Feature, list_providers, runtime_for

        from musaic import settings as settings_module

        provider_id = settings_module.ai_provider_id()
        model_id = settings_module.ai_model_id()
        print(f"  configured provider/model: {provider_id} / {model_id}")
        installed = list_providers(installed_only=True)
        print(f"  installed adapters: {installed}")
        if provider_id not in installed:
            print(f"  WARNING: provider {provider_id!r} is not installed — "
                  f"install its airframe extra (e.g. airframe-agents[{provider_id}]).")
        else:
            runtime = runtime_for(provider_id)()
            served = sorted(runtime.supported_native_tools())
            print(f"  supports(TOOLS_NATIVE): {runtime.supports(Feature.TOOLS_NATIVE)}")
            print(f"  hosted web tools served: {served or '(none — web research degrades to resolver)'}")
    except Exception as e:
        print(f"  airframe probe FAILED: {e}")
        traceback.print_exc()

    # Allow `python scripts/diagnose_claude.py <album_id>` to test the exact
    # failing album from the user's real library, instead of the synthetic
    # Wall payload.
    real_album_id = sys.argv[1] if len(sys.argv) > 1 else None

    asyncio.run(_run_diagnostics())
    asyncio.run(_call_real_endpoint(real_album_id))
    asyncio.run(_call_real_cover_endpoint(real_album_id))


async def _run_diagnostics() -> None:
    # Three calls of increasing complexity. The first failing one tells us
    # which axis is the problem (model string, tool permissions, or
    # prompt-shape).
    await _call(
        "1. Haiku, no tools, single turn",
        model="claude-haiku-4-5",
        allowed_tools=[],
        max_turns=1,
    )
    await _call(
        "2. Sonnet, no tools, single turn",
        model="claude-sonnet-4-6",
        allowed_tools=[],
        max_turns=1,
    )
    await _call(
        "3. Sonnet + WebSearch/WebFetch, 5 turns (the shape our app uses)",
        model="claude-sonnet-4-6",
        allowed_tools=["WebSearch", "WebFetch"],
        max_turns=5,
    )
    # We learned the hard way (May 2026) that Haiku + WebSearch/WebFetch in a
    # multi-turn agent loop crashes the CLI subprocess with exit code 1.
    # Keep this test so we notice if the SDK / Claude Code ever fixes it.
    await _call(
        "3b. Haiku + WebSearch/WebFetch (KNOWN-BROKEN combo as of 2026-05)",
        model="claude-haiku-4-5",
        allowed_tools=["WebSearch", "WebFetch"],
        max_turns=5,
    )


async def _call(label: str, *, model: str, allowed_tools: list[str], max_turns: int) -> None:
    from claude_agent_sdk import (
        AssistantMessage, ClaudeAgentOptions, ResultMessage, query,
    )

    print("\n" + "=" * 60)
    print(label)
    print("=" * 60)
    last_text = ""
    final = ""
    try:
        async for message in query(
            prompt="Reply with the single word: OK",
            options=ClaudeAgentOptions(
                allowed_tools=allowed_tools,
                model=model,
                max_turns=max_turns,
                system_prompt="You are a test bot. Reply only with 'OK'.",
            ),
        ):
            print(f"  [event] {type(message).__name__}")
            if isinstance(message, AssistantMessage):
                for block in message.content:
                    if hasattr(block, "text") and block.text:
                        last_text = block.text
            if isinstance(message, ResultMessage):
                if message.result:
                    final = message.result
        print(f"\n  ✓ Result: {final or last_text!r}")
    except Exception as e:
        print(f"\n  ✗ FAILED: {type(e).__name__}: {e}")
        if hasattr(e, "__cause__") and e.__cause__:
            print(f"  Caused by: {type(e.__cause__).__name__}: {e.__cause__}")
        print("  Traceback:")
        traceback.print_exc()


_SYNTHETIC_ALBUM = {
    "id": "test123",
    "path": "/tmp/Pink Floyd - The Wall [FLAC]",
    "dir_name": "Pink Floyd - The Wall [FLAC]",
    "title": "The Wall", "artist": "Pink Floyd", "album_artist": "Pink Floyd",
    "year": "1979", "genre": "Progressive Rock", "track_count": 2,
    "total_seconds": 600.0, "has_embedded_cover": True, "cover_file": None,
    "sample_track": "/tmp/01.flac", "dir_mtime": 0.0,
    "dir_artist": "Pink Floyd", "dir_album": "The Wall",
    "is_compilation": False, "disc_number": None, "disc_total": None,
    "sample_rate": 44100, "bits_per_sample": 16, "channels": 2,
}
_SYNTHETIC_TRACKS = [
    {"filename": "01 - In the Flesh.flac", "tracknumber": "1", "title": "In the Flesh?", "artist": "Pink Floyd", "duration": 199, "bpm": None, "replaygain_track_gain": None, "replaygain_track_peak": None, "replaygain_album_gain": None, "replaygain_album_peak": None},
    {"filename": "02 - The Thin Ice.flac",  "tracknumber": "2", "title": "The Thin Ice", "artist": "Pink Floyd", "duration": 147, "bpm": None, "replaygain_track_gain": None, "replaygain_track_peak": None, "replaygain_album_gain": None, "replaygain_album_peak": None},
]


def _load_album(album_id: str | None) -> tuple[dict, list[dict], str]:
    """Returns (album, tracks, label). If album_id is given, loads it from
    the user's library via the cache; otherwise returns synthetic data."""
    if not album_id:
        return _SYNTHETIC_ALBUM, _SYNTHETIC_TRACKS, "synthetic Pink Floyd / The Wall"
    try:
        from musaic import cache as cache_module
        from musaic import config as config_module
        from musaic.main import _read_tracks
    except ImportError as e:
        print(f"  ✗ couldn't import library helpers: {e}")
        return _SYNTHETIC_ALBUM, _SYNTHETIC_TRACKS, "synthetic (failed to load real album)"
    cfg = config_module.load_config()
    if not cfg.library_dir:
        print("  ✗ no library_dir configured; falling back to synthetic")
        return _SYNTHETIC_ALBUM, _SYNTHETIC_TRACKS, "synthetic (no library configured)"
    snap = cache_module.load_snapshot(cfg.library_dir)
    album = next((a for a in snap.albums if a["id"] == album_id), None)
    if album is None:
        print(f"  ✗ album {album_id!r} not in cache; falling back to synthetic")
        return _SYNTHETIC_ALBUM, _SYNTHETIC_TRACKS, f"synthetic (album {album_id} not found)"
    tracks = _read_tracks(album["path"])
    label = f"REAL album: {album.get('title') or album.get('dir_name')!r} ({len(tracks)} tracks)"
    return album, tracks, label


async def _call_real_endpoint(album_id: str | None) -> None:
    """Call our actual ai.suggest_metadata() with either a synthetic payload
    or the user's real failing album."""
    print("\n" + "=" * 60)
    print("4. Real ai.suggest_metadata()")
    print("=" * 60)
    try:
        from musaic import ai
    except ImportError as e:
        print(f"  ✗ couldn't import musaic.ai: {e}")
        return

    album, tracks, label = _load_album(album_id)
    print(f"  payload: {label}")
    try:
        result = await ai.suggest_metadata(album, tracks)
        print(f"  ✓ confidence: {result.confidence}")
        print(f"    suggestions: {result.suggestions}")
        print(f"    track changes: {len(result.tracks)}")
        print(f"    notes: {result.notes[:200]}")
    except Exception as e:
        print(f"  ✗ FAILED: {type(e).__name__}: {e}")
        if hasattr(e, "__cause__") and e.__cause__:
            print(f"  Caused by: {type(e.__cause__).__name__}: {e.__cause__}")
        print("  Traceback:")
        traceback.print_exc()


async def _call_real_cover_endpoint(album_id: str | None) -> None:
    """Call ai.suggest_cover() with the same combo used in the UI."""
    print("\n" + "=" * 60)
    print("5. Real ai.suggest_cover() (Haiku + tools — same as the failing UI call)")
    print("=" * 60)
    try:
        from musaic import ai
    except ImportError as e:
        print(f"  ✗ couldn't import musaic.ai: {e}")
        return

    album, tracks, label = _load_album(album_id)
    print(f"  payload: {label}")
    try:
        result = await ai.suggest_cover(album, tracks)
        print(f"  ✓ confidence: {result.confidence}")
        print(f"    cover_url: {result.cover_url}")
        print(f"    source: {result.source}")
        print(f"    notes: {result.notes[:200]}")
    except Exception as e:
        print(f"  ✗ FAILED: {type(e).__name__}: {e}")
        if hasattr(e, "__cause__") and e.__cause__:
            print(f"  Caused by: {type(e.__cause__).__name__}: {e.__cause__}")
        print("  Traceback:")
        traceback.print_exc()


if __name__ == "__main__":
    main()
