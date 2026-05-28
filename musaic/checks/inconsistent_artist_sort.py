"""Check S2 — clusters of credits whose primary artist matches but
whose sort tags don't agree, ACROSS BOTH album-level and track-level
fields.

Why this matters: jazz / classical / easy-listening libraries are full
of credit variations like:

    "Ray Conniff"
    "Ray Conniff & His Orchestra"
    "Ray Conniff, his Orchestra and Chorus"
    "Ray Conniff Featuring The Singers"

Each is a legitimate `album_artist` for whatever release they apply to,
but for sort purposes they're all the same artist. Plex / Roon use
ALBUMARTISTSORT for album-grouping AND ARTISTSORT for track-level
artist views, so when these credits all sort under their literal
display values you get four separate "Ray ..." entries in your sidebar
instead of one. The fix is to set the SAME canonical sort value
("Conniff, Ray") across every credit instance in the cluster.

Track-level relevance (v11+): compilations are the obvious case —
"Best of Easy Listening" with four tracks credited to four different
"Ray Conniff" variations. Each track's ARTISTSORT needs to converge
on the same value as the album-level entries on Conniff's solo
albums, so Plex's track-level "Ray Conniff" view shows everything.

Detection — clustering: strip a defined set of credit suffixes
("& His Orchestra", "and Her Singers", ", his ... and ...",
"Featuring X", "feat. X", "with X") and articles, lowercase + collapse
whitespace, group BOTH album-level credits AND distinct track-level
credits by the resulting "primary token".

Detection — classification: within each cluster of ≥2 members, the
ONLY question we ask is "do all members agree on a non-empty sort
value?" — empty vs non-empty, exact match across members. We do NOT
re-validate the *content* of sort values against the cluster's
credits; presence of a deliberate sort value is treated as the user
having addressed the field. See `_classify_cluster` for the design
note on why — the earlier rubric had blind spots ("Spike Jones New
Band" + "Jones, Spike") that created an inescapable re-flag loop.

Three cluster states the finding tracks:

  - PROPAGATABLE: at least one cluster member has its sort tag set;
    some others are empty. Tier-1 deterministic fix: propagate the
    one non-empty value to all empty members.

  - UNSORTED: every member is empty. The user needs to choose /
    type a canonical form. Custom Resolve dialog lets them type
    once and we bulk-apply.

  - CONFLICTING: multiple distinct non-empty sort values across the
    cluster. User picks which one wins; same dialog flow.

Fix path is custom — fix() is a stub (like duplicate-tracks), the
real action goes through a dedicated endpoint that takes the user's
chosen sort value and bulk-writes it. For album members the write
target is ALBUMARTISTSORT on every FLAC in the album; for track
members the target is ARTISTSORT on the listed filenames only. The
bulk_action_label triggers an auto-propagate sweep that handles
PROPAGATABLE clusters across the whole library in one click.

Member shape (in finding.detail.members):

    # Album-level entry — fix target is the whole album's
    # ALBUMARTISTSORT.
    {"kind": "album", "album_id": "...", "dir_name": "...",
     "album_artist": "Ray Conniff", "sort": "Conniff, Ray"}

    # Track-level entry — fix target is ARTISTSORT on the listed
    # filenames within one album. Multiple tracks of the same
    # album sharing the same (artist, sort) pair collapse into
    # one entry with a list of filenames.
    {"kind": "track", "album_id": "...", "dir_name": "...",
     "filenames": ["01 - foo.flac", "03 - bar.flac"],
     "artist": "Ray Conniff & His Orchestra", "sort": ""}
"""
from __future__ import annotations

import logging
import re
from collections import defaultdict
from typing import Iterable, Literal

from .. import db as db_module
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


