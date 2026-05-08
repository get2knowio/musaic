"""Per-track audio fingerprinting via Chromaprint + AcoustID.

Wraps the `pyacoustid` library, which itself shells out to the `fpcalc`
binary (Chromaprint's fingerprint generator). The two-tier dependency
means we have three failure modes to surface cleanly:

  - `fpcalc` not installed   → `FingerprintError("fpcalc-missing")`
  - AcoustID key not set      → `FingerprintError("no-api-key")`
  - AcoustID API / network    → `FingerprintError("api-error", detail=...)`

None of these should crash the caller; the `track-gaps` fix cascade
treats any failure here as "fall through to AI" and the missing-binary
case can be surfaced as its own health-check finding pointing the user
at `brew install chromaprint`.

Caching: every successful fingerprint is persisted via
`cache.upsert_track_fingerprint`. Subsequent calls for the same album
are served from cache unless the file is missing or `force=True`.

Rate limiting: AcoustID's free tier allows 3 requests per second per
application key. We batch all of an album's files into one HTTP call via
`acoustid.lookup`, so a 14-track album = 1 API call, not 14. That keeps
us well under any per-user limit.
"""
from __future__ import annotations

import logging
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import cache as cache_module
from . import settings as settings_module

log = logging.getLogger(__name__)


# Confidence floor for treating an AcoustID match as "good enough" to use
# without a human in the loop. AcoustID returns scores in [0.0, 1.0]; in
# practice exact-rip matches come back at 0.95+. We accept down to 0.85
# to handle minor encoding differences (FLAC re-encoded by a different
# encoder version, etc.).
HIGH_CONFIDENCE_SCORE = 0.85


# ---------- Errors ----------

class FingerprintError(Exception):
    """Raised when fingerprinting can't proceed. The `code` distinguishes
    actionable cases (missing binary, missing key) from transient ones
    (network, API). Callers use `code` to decide whether to surface a
    health-check finding vs just falling through to AI."""
    def __init__(self, code: str, detail: str = ""):
        super().__init__(detail or code)
        self.code = code
        self.detail = detail


# ---------- Public API ----------

@dataclass
class TrackFingerprint:
    """Per-file result of fingerprinting + AcoustID lookup. Mirrors the
    shape stored in the track_fingerprints table."""
    filename: str
    fingerprint: str
    duration_s: float
    acoustid_recording_mbid: str | None
    acoustid_score: float | None


def is_available() -> tuple[bool, str]:
    """Quick precondition check used by the UI / health checks. Returns
    (ok, reason). No network calls — just looks for the binary and key."""
    if shutil.which("fpcalc") is None:
        return False, "fpcalc binary not found (install with `brew install chromaprint`)"
    if not settings_module.acoustid_api_key():
        return False, "AcoustID API key not set (configure in Settings)"
    return True, "ok"


