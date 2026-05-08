"""Check D2 — duplicate audio files within an album.

Uses the per-track Chromaprint fingerprints already populated by the
track-gaps fix path (or by an explicit "fingerprint album" action, when
we add one). For each album we pull every fingerprinted file, compare
them pairwise by Chromaprint similarity (with a duration sanity gate to
short-circuit the obvious negatives), and union-find them into clusters.
A cluster of two or more is reported as a duplicate finding.

We deliberately do NOT group on `acoustid_recording_mbid`: AcoustID
returns different recording MBIDs for byte-identical FLACs whenever
MusicBrainz has multiple matching releases (popular standards like
"Theme from A Summer Place" appear on dozens of compilations, each with
a distinct recording row), so MBID-grouping silently drops the most
common real-world duplicates. The Chromaprint fingerprint itself is
deterministic from the audio bytes, which is the load-bearing signal.
The MBID still rides along on the finding for context — the resolve
dialog uses it to render "MB says: <title> — <artist>".

Cross-album duplicates (the same recording on multiple compilations) are
NOT yielded here — they're often legitimate, and surfacing all of them
would drown the page. A future `cross-album-duplicates` check can do
that with library-level findings if it ever becomes useful.

Findings need fingerprints to exist. Without them, this check yields
nothing for the affected album. The current way to populate fingerprints
is to click Fix on a track-gaps finding (which falls into the fingerprint
tier when title matching can't resolve cleanly); a dedicated "fingerprint
this album" action can come later.

Fix path: deterministic file deletion. The user picks which copy to
keep via a per-finding dialog (the FindingFixButton renders a custom
"Resolve…" button for this check). The fix() method on the Check itself
is a defensive stub — actual deletion goes through a custom endpoint
that takes a filename argument so we know which copy the user chose.
"""
from __future__ import annotations

import logging
from typing import Iterable

from .. import db as db_module
from .. import fingerprint as fingerprint_module
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


# Note: we used to require acoustid_score >= 0.85 to even consider a
# fingerprint, mirroring fingerprint.HIGH_CONFIDENCE_SCORE. That filter
# was load-bearing back when we grouped by recording_mbid — a low-score
# AcoustID match meant the MBID label was suspect. With the comparison
# now done at the Chromaprint level, the AcoustID score is irrelevant:
# the fingerprint itself is deterministic from the audio. The filter is
# gone; if you reintroduce it you'll silently mask real duplicates of
# popular standards that AcoustID can't confidently disambiguate.

# Max allowed duration delta within a candidate duplicate cluster, in
# seconds. AcoustID's recording_mbid clustering is noisy for short
# instrumental cues that share thematic material — e.g. multiple
# numbered "Interlude" tracks on a Mannheim Steamroller album get
# clustered under one recording MBID despite being genuinely distinct
# pieces of audio. Real duplicates from the same source rip have
# sub-second duration diffs; anything beyond a couple seconds is the
# AcoustID match being wrong, not the user having literal duplicates.
# 2.0s is generous enough to absorb encoder/cue-point variance while
# rejecting obviously-different tracks (4s+ gaps).
_MAX_DURATION_DELTA_SEC = 2.0

# Threshold for treating a cluster as a real duplicate based on the
# bitwise similarity of the underlying Chromaprint fingerprints. AcoustID
# sometimes groups genuinely-different songs under the same recording
# MBID (crowd-sourced data quality issue), and duration alone doesn't
# catch the case where two ~3:23 tracks coincidentally have similar
# lengths. The actual fingerprints don't lie — same audio produces near-
# identical bit patterns; different audio produces substantially
# different ones. See fingerprint.SIMILARITY_DUPLICATE_THRESHOLD for
# calibration notes.
_MIN_FINGERPRINT_SIMILARITY = fingerprint_module.SIMILARITY_DUPLICATE_THRESHOLD