# Regex matching credit suffix patterns we strip when computing the
# primary token. Order matters within an alternation — longer / more
# specific patterns first so they don't get partially matched by
# shorter ones. Anchored at the END of the string with optional
# trailing whitespace; case-insensitive.
#
# What each alternative covers:
#   ", his|her ... [and|with] ..."  — ", his Orchestra and Chorus"
#   " & his|her ..."                 — " & His Orchestra"
#   " and his|her ..."               — " and Her Singers"
#   " (featuring|feat\.|ft\.|with) ..." — feature credits
#   " and the ..."                   — " and the Singers"
_CREDIT_SUFFIX_RE = re.compile(
    r"""(?ix)                          # case-insensitive, verbose
    \s*
    (?:
        ,\s*(?:his|her)\s+\S+(?:\s+(?:and|with)\s+\S+)?   # , his Orchestra and Chorus
      | \s*&\s*(?:his|her)\s+\S+(?:\s+\S+)*               # & His Orchestra
      | \s+and\s+(?:his|her)\s+\S+(?:\s+\S+)*             # and His Choir
      | \s+(?:featuring|feat\.|ft\.|with)\s+.+            # Featuring X / feat. X
      | \s+and\s+the\s+\S+(?:\s+\S+)?                     # and the Singers
      | \s*&\s*the\s+\S+(?:\s+\S+)*                       # & the Shondells / & the Comets
    )
    \s*$
    """
)

# Leading articles to strip when deriving the primary token. Sort
# behavior for articles is already handled by missing-sort-tags; here
# we strip them so cluster matching survives ("The Beatles" and
# "The Beatles & His Backing Band" cluster on "beatles", not
# "the beatles" vs. "the beatles & his...").
_LEADING_ARTICLE_RE = re.compile(r"^(the|a|an)\s+", re.IGNORECASE)


# album_artist values that are themselves the canonical sort form and
# should NOT be rotated, normalized, or otherwise touched. "Various
# Artists" is MB's canonical compilation credit and the standard Plex
# / Roon expects to see verbatim — flagging it as needing
# "Artists, Various" treatment would scramble every comp in the
# library. We compare case-insensitively after a trim.
_NEVER_NORMALIZE: frozenset[str] = frozenset({
    "various artists",
    "various",
    "various composers",       # used by some classical labels
    "various performers",
    "soundtrack",              # generic ST credit on some rips
    "original soundtrack",
    "no artist",               # pathological but seen in the wild
    "unknown artist",
})


def _is_never_normalize(album_artist: str | None) -> bool:
    """True when the credit is already-canonical and shouldn't be
    touched by any sort-tag normalization. Centralized here so the
    cluster check, the article check, and the AI suggestion path can
    all agree on the same skip-set."""
    if not album_artist:
        return False
    return album_artist.strip().lower() in _NEVER_NORMALIZE

ClusterState = Literal["propagatable", "unsorted", "conflicting"]