def fingerprint_album(
    album_id: str,
    album_path: str,
    *,
    force: bool = False,
    timeout_per_file: int = 30,
) -> list[TrackFingerprint]:
    """Compute fingerprints for every FLAC in an album dir (or read from
    cache when fresh) and submit them to AcoustID in one batched call.

    Persists results to the track_fingerprints table on success.

    Raises FingerprintError on missing binary / key / network errors.
    Per-file Chromaprint failures are caught and logged but don't abort
    the rest of the album.
    """
    available, reason = is_available()
    if not available:
        if "fpcalc" in reason:
            raise FingerprintError("fpcalc-missing", reason)
        raise FingerprintError("no-api-key", reason)

    album_dir = Path(album_path)
    if not album_dir.is_dir():
        raise FingerprintError("album-missing", f"Album directory not found: {album_path}")

    # Cache hit: return what we have unless the caller forced a refresh.
    if not force:
        cached = cache_module.get_track_fingerprints(album_id)
        # Make sure the cached set still matches what's on disk — files
        # could have been added/removed since the last fingerprinting run.
        on_disk = sorted(_album_flacs(album_dir))
        if cached and {f["filename"] for f in cached} == {p.name for p in on_disk}:
            log.debug("fingerprint cache hit for album %s (%d tracks)",
                      album_id, len(cached))
            return [_dict_to_track(d) for d in cached]

    # Compute fingerprints with Chromaprint (one fpcalc invocation per
    # file — pyacoustid's `fingerprint_file` does this for us). Skip files
    # that error out individually; an unreadable FLAC shouldn't blow up
    # the whole album.
    flacs = sorted(_album_flacs(album_dir))
    if not flacs:
        return []

    # pyacoustid is an optional dep — guard the import so a missing dep
    # surfaces as our typed error rather than ImportError.
    try:
        import acoustid  # type: ignore[import-untyped]
    except ImportError as e:
        raise FingerprintError(
            "pyacoustid-missing",
            f"pyacoustid library not installed: {e}",
        ) from e

    per_file: list[tuple[Path, float, str]] = []  # (path, duration, fingerprint)
    for flac_path in flacs:
        try:
            duration, fingerprint = acoustid.fingerprint_file(
                str(flac_path),
                maxlength=120,  # 2 min is plenty for matching
            )
            # pyacoustid returns the fingerprint as bytes in some versions
            if isinstance(fingerprint, bytes):
                fingerprint = fingerprint.decode("ascii")
            per_file.append((flac_path, float(duration), fingerprint))
        except (acoustid.FingerprintGenerationError, subprocess.TimeoutExpired,
                FileNotFoundError, OSError) as e:
            log.warning("Chromaprint failed for %s: %s", flac_path.name, e)
            continue

    if not per_file:
        raise FingerprintError(
            "no-fingerprints",
            "Couldn't fingerprint any files in this album",
        )

    # Submit all fingerprints to AcoustID in one batched lookup. Returns
    # per-fingerprint match results.
    api_key = settings_module.acoustid_api_key()
    out: list[TrackFingerprint] = []
    for flac_path, duration, fp in per_file:
        try:
            results = acoustid.lookup(
                api_key, fp, duration,
                meta="recordings",
            )
        except acoustid.WebServiceError as e:
            # Don't let one file's API miss kill the whole album. We still
            # cache the fingerprint itself so the next attempt can re-query.
            log.warning("AcoustID lookup failed for %s: %s", flac_path.name, e)
            tf = TrackFingerprint(
                filename=flac_path.name,
                fingerprint=fp,
                duration_s=duration,
                acoustid_recording_mbid=None,
                acoustid_score=None,
            )
            cache_module.upsert_track_fingerprint(
                album_id,
                filename=tf.filename,
                fingerprint=tf.fingerprint,
                duration_s=tf.duration_s,
                acoustid_recording_mbid=None,
                acoustid_score=None,
            )
            out.append(tf)
            continue

        recording_mbid, score = _best_recording(results)
        tf = TrackFingerprint(
            filename=flac_path.name,
            fingerprint=fp,
            duration_s=duration,
            acoustid_recording_mbid=recording_mbid,
            acoustid_score=score,
        )
        cache_module.upsert_track_fingerprint(
            album_id,
            filename=tf.filename,
            fingerprint=tf.fingerprint,
            duration_s=tf.duration_s,
            acoustid_recording_mbid=tf.acoustid_recording_mbid,
            acoustid_score=tf.acoustid_score,
        )
        out.append(tf)

    return out


# ---------- Internal helpers ----------

def _album_flacs(album_dir: Path) -> list[Path]:
    return [
        p for p in album_dir.iterdir()
        if p.is_file() and p.suffix.lower() == ".flac" and not p.name.startswith("._")
    ]


def _dict_to_track(d: dict[str, Any]) -> TrackFingerprint:
    return TrackFingerprint(
        filename=d["filename"],
        fingerprint=d["fingerprint"],
        duration_s=float(d["duration_s"]),
        acoustid_recording_mbid=d.get("acoustid_recording_mbid"),
        acoustid_score=d.get("acoustid_score"),
    )


# ---------- Fingerprint similarity ----------

# Threshold used by the duplicate-tracks check to decide whether two
# fingerprints represent the same audio. Calibration:
#
#   identical FLACs (same source rip, same encoder)         ≈ 0.99–1.00
#   re-encode at different settings (same source audio)     ≈ 0.95–0.99
#   live vs studio of the same song                         ≈ 0.55–0.75
#   completely different songs AcoustID grouped together    ≈ 0.50–0.70
#
# 0.95 is the safe "definitely the same audio" line. Below that, we'd
# rather miss a real duplicate than falsely flag a distinct track and
# trick the user into deleting it.
SIMILARITY_DUPLICATE_THRESHOLD = 0.95


# ----- libchromaprint lookup -----
#
# `pyacoustid` ships a `chromaprint` Python module that's a ctypes wrapper
# around libchromaprint. Its import fails when ctypes.util.find_library
# can't locate the dylib — and on Apple Silicon Macs, Homebrew installs
# it to /opt/homebrew/lib/, which isn't in Python's default search path.
# Result: our helper silently returns 0.0 for every comparison.
#
# Workaround: load libchromaprint directly via ctypes.CDLL with explicit
# search of the common Homebrew + system paths, then call
# chromaprint_decode_fingerprint ourselves. Bypasses find_library entirely.

import ctypes
import ctypes.util
import os
from functools import lru_cache

# Common locations to look for the dylib. Order matters — first match wins.
_LIBCHROMAPRINT_CANDIDATES: tuple[str, ...] = (
    "/opt/homebrew/lib/libchromaprint.dylib",  # Homebrew on Apple Silicon
    "/usr/local/lib/libchromaprint.dylib",     # Homebrew on Intel + system installs
    "/usr/lib/x86_64-linux-gnu/libchromaprint.so.1",  # Debian/Ubuntu
    "/usr/lib/libchromaprint.so.1",            # other Linuxes
    "/opt/homebrew/lib/libchromaprint.1.dylib",
)


