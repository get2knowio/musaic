"""Per-album audio analysis: BPM detection (librosa) and ReplayGain (pyloudnorm).

Both pipelines share the same shape:
  - decode each FLAC once,
  - compute the analysis result for that track,
  - write the result back to the FLAC's Vorbis comments,
  - yield an SSE event so the UI can update.

For ReplayGain we additionally need album-level aggregates, so after every
track is measured we compute the energy-weighted album loudness + max
sample peak across tracks and write `replaygain_album_*` to every FLAC.

The analyze pipeline is a sibling to `services.apply` — same album-mutation
bones (per-file write + post-write rescan + sidecar resync + resolver
invalidate) but driven by computed values instead of MB suggestions.
Rather than thread BPM/RG keys through `tags._FIELD_TO_VORBIS`, we write
directly to FLAC here: the values come from a deterministic analysis, not
from a user-editable suggestion stream, so they don't need the diff/dry-run
machinery.

Event sequence:
  ("start",        {what, file_count})
  ("file_started", {index, filename, path})
  ("file_done",    {index, filename, bpm? | replaygain_track_gain? + replaygain_track_peak?, error?})
  ("album_done",   {replaygain_album_gain, replaygain_album_peak})  # RG only
  ("updated_album", fresh album dict)
  ("done",         {analyzed_count, errors, what})
  ("error",        {detail})  # only on fatal config errors

The endpoint serializes these as SSE; cancellation is best-effort (a
client disconnect mid-track lets that track finish — the saved tags are
still useful).
"""
from __future__ import annotations

import logging
import math
from pathlib import Path
from typing import Any, Iterator, Literal

from mutagen.flac import FLAC

from .. import cache as cache_module
from ..scanner import album_to_dict, scan_one_album

_log = logging.getLogger(__name__)

AnalysisKind = Literal["bpm", "replaygain"]


# ---------- Capability probes ----------

def _try_import_librosa():
    try:
        import librosa  # type: ignore
        return librosa
    except ImportError:
        return None


def _try_import_loudness():
    try:
        import numpy as np  # type: ignore
        import pyloudnorm  # type: ignore
        import soundfile  # type: ignore
        return np, pyloudnorm, soundfile
    except ImportError:
        return None


# ---------- Per-track analysis ----------

def _detect_bpm(librosa_mod, path: Path) -> float:
    """Return the detected tempo in beats-per-minute.

    Uses librosa's beat tracker on a 22 kHz mono downmix — same setup the
    flac-bpm skill uses. Wrapped in `float(np.atleast_1d(...))` because
    librosa's return shape changed between 0.9 and 0.10 (scalar vs 1-element
    array); this normalizes to a plain Python float either way.
    """
    import numpy as np  # local import — librosa pulls it in when available

    y, sr = librosa_mod.load(str(path), sr=22050, mono=True)
    tempo, _beats = librosa_mod.beat.beat_track(y=y, sr=sr)
    return float(np.atleast_1d(tempo)[0])


def _measure_track_loudness(np_mod, pyln_mod, sf_mod, path: Path) -> tuple[float, float]:
    """Return (integrated_LUFS, sample_peak_ratio) for one track.

    Sample peak is `max(|samples|)` against the full-scale range — i.e.
    a value in [0, 1] for normal PCM. ReplayGain 2.0 stores this as a
    float (not dB).
    """
    data, sr = sf_mod.read(str(path), always_2d=False)
    # pyloudnorm wants float in [-1, 1]; soundfile returns floats by default
    # for FLAC (it normalizes integer PCM via dtype="float64"), so no scaling.
    meter = pyln_mod.Meter(sr)
    loudness = float(meter.integrated_loudness(data))
    peak = float(np_mod.max(np_mod.abs(data))) if data.size else 0.0
    return loudness, peak


def _format_gain(reference_lufs: float, measured_lufs: float) -> str:
    """ReplayGain 2.0 string: gain that brings track to the reference.

    The reference is -18 LUFS (EBU R128). A track measured at -22 LUFS
    needs +4 dB to reach the reference, so gain = -18 - (-22) = +4.
    """
    gain_db = reference_lufs - measured_lufs
    return f"{gain_db:+.2f} dB"


def _format_peak(peak: float) -> str:
    """ReplayGain 2.0 peak: positive float, six decimal places."""
    return f"{peak:.6f}"


def _aggregate_album(track_results: list[tuple[float, float, float]]) -> tuple[float, float] | None:
    """Energy-weighted album LUFS + max peak across tracks.

    Each input tuple is `(duration_s, lufs, peak)`. Tracks with infinite
    or NaN loudness (silent / too-short to gate) are dropped from the
    weighted mean but still considered for peak.

    Album LUFS = 10·log10( Σ d_i · 10^(lufs_i/10) / Σ d_i ).
    Returns None when no track had a finite loudness measurement.
    """
    finite = [(d, l, p) for (d, l, p) in track_results if math.isfinite(l) and d > 0]
    peak = max((p for (_d, _l, p) in track_results), default=0.0)
    if not finite:
        return None
    energy_sum = sum(d * (10.0 ** (l / 10.0)) for (d, l, _p) in finite)
    duration_sum = sum(d for (d, _l, _p) in finite)
    if duration_sum == 0 or energy_sum <= 0:
        return None
    album_lufs = 10.0 * math.log10(energy_sum / duration_sum)
    return album_lufs, peak