class DuplicateTracks(Check):
    meta = CheckMeta(
        id="duplicate-tracks",
        label="Duplicate audio files",
        description=(
            "Albums with multiple files AcoustID identified as the same "
            "canonical recording — i.e. literal copies of the same audio. "
            "Requires fingerprints first (run \"Albums not yet fingerprinted\" "
            "or click Fix on a track-gaps finding). The Resolve dialog lets "
            "you pick which copy to keep."
        ),
        category="duplicates",
        cost="cheap",
        runs_during_scan=True,
        # The "Fix" UI is a custom dialog, not a one-click action — the user
        # has to pick which copy to keep. We mark it as 'deterministic'
        # because the actual deletion is local and exact, but the
        # FindingFixButton renders a "Resolve…" button instead of "Fix" so
        # the user lands in the chooser dialog rather than a no-prompt apply.
        fix_kind="deterministic",
        # No bulk action — deletion always needs the user to pick a file.
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        """Find clusters of duplicate audio files within an album.

        We *don't* group on `acoustid_recording_mbid` here — AcoustID
        returns different recording MBIDs for two byte-identical FLACs
        when MusicBrainz has multiple matching releases ("Theme from A
        Summer Place" appears on dozens of comp releases, each with a
        distinct recording row). Grouping on MBID would silently filter
        out exactly the case the user most wants flagged.
        Instead, we pull every album's fingerprints, compare them
        pairwise, and cluster by Chromaprint similarity. The MBID still
        rides along for context but is no longer load-bearing.

        Cost: per album, this is O(n^2) pair comparisons where n is the
        number of fingerprinted tracks. For typical 10-20 track albums,
        that's 50-200 pairs — each pair is a ~100µs Chromaprint decode +
        bit-popcount, all in-process. Negligible compared to the
        per-track fingerprinting that produced the data.
        """
        # Pull every fingerprint row, regardless of whether AcoustID was
        # able to identify it. The Chromaprint fingerprint itself is
        # deterministic from the audio — same input → same bits — and
        # our similarity comparison doesn't depend on AcoustID at all.
        # We used to require acoustid_score >= 0.85 here, but that
        # silently dropped tracks AcoustID couldn't confidently identify
        # (very common for popular standards like "Theme from A Summer
        # Place" that appear on dozens of comp releases — confidence
        # gets split across many candidate recordings, none clearing the
        # threshold). With recording_mbid no longer load-bearing, the
        # score filter just hid real duplicates from us.
        # acoustid_score is intentionally NOT selected: see the comment
        # block above. It used to gate inclusion (the hashtag-confidence
        # filter), but with Chromaprint similarity now load-bearing, the
        # score is irrelevant and reading it is wasted I/O / reader noise.
        with db_module.connect() as conn:
            rows = conn.execute(
                """
                SELECT album_id, filename, duration_s, fingerprint,
                       acoustid_recording_mbid
                FROM track_fingerprints
                WHERE fingerprint IS NOT NULL
                ORDER BY album_id, filename
                """
            ).fetchall()

        # Partition by album so we can do per-album pairwise comparison.
        # (Cross-album duplicates are explicitly out of scope for this
        # check — see the module docstring.)
        from collections import defaultdict
        by_album: dict[str, list[dict]] = defaultdict(list)
        for r in rows:
            by_album[r["album_id"]].append({
                "filename":    r["filename"],
                "duration_s":  float(r["duration_s"] or 0.0),
                "fingerprint": r["fingerprint"],
                "mbid":        r["acoustid_recording_mbid"],
            })

        for album_id, tracks in by_album.items():
            if len(tracks) < 2:
                continue
            for cluster in _cluster_duplicates(album_id, tracks):
                yield _build_finding(album_id, cluster)

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        """Defensive fallback. The frontend has a custom Resolve dialog
        (FindingFixButton routes duplicate-tracks findings there by id)
        and the actual deletion happens through the per-file
        `delete_duplicate_file` endpoint, so this method should never be
        called in normal flow. The bulk `quick-fix-all` loop hits
        `try_quick_fix` instead, which is the default `None`-returning
        no-op (counted as 'not-applicable' by the dispatcher).

        We keep this as a nice message rather than letting the base
        class raise NotImplementedError so a future caller that forgets
        the special case gets a clear explanation rather than a 500.
        `user_hint` is accepted for the framework signature but
        intentionally ignored — duplicate resolution is a per-file user
        choice, not an AI lookup.
        """
        return FixResult(
            ok=False,
            message=(
                "Duplicate-tracks findings are resolved per-file via the "
                "Resolve… dialog on the album page; this check has no "
                "single-click fix because the user has to choose which "
                "copy to keep."
            ),
        )

    # No try_quick_fix override either — bulk MB-only doesn't apply to
    # duplicates. The default returns None, which the bulk endpoint counts
    # as "not-applicable."


# ---- Helpers ----

# Characters Windows refuses to allow in filenames (NTFS / exFAT / FAT, plus
# SMB shares mounted from Windows clients). Trailing dots and spaces are also
# rejected. Linux / macOS happily store them, but a copy of the library to a
# Windows drive or NAS share will silently rename or refuse those files. The
# tie-breaker prefers filenames with fewer of these characters.
_WINDOWS_FORBIDDEN_CHARS = frozenset('<>:"/\\|?*')


def _portability_score(filename: str) -> int:
    """How "portable" a filename is across filesystems. Lower is better.

    Score is the count of Windows-forbidden characters plus a small penalty
    for trailing dots/spaces (also forbidden on Windows). Used purely as a
    tie-breaker between duplicate files — we never sanitize filenames
    ourselves; the user always chooses which copy to delete.
    """
    n = sum(1 for ch in filename if ch in _WINDOWS_FORBIDDEN_CHARS)
    # Trailing dot / space — treat each as one penalty point (rare in
    # practice but cheap to score).
    if filename and filename[-1] in (" ", "."):
        n += 1
    return n


def _recommended_keep(filenames: list[str]) -> dict[str, str | None]:
    """Pick the filename to keep among duplicates.

    Strategy: lowest portability score wins. Ties (e.g. all candidates have
    no forbidden chars) get a `None` filename so the UI can skip rendering
    the recommendation — there's nothing to tie-break on. Lexicographic
    order is the silent secondary tie-breaker for determinism.

    Returns a dict with:
      - filename: the recommended-to-keep file, or None if all tie
      - reason:   short explanation suitable for inline UI copy, or None
    """
    if not filenames:
        return {"filename": None, "reason": None}
    if len(filenames) == 1:
        # Single-file finding shouldn't happen in practice (the SQL filters
        # for count > 1) but be defensive.
        return {"filename": filenames[0], "reason": None}

    scored = sorted(
        ((_portability_score(f), f) for f in filenames),
        key=lambda t: (t[0], t[1]),
    )
    best_score, best_file = scored[0]
    worst_score = scored[-1][0]
    if best_score == worst_score:
        # Everything's equally portable — no useful recommendation.
        return {"filename": None, "reason": None}

    # At least one candidate has Windows-incompatible characters; flag why.
    reason = (
        "Other copies use characters that Windows filesystems and SMB "
        "shares reject (?, :, *, etc.). Keeping this one keeps the library "
        "portable to Windows backups, NAS shares, and external drives."
    )
    return {"filename": best_file, "reason": reason}


def _cluster_duplicates(album_id: str, tracks: list[dict]) -> list[list[dict]]:
    """Find clusters of duplicate audio within `tracks` (one album's
    fingerprinted files).

    Walks every pair of tracks. A pair is treated as a duplicate edge
    when:
      - their durations differ by ≤ _MAX_DURATION_DELTA_SEC (cheap
        precondition; sub-second variance is normal for re-encodes,
        multi-second gaps mean the audio is genuinely different), AND
      - the Chromaprint fingerprints have similarity ≥
        _MIN_FINGERPRINT_SIMILARITY (the load-bearing test).

    Connected components of size ≥ 2 are the duplicate clusters. We
    use simple union-find rather than building a full graph because
    cluster sizes are tiny (typically 2, occasionally 3+) and the code
    stays readable.

    Returns a list of clusters; each cluster is a list of track dicts
    from the input. Tracks not in any duplicate edge are omitted.
    """
    n = len(tracks)
    if n < 2:
        return []

    # Union-find. parent[i] = j means i belongs to the component rooted at j.
    parent = list(range(n))
    def find(i: int) -> int:
        while parent[i] != i:
            parent[i] = parent[parent[i]]  # path compression
            i = parent[i]
        return i
    def union(i: int, j: int) -> None:
        ri, rj = find(i), find(j)
        if ri != rj:
            parent[ri] = rj

    edges_added = 0
    for i in range(n):
        for j in range(i + 1, n):
            if find(i) == find(j):
                continue  # already in the same component, skip the work
            if abs(tracks[i]["duration_s"] - tracks[j]["duration_s"]) > _MAX_DURATION_DELTA_SEC:
                continue
            sim = fingerprint_module.chromaprint_similarity(
                tracks[i]["fingerprint"], tracks[j]["fingerprint"],
            )
            if sim >= _MIN_FINGERPRINT_SIMILARITY:
                union(i, j)
                edges_added += 1

    if edges_added == 0:
        return []

    # Bucket tracks by their component root.
    from collections import defaultdict
    groups: dict[int, list[dict]] = defaultdict(list)
    for i in range(n):
        groups[find(i)].append(tracks[i])
    clusters = [g for g in groups.values() if len(g) >= 2]
    if clusters:
        log.info(
            "duplicate-tracks: album %s has %d cluster(s) totaling %d files",
            album_id, len(clusters), sum(len(c) for c in clusters),
        )
    return clusters


def _build_finding(album_id: str, cluster: list[dict]) -> Finding:
    """Render one duplicate-cluster as a Finding."""
    filenames = [t["filename"] for t in cluster]
    durations = [t["duration_s"] for t in cluster]
    # Pick the most-common non-null MBID across the cluster as the
    # representative "recording MBID" for context. Falls through to None
    # when no track in the cluster matched any MBID.
    mbids = [t["mbid"] for t in cluster if t["mbid"]]
    rep_mbid: str | None = None
    if mbids:
        from collections import Counter
        rep_mbid = Counter(mbids).most_common(1)[0][0]

    label = _readable_label(filenames[0]) if filenames else "Unknown track"
    summary = f'Duplicate audio: "{label}" ({len(cluster)} copies)'

    recommended = _recommended_keep(filenames)
    return Finding(
        album_id=album_id,
        summary=summary,
        severity="warn",
        detail={
            "recording_mbid":     rep_mbid,
            "filenames":          filenames,
            "durations_s":        durations,
            "duplicate_count":    len(cluster),
            "recommended_keep":   recommended["filename"],
            "recommended_reason": recommended["reason"],
        },
    )


def _readable_label(filename: str) -> str:
    """Strip a leading "01 - " tracknumber and the `.flac` extension to
    produce a human-readable label for the finding summary."""
    name = filename
    # Drop extension
    if name.lower().endswith(".flac"):
        name = name[:-5]
    # Drop a leading "NN - " or "NN. " prefix
    parts = name.split(" - ", 1)
    if len(parts) == 2 and parts[0].strip().isdigit():
        name = parts[1]
    parts = name.split(". ", 1)
    if len(parts) == 2 and parts[0].strip().isdigit():
        name = parts[1]
    return name.strip()
