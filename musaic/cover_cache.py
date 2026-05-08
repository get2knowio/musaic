"""Tiny in-memory cache for cover-art bytes keyed by URL.

Why this exists: when the AI proposes a cover URL we already make an HTTP
request to verify it resolves to an image. If we read the response body
once during verification and stash it here, the apply path can reuse the
bytes — no second download, no extra latency, no risk of the URL turning
404 between suggestion and apply.

Constraints:

  - Bounded size and TTL — covers can be 100KB-2MB each, so we cap entries
    and total bytes to keep memory in check (this is a personal-scale app,
    not a service).
  - In-memory only. A restart wipes it; the apply path falls back to a
    fresh download in that case. No persistence layer needed.
  - Thread-safe enough — we hold a single lock around the dict mutation.
    Read paths grab a snapshot then release.

If we ever start serving cover previews to the browser through our backend
(rather than letting the browser hit the third-party URL directly), we'll
hang the proxy endpoint off this same cache.
"""
from __future__ import annotations

import threading
import time

# (url) → (bytes, mime, inserted_at_seconds)
_cache: dict[str, tuple[bytes, str, float]] = {}
_lock = threading.Lock()

# Tunables. Cover images are typically 100KB-2MB; 32 entries × ~1MB ≈ 32MB
# upper bound, comfortable on modern machines and far below the SDK's own
# memory footprint.
MAX_ENTRIES = 32
MAX_TOTAL_BYTES = 64 * 1024 * 1024  # 64 MB hard ceiling
MAX_BYTES_PER_ENTRY = 8 * 1024 * 1024  # 8 MB per cover (Cover Art Archive originals run big)
TTL_SECONDS = 30 * 60  # 30 minutes — well beyond a typical "see suggestion → apply" gap


def get(url: str) -> tuple[bytes, str] | None:
    """Return cached (bytes, mime) for `url`, or None if missing/expired.

    Expired entries are evicted as a side effect — we don't bother with a
    background sweeper for a cache this small."""
    with _lock:
        entry = _cache.get(url)
        if entry is None:
            return None
        data, mime, inserted_at = entry
        if time.time() - inserted_at > TTL_SECONDS:
            _cache.pop(url, None)
            return None
        return data, mime


def put(url: str, data: bytes, mime: str) -> None:
    """Store `data` under `url`. Silently no-ops on outsized payloads.

    Eviction policy: when at capacity, drop the oldest insertion (FIFO).
    Good enough for a workflow where each URL is seen ~once."""
    if len(data) > MAX_BYTES_PER_ENTRY:
        return
    with _lock:
        # If overwriting an existing entry, just refresh.
        if url in _cache:
            _cache[url] = (data, mime, time.time())
            return
        # Evict by insertion order until we have headroom on both axes.
        while len(_cache) >= MAX_ENTRIES or _total_bytes_locked() + len(data) > MAX_TOTAL_BYTES:
            try:
                oldest = next(iter(_cache))
            except StopIteration:
                break
            _cache.pop(oldest, None)
        _cache[url] = (data, mime, time.time())


def invalidate(url: str) -> None:
    """Drop the cache entry for `url` (e.g. after the cover has been written
    to all FLAC files and isn't expected to be re-fetched)."""
    with _lock:
        _cache.pop(url, None)


def stats() -> dict[str, int]:
    """Diagnostic snapshot — useful for /healthz or a future debug page."""
    with _lock:
        return {
            "entries": len(_cache),
            "total_bytes": _total_bytes_locked(),
        }


def _total_bytes_locked() -> int:
    return sum(len(data) for data, _, _ in _cache.values())