# ---------- Tag write ----------

def _write_tags(path: Path, updates: dict[str, str]) -> None:
    """Stamp a small set of Vorbis comments onto a FLAC. Writes only the
    keys passed in; everything else is left alone."""
    audio = FLAC(str(path))
    for key, value in updates.items():
        audio[key] = value
    audio.save()


# ---------- Pipeline ----------

REPLAYGAIN_REFERENCE_LUFS = -18.0


def analyze_album_iter(
    *,
    album: dict[str, Any],
    library_dir: str,
    what: AnalysisKind,
) -> Iterator[tuple[str, dict[str, Any]]]:
    """Run the requested analysis across every FLAC in the album and stream
    progress + final state.

    `library_dir` is forwarded to the cache upsert so the refreshed album
    row lands at the right `library_path`. Pass `""` to skip the upsert
    (rarely useful outside tests).
    """
    album_id = album["id"]
    album_path_str = album["path"]
    album_path = Path(album_path_str)

    if what == "bpm":
        librosa_mod = _try_import_librosa()
        if librosa_mod is None:
            yield "error", {
                "detail": "BPM detection requires librosa. Install with `uv sync`.",
            }
            return
    elif what == "replaygain":
        loudness_mods = _try_import_loudness()
        if loudness_mods is None:
            yield "error", {
                "detail": "ReplayGain requires pyloudnorm + soundfile. Install with `uv sync`.",
            }
            return
        np_mod, pyln_mod, sf_mod = loudness_mods
    else:
        yield "error", {"detail": f"Unknown analysis kind: {what!r}"}
        return

    flacs = sorted(
        p for p in album_path.iterdir()
        if p.is_file() and p.suffix.lower() == ".flac" and not p.name.startswith("._")
    )

    yield "start", {"what": what, "file_count": len(flacs)}

    if not flacs:
        yield "done", {"what": what, "analyzed_count": 0, "errors": 0}
        return

    analyzed_count = 0
    errors = 0
    # For ReplayGain album-mode: collect (duration, lufs, peak) per track.
    rg_tracks: list[tuple[float, float, float]] = []

    for index, path in enumerate(flacs):
        yield "file_started", {"index": index, "filename": path.name, "path": str(path)}

        try:
            if what == "bpm":
                bpm = _detect_bpm(librosa_mod, path)
                _write_tags(path, {"bpm": f"{bpm:.2f}"})
                analyzed_count += 1
                yield "file_done", {
                    "index": index,
                    "filename": path.name,
                    "bpm": round(bpm, 2),
                }
            else:
                lufs, peak = _measure_track_loudness(np_mod, pyln_mod, sf_mod, path)
                gain_str = _format_gain(REPLAYGAIN_REFERENCE_LUFS, lufs)
                peak_str = _format_peak(peak)
                _write_tags(path, {
                    "replaygain_track_gain": gain_str,
                    "replaygain_track_peak": peak_str,
                    "replaygain_reference_loudness": f"{REPLAYGAIN_REFERENCE_LUFS:.2f} LUFS",
                })
                # Need the duration for energy-weighted album loudness.
                # mutagen-cached FLAC info already loaded via _write_tags
                # is gone; re-open just to read .info.length cheaply.
                duration = float(FLAC(str(path)).info.length or 0.0)
                rg_tracks.append((duration, lufs, peak))
                analyzed_count += 1
                yield "file_done", {
                    "index": index,
                    "filename": path.name,
                    "replaygain_track_gain": gain_str,
                    "replaygain_track_peak": peak_str,
                }
        except Exception as e:
            errors += 1
            _log.exception("audio analysis failed for %s", path)
            yield "file_done", {
                "index": index,
                "filename": path.name,
                "error": str(e),
            }

    # Album-level aggregation for ReplayGain.
    if what == "replaygain" and rg_tracks:
        agg = _aggregate_album(rg_tracks)
        if agg is not None:
            album_lufs, album_peak = agg
            album_gain_str = _format_gain(REPLAYGAIN_REFERENCE_LUFS, album_lufs)
            album_peak_str = _format_peak(album_peak)
            # Stamp album-level tags onto every FLAC. Mutagen rewrites
            # only the metadata block, so this is fast even for long albums.
            for path in flacs:
                try:
                    _write_tags(path, {
                        "replaygain_album_gain": album_gain_str,
                        "replaygain_album_peak": album_peak_str,
                    })
                except Exception:
                    _log.exception("failed to write album RG tags to %s", path)
                    errors += 1
            yield "album_done", {
                "replaygain_album_gain": album_gain_str,
                "replaygain_album_peak": album_peak_str,
            }

    # Refresh the album row so the cache + UI show the new tags. The
    # scanner currently doesn't read BPM/RG into the album-level dict
    # (those are per-track), but a rescan still bumps `dir_mtime` /
    # ensures the row matches disk. The frontend re-fetches /tracks too.
    fresh = scan_one_album(album_path_str)
    if fresh is not None:
        fresh_dict = album_to_dict(fresh)
        if library_dir:
            cache_module.upsert_album(library_dir, fresh_dict)
        yield "updated_album", fresh_dict

    yield "done", {"what": what, "analyzed_count": analyzed_count, "errors": errors}