class InconsistentArtistSort(Check):
    meta = CheckMeta(
        id="inconsistent-artist-sort",
        label="Inconsistent artist sort",
        description=(
            "Clusters of albums whose album_artist values are credit "
            "permutations of the same primary artist (\"Ray Conniff\" / "
            "\"Ray Conniff & His Orchestra\" / \"Ray Conniff, his "
            "Orchestra and Chorus\") but whose sort tags don't agree. "
            "Setting the same album_artist_sort across the cluster "
            "groups them under one entry in Plex / Roon sidebars."
        ),
        category="metadata",
        cost="cheap",
        runs_during_scan=True,
        # Custom resolve dialog (like duplicate-tracks). The Fix button
        # routes there; deterministic from there since we just write
        # whatever sort string the user confirms.
        fix_kind="deterministic",
        # Bulk action sweeps clusters where ONE member already has a
        # canonical sort, propagating it to the rest. Doesn't touch
        # unsorted / conflicting clusters — those need user choice.
        bulk_action_label="Propagate where possible",
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        with db_module.connect() as conn:
            rows = conn.execute(
                """
                SELECT id, dir_name, album_artist, album_artist_sort,
                       track_artists_json
                FROM albums
                WHERE library_path = ?
                  AND missing_since IS NULL
                """,
                (scope.library_path,),
            ).fetchall()

        # Bucket BOTH album-level and track-level credits by primary
        # token. An album with album_artist "Ray Conniff" lands in the
        # same bucket as a compilation track credited "Ray Conniff &
        # His Orchestra". The fix flow can then propagate one
        # canonical sort across both — that's the whole point of
        # extending the check to track level.
        #
        # Skip rules:
        #   - Empty album_artist OR a "never-normalize" sentinel
        #     (Various Artists, Soundtrack…) doesn't contribute the
        #     album-level entry. Track-level entries from those
        #     same compilations DO still cluster — that's where the
        #     value lives.
        #   - Empty primary token after suffix-stripping doesn't
        #     cluster (rare; all-suffix credit like "& His Orchestra"
        #     alone).
        clusters: dict[str, list[dict]] = defaultdict(list)
        for r in rows:
            album_id = r["id"]
            dir_name = r["dir_name"]
            album_artist = r["album_artist"] or ""

            # Album-level contribution.
            if album_artist.strip() and not _is_never_normalize(album_artist):
                token = _primary_token(album_artist)
                if token:
                    clusters[token].append({
                        "kind":         "album",
                        "album_id":     album_id,
                        "dir_name":     dir_name,
                        "album_artist": album_artist,
                        "sort":         (r["album_artist_sort"] or "").strip(),
                    })

            # Track-level contributions. Each distinct (artist, sort)
            # tuple within an album collapses to one entry that
            # carries the list of filenames sharing that credit —
            # a 12-track compilation with one "Ray Conniff" credit
            # block becomes ONE track-level cluster member, not 12.
            track_artists_json = r["track_artists_json"] if "track_artists_json" in r.keys() else None
            track_artists = _parse_track_artists_json(track_artists_json)
            grouped: dict[tuple[str, str], list[str]] = defaultdict(list)
            for t in track_artists:
                t_artist = (t.get("artist") or "").strip()
                if not t_artist:
                    continue
                if _is_never_normalize(t_artist):
                    continue
                t_sort = (t.get("artist_sort") or "").strip()
                grouped[(t_artist, t_sort)].append(t["filename"])
            for (t_artist, t_sort), filenames in grouped.items():
                # Skip the redundant case: track artist is identical
                # to the album artist AND its sort agrees. The
                # album-level entry already represents this credit.
                if (
                    t_artist == album_artist.strip()
                    and t_sort == (r["album_artist_sort"] or "").strip()
                ):
                    continue
                token = _primary_token(t_artist)
                if not token:
                    continue
                clusters[token].append({
                    "kind":      "track",
                    "album_id":  album_id,
                    "dir_name":  dir_name,
                    "filenames": sorted(filenames),
                    "artist":    t_artist,
                    "sort":      t_sort,
                })

        for token, members in clusters.items():
            if len(members) < 2:
                continue  # single member — nothing to normalize against
            state, canonical = _classify_cluster(members)
            # already-consistent clusters yield no finding; nothing to fix
            if state == "consistent":
                continue
            yield _build_finding(token, members, state, canonical)

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        """Stub. Resolution goes through the cluster-apply endpoint
        because the user picks WHICH sort value to bulk-write across
        the cluster — that decision can't be encoded in a no-prompt
        Fix click. The FindingFixButton recognizes this check_id and
        opens a custom dialog instead of calling this method.

        Implemented (rather than NotImplementedError) so the framework
        dispatcher gets a clean message if anything calls fix() directly
        — e.g. the bulk loop, which we route through try_quick_fix
        for the propagate-where-possible behavior."""
        return FixResult(
            ok=False,
            message=(
                "Inconsistent-artist-sort findings are resolved per-cluster "
                "via the Resolve… dialog; this check has no single-click "
                "fix because the user has to confirm which canonical sort "
                "value to bulk-apply."
            ),
        )

    def try_quick_fix(self, finding: Finding) -> FixResult | None:
        """Bulk-mode entry: only acts on PROPAGATABLE clusters where
        one member already has a canonical sort tag. Skips UNSORTED
        and CONFLICTING clusters (those need user input from the
        Resolve dialog).

        Reads the cluster info out of finding.detail so we don't have
        to re-cluster on every call. The actual write lives in the
        shared `apply_cluster_sort` helper — same code path the custom
        Resolve dialog hits, just driven by the cluster's existing
        canonical value instead of a user-typed one."""
        detail = finding.detail or {}
        state = detail.get("state")
        canonical = detail.get("canonical_sort")
        members = detail.get("members") or []
        if state != "propagatable" or not canonical or not members:
            return FixResult(
                ok=False,
                message=f"Skipped (state={state}); needs user input via Resolve dialog",
            )
        applied = apply_cluster_sort(
            members=members,
            sort_value=canonical,
        )
        return FixResult(
            ok=True,
            message=f"Propagated sort '{canonical}' to {applied} member(s) in cluster",
        )


# ---- Helpers (also used by the cluster-apply endpoint) -------------


def _primary_token(album_artist: str | None) -> str:
    """Reduce a credit string to its primary-artist token for clustering.

    Steps: strip credit suffixes (Orchestra/Choir/feat./etc.), strip
    leading articles, lowercase, collapse whitespace. The result is
    the dictionary key used to cluster albums.

    Returns empty string when the input is None / empty / reduces to
    nothing (a credit that's all suffix doesn't cluster against
    anything). Empty results don't get flagged.
    """
    if not album_artist:
        return ""
    s = album_artist.strip()
    # Repeatedly strip suffixes — handles compound credits like
    # "X & His Orchestra Featuring Y" where two patterns apply.
    for _ in range(3):
        new = _CREDIT_SUFFIX_RE.sub("", s).strip()
        if new == s:
            break
        s = new
    s = _LEADING_ARTICLE_RE.sub("", s).strip()
    s = re.sub(r"\s+", " ", s)
    return s.lower()


def _classify_cluster(
    members: list[dict],
) -> tuple[Literal["consistent", "propagatable", "unsorted", "conflicting"], str | None]:
    """Inspect a cluster's existing sort tags. Returns (state, value)
    where `value` is the sort string the bulk action should apply
    when state == "propagatable" or "consistent" (None for the others).

    Decision rules — empty vs non-empty is the only filter:

      consistent   — every member has the SAME non-empty value. The
                     user has addressed this cluster; nothing to do,
                     no finding emitted.
      propagatable — exactly one distinct non-empty value exists and
                     some members are empty. Bulk action spreads that
                     value across the cluster.
      unsorted     — every member is empty. User must type a fresh
                     canonical value.
      conflicting  — two or more distinct non-empty values. User
                     picks which one wins.

    Design note — we deliberately do NOT re-validate the *content* of
    sort values against the cluster's display credits. The previous
    implementation cross-checked each sort value's primary token
    against the cluster's primary token, trying to detect stale /
    mis-tagged carryovers ("Drifters, The" on a Skyliners track
    cluster). That rubric had blind spots — credits like "Spike Jones
    New Band" produce cluster token "spike jones new band" while the
    correct sort "Jones, Spike" produces token "spike jones". They
    don't match, so the cluster was perpetually re-flagged even
    though every track was correctly sorted. The fix loop was
    inescapable for any credit with an ensemble qualifier the suffix
    regex didn't recognize ("Quartet", "Quintet", "New Band", "Trio").

    Trading off: under the new rule, the rare "stale tag from a bad
    upstream import" case would silently survive a re-run. The user
    has to spot and correct it (or its presence would surface as a
    cluster-wide conflict if another member has the *correct* sort).
    In practice, deliberate user-set values vastly outnumber bad
    imports, and the constant-friction failure mode is far worse than
    the rare false-acceptance one.

    Single-name artists ("Madonna", "U2"), canonical ensembles
    ("London Symphony Orchestra"), and "Last, First" rotations all
    pass naturally — they're all just non-empty values that agree
    across the cluster.
    """
    sort_values = [(m.get("sort") or "").strip() for m in members]
    distinct_non_empty: set[str] = {v for v in sort_values if v}
    has_empty = any(not v for v in sort_values)

    if not distinct_non_empty:
        # Every member is empty.
        return "unsorted", None

    if len(distinct_non_empty) == 1:
        value = next(iter(distinct_non_empty))
        # All non-empty AND all the same → already consistent. Mixed
        # empty + same-value → propagate.
        return ("consistent" if not has_empty else "propagatable"), value

    # Multiple distinct non-empty values → user picks which wins.
    return "conflicting", None


def _build_finding(
    token: str,
    members: list[dict],
    state: str,
    canonical: str | None,
) -> Finding:
    # Representative album: alphabetically-first album_id across all
    # members (album OR track kind), deterministic so the same cluster
    # generates the same finding row across runs (so the upsert in
    # the runner can dedupe properly).
    representative_id = sorted({m["album_id"] for m in members})[0]
    n = len(members)
    # Distinct displayed-credit strings, drawn from whichever field
    # the member kind uses for the credit (album_artist vs artist).
    distinct_credits = sorted({_credit_of(m) for m in members})
    n_album = sum(1 for m in members if m.get("kind") != "track")
    n_track = sum(1 for m in members if m.get("kind") == "track")
    track_count = sum(
        len(m.get("filenames") or []) for m in members
        if m.get("kind") == "track"
    )
    head = f"{n} credit instances"
    if n_track > 0:
        head = (
            f"{n_album} album-level + {n_track} track-level "
            f"({track_count} tracks)"
        )
    summary = (
        f"{head} for {distinct_credits[0]!r}"
        + (f" / {distinct_credits[1]!r}" if len(distinct_credits) > 1 else "")
        + (f" + {len(distinct_credits) - 2} more variation(s)"
           if len(distinct_credits) > 2 else "")
    )
    # Headline for the Health detail row. The default row template
    # would show "Album — Artist" of the representative album_id, but
    # this check is about an artist credit cluster, not about a
    # specific album — picking one to display is misleading. We
    # surface the canonical credit (shortest non-empty, usually the
    # display-form before any "& His Orchestra" / "feat. X" suffix
    # got tacked on) and let the row template prefer this.
    headline = _canonical_display(distinct_credits) or token.title()
    if len(distinct_credits) > 1:
        headline = f"{headline} ({len(distinct_credits)} credit variations)"
    return Finding(
        album_id=representative_id,
        summary=summary,
        severity="info",
        detail={
            "primary_token":  token,
            "state":          state,
            "canonical_sort": canonical,
            "headline":       headline,
            "members": members,
        },
    )


def _canonical_display(credits: list[str]) -> str:
    """Pick the most "canonical-looking" displayed credit from the
    cluster — the one most likely to be the user's mental name for
    the artist. Heuristic: prefer the shortest non-empty credit
    (since variations are usually built by appending suffixes like
    " & His Orchestra" or " feat. The Singers"). Tie-break by
    alphabetical order so the choice is deterministic across runs."""
    candidates = [c for c in credits if c]
    if not candidates:
        return ""
    candidates.sort(key=lambda c: (len(c), c))
    return candidates[0]


def _credit_of(member: dict) -> str:
    """Return the displayed-credit string from a cluster member,
    independent of kind. Album members use album_artist; track
    members use artist."""
    if member.get("kind") == "track":
        return member.get("artist") or ""
    return member.get("album_artist") or ""


def _parse_track_artists_json(raw: str | None) -> list[dict[str, str]]:
    """Local mirror of cache._parse_track_artists_json — kept here so
    the check can decode the column without importing the cache module
    (which itself imports a bunch of other things)."""
    import json
    if not raw:
        return []
    try:
        loaded = json.loads(raw)
    except Exception:
        return []
    if not isinstance(loaded, list):
        return []
    out: list[dict[str, str]] = []
    for item in loaded:
        if not isinstance(item, dict):
            continue
        filename = str(item.get("filename") or "")
        if not filename:
            continue
        out.append({
            "filename":    filename,
            "artist":      str(item.get("artist") or ""),
            "artist_sort": str(item.get("artist_sort") or ""),
        })
    return out


def apply_cluster_sort(
    *,
    members: list[dict],
    sort_value: str,
    # Backwards-compat alias: older callers passed `album_ids`. Treated
    # as a list of album-kind members so existing code paths keep
    # working until they're migrated. New code should pass `members`.
    album_ids: list[str] | None = None,
) -> int:
    """Write `sort_value` to the right Vorbis tag for every cluster
    member, then trigger one post-apply check rerun. Returns the
    number of members where the write succeeded.

    Member kinds and what they write:
      - kind="album": ALBUMARTISTSORT on every FLAC in the album.
        Coalesced per album_id when multiple album-kind entries
        target the same album (rare, but cheap to dedupe).
      - kind="track": ARTISTSORT on the listed filenames within
        one album, via per-track suggestions. The album's other
        tracks are untouched.

    Multiple track-kind members for the same album_id are coalesced
    so we issue one apply_direct call per album with the merged
    track_suggestions dict, instead of N round-trips through the
    apply pipeline for one compilation.

    All work goes through `apply_direct` so the FLAC tags actually
    change on disk — Plex / Roon read tags, not our cache. Uses
    `in_bulk_run` so per-album post-scan check reruns are suppressed;
    one rerun fires at the end if any write succeeded.
    """
    # Backwards-compat: accept the old album_ids signature.
    if album_ids is not None and not members:
        members = [{"kind": "album", "album_id": aid} for aid in album_ids]

    log.info(
        "apply_cluster_sort START: %d member(s), sort_value=%r",
        len(members), sort_value,
    )
    if not members or not sort_value:
        log.warning("apply_cluster_sort: empty inputs, skipping")
        return 0

    # Coalesce per-album: each album_id gets at most one apply_direct
    # call, with the union of album-level + per-track writes the
    # cluster requires.
    #
    # `per_album` shape:
    #   album_id → {
    #     "album_level":      bool,                    # write ALBUMARTISTSORT?
    #     "track_filenames":  set[str],                # write ARTISTSORT to these
    #   }
    per_album: dict[str, dict] = {}
    for m in members:
        aid = m.get("album_id")
        if not aid:
            continue
        bucket = per_album.setdefault(
            aid, {"album_level": False, "track_filenames": set()},
        )
        if m.get("kind") == "track":
            for fn in (m.get("filenames") or []):
                bucket["track_filenames"].add(fn)
        else:
            bucket["album_level"] = True

    from . import _apply as direct_apply
    token = direct_apply.in_bulk_run.set(True)
    succeeded = 0
    try:
        for aid, plan in per_album.items():
            suggestions: dict[str, str] = {}
            if plan["album_level"]:
                suggestions["album_artist_sort"] = sort_value
            track_suggestions: dict[str, dict[str, str]] = {}
            for fn in plan["track_filenames"]:
                track_suggestions[fn] = {"artist_sort": sort_value}
            if not suggestions and not track_suggestions:
                continue
            ok, msg = direct_apply.apply_direct(
                album_id=aid,
                suggestions=suggestions or None,
                track_suggestions=track_suggestions or None,
            )
            if ok:
                succeeded += 1
                log.info("apply_cluster_sort album=%s OK: %s", aid, msg)
            else:
                log.warning(
                    "apply_cluster_sort album=%s FAILED: %s", aid, msg,
                )
    finally:
        direct_apply.in_bulk_run.reset(token)
    log.info(
        "apply_cluster_sort writes complete: %d/%d album(s) succeeded",
        succeeded, len(per_album),
    )

    # One post-apply rerun for the whole cluster (saves N×checks-runtime
    # for clusters of any size). Errors are logged and swallowed — never
    # fail the bulk-apply over a check rerun hiccup.
    #
    # IMPORTANT: run_check is async. Bare-calling it without await
    # creates a coroutine that's immediately discarded — work never
    # runs, the finding never resolves, and the user thinks the apply
    # silently failed. Mirror the asyncio.run() pattern from
    # main._run_post_scan_checks so we get the same fresh-event-loop
    # semantics from this sync path.
    if succeeded > 0:
        log.info("apply_cluster_sort: starting post-apply check rerun")
        try:
            import asyncio
            from .. import config as config_module
            from pathlib import Path
            cfg = config_module.load_config()
            if cfg.library_dir:
                from .runner import run_check
                from . import REGISTRY as CHECK_REGISTRY
                lib_path = str(Path(cfg.library_dir).expanduser().resolve())
                for cls in CHECK_REGISTRY:
                    if not cls.meta.runs_during_scan:
                        continue
                    try:
                        asyncio.run(run_check(
                            cls.meta.id,
                            library_path=lib_path,
                            scope_kind="all",
                        ))
                    except Exception:
                        log.exception(
                            "post-cluster-apply rerun of check %s failed (continuing)",
                            cls.meta.id,
                        )
            else:
                log.warning(
                    "apply_cluster_sort: no library_dir configured, "
                    "skipping post-apply rerun (finding won't auto-resolve)",
                )
        except Exception:
            log.exception("post-cluster-apply check sweep failed (continuing)")
        log.info("apply_cluster_sort: post-apply rerun complete")

        # Publish BulkBatchCompleted as a fallback: if the sync rerun
        # sweep above silently failed (every asyncio.run raised, or
        # cfg.library_dir was missing), the bus subscriber's
        # library-wide rerun is the only remaining path that can
        # auto-resolve the cluster's finding. Cheap on success too —
        # the rerun is idempotent and the relay just delivers a
        # broad-invalidate event to connected clients.
        try:
            from .. import library_events
            library_events.publish(library_events.BulkBatchCompleted())
        except Exception:
            log.exception(
                "library_events publish failed for apply_cluster_sort end"
            )
    else:
        log.warning(
            "apply_cluster_sort: zero albums succeeded, skipping rerun "
            "(finding will stay open)",
        )
    log.info("apply_cluster_sort END: returning succeeded=%d", succeeded)
    return succeeded