@lru_cache(maxsize=1)
def _load_libchromaprint() -> ctypes.CDLL | None:
    """Locate and load libchromaprint. Returns None and logs a one-time
    warning if it can't be found — caller falls back to equality-only
    similarity (still catches the byte-identical-duplicates case)."""
    # First try the platform's library lookup; on Linux this usually
    # works, on macOS it usually doesn't if Homebrew's lib path isn't
    # exported.
    name = ctypes.util.find_library("chromaprint")
    candidates = list(_LIBCHROMAPRINT_CANDIDATES)
    if name:
        candidates.insert(0, name)

    for path in candidates:
        try:
            if path != name and not os.path.exists(path):
                continue
            lib = ctypes.CDLL(path)
        except OSError:
            continue
        # Bind the two functions we need.
        lib.chromaprint_decode_fingerprint.argtypes = [
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.POINTER(ctypes.POINTER(ctypes.c_uint32)),
            ctypes.POINTER(ctypes.c_int),
            ctypes.POINTER(ctypes.c_int),
            ctypes.c_int,
        ]
        lib.chromaprint_decode_fingerprint.restype = ctypes.c_int
        lib.chromaprint_dealloc.argtypes = [ctypes.c_void_p]
        lib.chromaprint_dealloc.restype = None
        log.info("libchromaprint loaded from %s", path)
        return lib

    log.warning(
        "libchromaprint not found — fingerprint similarity falls back to "
        "exact-string-match only. Byte-identical duplicate FLACs still get "
        "flagged; near-duplicates from re-encodes won't. Install via "
        "`brew install chromaprint` (macOS) or your distro's chromaprint "
        "package (Linux)."
    )
    return None


def _decode_fingerprint(encoded: str) -> list[int]:
    """Decode a base64-encoded Chromaprint string to a list of 32-bit
    subfingerprints. Returns [] when the library isn't loadable or the
    decode fails."""
    lib = _load_libchromaprint()
    if lib is None:
        return []
    encoded_bytes = encoded.encode("ascii")
    ints_ptr = ctypes.POINTER(ctypes.c_uint32)()
    size = ctypes.c_int(0)
    algo = ctypes.c_int(0)
    rc = lib.chromaprint_decode_fingerprint(
        encoded_bytes, len(encoded_bytes),
        ctypes.byref(ints_ptr),
        ctypes.byref(size),
        ctypes.byref(algo),
        1,  # base64
    )
    if rc == 0 or size.value == 0 or not ints_ptr:
        return []
    try:
        return [ints_ptr[i] for i in range(size.value)]
    finally:
        # Free the buffer chromaprint allocated for us.
        lib.chromaprint_dealloc(ctypes.cast(ints_ptr, ctypes.c_void_p))


def chromaprint_similarity(fp_a: str, fp_b: str) -> float:
    """Return [0.0, 1.0] similarity between two Chromaprint fingerprint
    strings. 1.0 = identical bits, 0.0 = every bit differs.

    Two-tier strategy:
      1. Fast path: byte-identical strings → 1.0. Same audio re-encoded
         by the same fpcalc version always produces the same encoded
         output, so this catches the "I copied a FLAC" case without
         needing libchromaprint to be loadable at all.
      2. Slow path: bitwise XOR + popcount over the decoded integer
         arrays. Requires libchromaprint to be findable on the system.
         Falls through to 0.0 if the library can't be loaded — the
         duplicate-tracks check treats <0.95 as "not similar," which is
         the safer default.

    Empty / missing fingerprints return 0.0 unconditionally.
    """
    if not fp_a or not fp_b:
        return 0.0

    # Fast path: byte equality. Same audio + same encoder version →
    # identical encoded bytes, no decode needed.
    if fp_a == fp_b:
        return 1.0

    # Slow path: decode and compare bitwise.
    a_ints = _decode_fingerprint(fp_a)
    b_ints = _decode_fingerprint(fp_b)
    n = min(len(a_ints), len(b_ints))
    if n == 0:
        return 0.0

    differing = sum((a_ints[i] ^ b_ints[i]).bit_count() for i in range(n))
    total_bits = n * 32
    return 1.0 - (differing / total_bits)


def _best_recording(results: dict[str, Any]) -> tuple[str | None, float | None]:
    """Pick the highest-confidence recording MBID from an AcoustID lookup
    response. Returns (mbid, score) or (None, None) if nothing qualifies.

    AcoustID groups by acoustid (the fingerprint cluster ID); each result
    has a score and a list of recording matches. We pick the best
    recording from the best-scoring acoustid result. Strict floor at
    HIGH_CONFIDENCE_SCORE — anything below isn't safe to auto-apply."""
    best_score = 0.0
    best_mbid: str | None = None
    for result in results.get("results", []) or []:
        score = float(result.get("score", 0) or 0)
        if score < HIGH_CONFIDENCE_SCORE:
            continue
        recordings = result.get("recordings", []) or []
        for rec in recordings:
            mbid = rec.get("id")
            if mbid and score > best_score:
                best_score = score
                best_mbid = mbid
                break  # first recording on a high-score result is canonical
    if best_mbid is None:
        return None, None
    return best_mbid, best_score
