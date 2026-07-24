"""Check F1 — albums with one or more FLAC files whose filename
doesn't match the user-configured filename template.

Why this matters: ad-hoc-ripped libraries collect filename drift
over time (different rippers use different conventions, metadata
edits happen long after the file was named, the user changes their
preferred convention mid-library). Plex / Roon read tags so the
mismatch doesn't break playback, but it makes filesystem browsing
ugly, complicates moving subsets to other tools, and creates
inconsistency that compounds with every new rip.

Templates live in user settings:

  - filename_template_single_artist  (e.g. "{tracknumber:02} - {title}")
  - filename_template_compilation    (e.g. "{tracknumber:02} - {artist} - {title}")

The check reads the FLAC tags at run time (cost=medium) rather than
relying on the cache, since per-track titles aren't denormalized
anywhere and we want the comparison to reflect what's really on
disk right now.

Per-album finding shape (in detail):

    {
      "dir_name":   "Compilations - 50 Romantic Records [FLAC]",
      "is_compilation": True,
      "template":   "{tracknumber:02} - {artist} - {title}",
      "renames": [
        {
          "filename":  "01 The Beatles - Yesterday.flac",
          "expected":  "01 - The Beatles - Yesterday.flac",
          "tracknumber": 1,
          "title":     "Yesterday",
          "artist":    "The Beatles",
        },
        ...
      ]
    }

Fix path is custom (deterministic): a dedicated apply endpoint
takes the user-confirmed rename plan and runs it through a two-pass
rename so swapped names don't collide. Track tags follow the
rename via cache.rename_track_filename.
"""
from __future__ import annotations

import logging
import os
import re
from pathlib import Path
from typing import Any, Iterable

from mutagen.flac import FLAC

from .. import album_fields
from .. import db as db_module
from .. import filename_template as ft
from .. import settings as settings_module
from .base import Check, CheckMeta, CheckScope, Finding, FixResult

log = logging.getLogger(__name__)


class FilenameMismatch(Check):
    meta = CheckMeta(
        id="filename-mismatch",
        label="Filename doesn't match template",
        description=(
            "Albums where one or more FLAC filenames don't match your "
            "configured filename template (Settings → Filename templates). "
            "Filenames drift over time: different rippers use different "
            "conventions, metadata edits happen after the rip, you change "
            "your preferred format. The Resolve dialog shows current → "
            "expected for every track and renames in one safe two-pass "
            "operation."
        ),
        category="files",
        cost="medium",
        # Reading every FLAC's tags is too expensive to run on every
        # scan — gate behind explicit "Run now" / on-demand only.
        runs_during_scan=False,
        # Deterministic: the rename target comes from the template,
        # no AI judgment. The Resolve dialog is custom (mirrors
        # duplicate-tracks / inconsistent-artist-sort), so fix() is
        # a stub.
        fix_kind="deterministic",
        bulk_action_label=None,
    )

    def run(self, scope: CheckScope) -> Iterable[Finding]:
        single_template = settings_module.filename_template_single_artist()
        comp_template = settings_module.filename_template_compilation()
        single_dir_template = settings_module.dir_template_single_artist()
        comp_dir_template = settings_module.dir_template_compilation()
        # Scope-limited fetch when the runner asked for a subset (per-
        # album rerun after a fix). Without this the check would still
        # read FLAC tags from every album in the library — minutes of
        # disk I/O on SMB — even though we only care about one album.
        with db_module.connect() as conn:
            if scope.kind == "albums":
                ids = [a["id"] for a in scope.albums if a.get("id")]
                if not ids:
                    return
                placeholders = ",".join("?" * len(ids))
                rows = conn.execute(
                    f"""
                    SELECT id, path, dir_name, album_artist, artist,
                           dir_artist, title, year,
                           is_compilation, disc_number, disc_total
                    FROM albums
                    WHERE library_path = ? AND id IN ({placeholders})
                      AND missing_since IS NULL
                    """,
                    (scope.library_path, *ids),
                ).fetchall()
            else:
                rows = conn.execute(
                    """
                    SELECT id, path, dir_name, album_artist, artist,
                           dir_artist, title, year,
                           is_compilation, disc_number, disc_total
                    FROM albums
                    WHERE library_path = ?
                      AND missing_since IS NULL
                    """,
                    (scope.library_path,),
                ).fetchall()

        for row in rows:
            album_path = row["path"]
            if not album_path or not Path(album_path).is_dir():
                continue
            template = comp_template if row["is_compilation"] else single_template
            dir_template = comp_dir_template if row["is_compilation"] else single_dir_template
            try:
                renames = _check_album(album_path, template, row)
                dir_rename = _check_dir(album_path, dir_template, row)
            except Exception:
                log.exception("filename-mismatch: error scanning album %s", album_path)
                continue
            if not renames and not dir_rename:
                continue
            yield Finding(
                album_id=row["id"],
                summary=_summarize(row["dir_name"], renames, dir_rename),
                severity="info",
                detail={
                    "dir_name":       row["dir_name"],
                    "is_compilation": bool(row["is_compilation"]),
                    "template":       template,
                    "dir_template":   dir_template,
                    "headline":       row["dir_name"] or row["id"],
                    "renames":        renames,
                    "dir_rename":     dir_rename,
                },
            )

    def fix(self, finding: Finding, *, user_hint: str = "") -> FixResult:
        """Stub. Resolution goes through the dedicated apply endpoint
        (POST /api/checks/filename-mismatch/findings/{id}/apply) — the
        user has to confirm the rename plan in the Resolve dialog
        before any file moves on disk. Implemented (rather than
        NotImplementedError) so accidental calls return a clean
        message."""
        return FixResult(
            ok=False,
            message=(
                "Filename-mismatch findings are resolved via the "
                "Resolve… dialog; this check has no single-click fix "
                "because the user has to review the rename plan first."
            ),
        )


def _check_album(
    album_path: str,
    template: str,
    row: Any,
) -> list[dict[str, Any]]:
    """Read the FLACs in `album_path`, render the expected filename
    for each, and return a list of rename plan entries for files
    whose actual filename doesn't match. Empty list = album is
    already conformant."""
    from ..scanner import dedupe_by_inode
    p = Path(album_path)
    flacs = sorted(
        e for e in p.iterdir()
        if e.is_file() and e.suffix.lower() == ".flac"
        and not e.name.startswith("._")
    )
    # SMB mounts (e.g. Synology DSM) can yield the same physical file
    # under two directory entries when the filename contains characters
    # that need encoding translation (`?`, `*`, `:`). Without inode-
    # dedupe the rename plan ends up with the same source listed twice,
    # which then surfaces as visible duplicate rows in the Resolve
    # dialog. Other scan paths already dedupe via scanner.dedupe_by_inode;
    # mirror that here.
    flacs = dedupe_by_inode(flacs)
    if not flacs:
        return []

    renames: list[dict[str, Any]] = []
    # Track collisions WITHIN the rendered set — if two tracks render
    # to the same expected filename, the rename can't proceed cleanly.
    # We still report them but mark them so the apply path can refuse.
    seen_targets: dict[str, str] = {}

    for flac in flacs:
        try:
            audio = FLAC(str(flac))
            tags = audio.tags or {}
        except Exception as e:
            log.warning("filename-mismatch: couldn't open %s: %s", flac, e)
            continue
        tracknumber = _first_tag(tags, "tracknumber")
        title = _first_tag(tags, "title")
        artist = _first_tag(tags, "artist")
        albumartist = _first_tag(tags, "albumartist")
        # Don't propose a rename when we can't even render: missing
        # title is the most common "incomplete tag" case and
        # rendering would yield "05 - " which the user doesn't want
        # us to write to disk. The missing-core-tags check handles
        # that pathology separately.
        if not title or not tracknumber:
            continue

        rendered = ft.render(template, {
            "tracknumber": tracknumber,
            "title":       title,
            "artist":      artist or albumartist or "",
            "albumartist": albumartist or row["album_artist"] or "",
            # The album-row's `title` column holds the album NAME,
            # not the track title — the template variable {album}
            # maps to it.
            "album":       row["title"] or "",
            "year":        row["year"] or "",
            "disc":        row["disc_number"],
        })
        if not rendered:
            continue
        expected = f"{rendered}.flac"
        # Already-conformant case: skip.
        if expected == flac.name:
            continue

        collision_with = seen_targets.get(expected)
        seen_targets[expected] = flac.name

        renames.append({
            "filename":    flac.name,
            "expected":    expected,
            "tracknumber": tracknumber,
            "title":       title,
            "artist":      artist or albumartist or "",
            "collides_with": collision_with,
        })

    return renames


def _first_tag(tags: Any, key: str) -> str:
    """Robust first-tag-value getter. Vorbis comments are case-
    insensitive but mutagen normalizes them; .get() returns None
    or a list of strings."""
    if tags is None:
        return ""
    val = tags.get(key) or tags.get(key.lower()) or tags.get(key.upper())
    if not val:
        return ""
    if isinstance(val, list):
        return str(val[0]) if val else ""
    return str(val)


def _summarize(
    dir_name: str | None,
    renames: list[dict[str, Any]],
    dir_rename: dict[str, str] | None,
) -> str:
    label = dir_name or "(unknown album)"
    if dir_rename and renames:
        n = len(renames)
        return (
            f"Album dir + {n} file{'s' if n != 1 else ''} in {label!r} "
            "don't match the templates"
        )
    if dir_rename:
        return f"Album dir {label!r} doesn't match the template"
    n = len(renames)
    return f"{n} file{'s' if n != 1 else ''} in {label!r} don't match the template"


def _check_dir(
    album_path: str,
    template: str,
    row: Any,
) -> dict[str, str] | None:
    """Compare the album's actual directory basename to the rendered
    template. Returns {"old": current_basename, "new": expected_basename}
    when they differ, None when they already match (or when we can't
    render — e.g. missing album title).

    Multi-disc note: when disc_total > 1, we append "(disc N of M)"
    to the rendered base. This matches the flac-jacket convention
    and avoids requiring users to embed the disc segment in their
    template — it's an automatic structural concern, not a
    user-customizable one. The parser in scanner._DIR_PATTERN
    already understands this exact suffix shape.
    """
    p = Path(album_path)
    current = p.name
    # Sanity: missing album title would render an empty / nonsense
    # name. Skip rather than propose something destructive.
    title = row["title"] or ""
    if not title.strip():
        return None
    # Use the centralized cascade (album_artist → artist → dir_artist).
    # `include_dir_artist=True` is appropriate here because we're
    # rendering a path: an empty {albumartist} token would silently
    # drop the artist from the proposed directory name (the Mancini
    # bug). See `album_fields.display_artist` for the rationale.
    aa = album_fields.display_artist(
        {k: row[k] for k in ("album_artist", "artist", "dir_artist")},
        include_dir_artist=True,
    )
    rendered = ft.render(template, {
        "title":       title,
        "album":       title,                    # alias: many users
        "artist":      aa,
        "albumartist": aa,
        "year":        row["year"] or "",
    })
    if not rendered:
        return None
    # Multi-disc segment. Insert before the trailing "[FLAC]" if the
    # template has it, otherwise append. Keeps the trailing suffix
    # at the end of the basename either way.
    disc_n = row["disc_number"]
    disc_total = row["disc_total"]
    if disc_total and disc_total > 1 and disc_n:
        suffix = f"(disc {disc_n} of {disc_total})"
        # Insert the suffix before the last [FLAC]-shaped tag if the
        # template ended with one, so the result reads naturally.
        m = re.search(r"\s+\[\w+\]\s*$", rendered)
        if m:
            rendered = rendered[: m.start()] + " " + suffix + rendered[m.start():]
        else:
            rendered = rendered + " " + suffix
        # Re-sanitize (a defensive belt-and-suspenders pass — the
        # variables don't carry path-illegal chars in practice but
        # the user's template could).
        rendered = ft.sanitize_for_filename(rendered)
    if rendered == current:
        return None
    return {"old": current, "new": rendered}


def derive_plan(album_id: str) -> dict[str, Any] | None:
    """Re-derive the filename-mismatch plan for a single album against
    the current state of disk + DB + settings. Returns the same shape
    that `run()` writes into `Finding.detail`, or None when the album
    is missing.

    This is the read-side counterpart to `execute_renames`: a Resolve
    dialog calls it on open so the rename plan it shows reflects right-
    now reality, not whatever the check happened to capture at
    finding-creation time. Without this, a finding that was created
    before the user already half-fixed the album by hand (or before a
    sibling Resolve click already ran) shows stale rows that either
    fail to apply or re-do work that was already done.

    Empty plan (no `renames` AND no `dir_rename`) means the album is
    already conformant — callers should clear the stale finding rather
    than render an empty dialog. See the `/plan-preview` API endpoint
    for the wire shape.
    """
    single_template = settings_module.filename_template_single_artist()
    comp_template = settings_module.filename_template_compilation()
    single_dir_template = settings_module.dir_template_single_artist()
    comp_dir_template = settings_module.dir_template_compilation()

    with db_module.connect() as conn:
        row = conn.execute(
            """
            SELECT id, path, dir_name, album_artist, artist,
                   dir_artist, title, year,
                   is_compilation, disc_number, disc_total
            FROM albums
            WHERE id = ?
              AND missing_since IS NULL
            """,
            (album_id,),
        ).fetchone()
    if row is None:
        # Either the album was never cached, or it's currently Missing
        # — in both cases the plan-preview endpoint's None branch
        # surfaces this as a stale finding the dialog should clear.
        return None

    album_path = row["path"]
    if not album_path or not Path(album_path).is_dir():
        return {
            "dir_name":       row["dir_name"],
            "is_compilation": bool(row["is_compilation"]),
            "template":       "",
            "dir_template":   "",
            "headline":       row["dir_name"] or album_id,
            "renames":        [],
            "dir_rename":     None,
            "album_missing":  True,
        }

    template = comp_template if row["is_compilation"] else single_template
    dir_template = comp_dir_template if row["is_compilation"] else single_dir_template
    try:
        renames = _check_album(album_path, template, row)
        dir_rename = _check_dir(album_path, dir_template, row)
    except Exception:
        log.exception("filename-mismatch: derive_plan error for %s", album_path)
        return {
            "dir_name":       row["dir_name"],
            "is_compilation": bool(row["is_compilation"]),
            "template":       template,
            "dir_template":   dir_template,
            "headline":       row["dir_name"] or album_id,
            "renames":        [],
            "dir_rename":     None,
            "error":          "Couldn't re-derive plan; see server log",
        }

    return {
        "dir_name":       row["dir_name"],
        "is_compilation": bool(row["is_compilation"]),
        "template":       template,
        "dir_template":   dir_template,
        "headline":       row["dir_name"] or album_id,
        "renames":        renames,
        "dir_rename":     dir_rename,
    }


def execute_renames(
    *,
    album_id: str,
    plan: list[dict[str, str]],
    dir_rename: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Apply a rename plan to the album. Two-pass (tmp → final) so
    swap collisions don't blow up. Migrates track_tags via
    cache.rename_track_filename. Returns a summary dict.

    `plan` shape: list of {"old": filename, "new": filename}. Caller
    is responsible for filtering out collisions / no-ops first; we
    do a defensive check here too.

    `dir_rename` shape (optional): {"old": current_basename,
    "new": expected_basename}. When present, the album's directory
    is renamed FIRST (with FK migrations across all tables that
    reference album_id, since the album's id is sha1 of its path).
    The file renames then run inside the new directory. Failure of
    the dir rename short-circuits the whole apply — we never want
    to rename files in a partially-migrated album.
    """
    from .. import cache as cache_module
    from ..config import load_config
    cfg = load_config()
    if not cfg.library_dir:
        return {"ok": False, "message": "No library configured", "renamed": 0}
    with db_module.connect() as conn:
        row = conn.execute(
            "SELECT path FROM albums WHERE id = ?", (album_id,),
        ).fetchone()
    if row is None:
        return {"ok": False, "message": "Album not found", "renamed": 0}
    album_dir = Path(row["path"])
    if not album_dir.is_dir():
        return {"ok": False, "message": f"Album directory missing: {album_dir}", "renamed": 0}

    # Dir rename FIRST (when requested). Subsequent file renames
    # operate inside the new dir.
    new_album_id = album_id
    dir_renamed = False
    dir_migration_summary: dict[str, int] = {}
    if dir_rename:
        new_dir_basename = (dir_rename.get("new") or "").strip()
        if not new_dir_basename:
            return {"ok": False, "message": "dir_rename.new is empty", "renamed": 0}
        if "/" in new_dir_basename or "\\" in new_dir_basename:
            return {
                "ok": False,
                "message": f"Refusing dir rename target with path separator: {new_dir_basename!r}",
                "renamed": 0,
            }
        new_dir_path = album_dir.parent / new_dir_basename
        # Same-physical-dir check (handles case-insensitive APFS, NFC vs
        # NFD unicode normalization, etc.). Path string equality alone
        # would refuse perfectly safe renames where the source and
        # target are the same directory under a different surface form
        # — e.g. an album with `é` (NFD on disk) being renamed to `é`
        # (NFC from the template render).
        same_physical = False
        if new_dir_path.exists():
            try:
                same_physical = os.path.samefile(str(new_dir_path), str(album_dir))
            except OSError:
                same_physical = False
        if new_dir_path.exists() and not same_physical:
            return {
                "ok": False,
                "message": (
                    f"Refusing to overwrite existing directory: "
                    f"{new_dir_path.name}"
                ),
                "renamed": 0,
            }
        try:
            # Rename on disk. shutil.move handles cross-device but
            # within a library dir we expect same-fs and a plain
            # rename suffices. Even when source and target point at the
            # same physical dir (case / unicode-form difference), we
            # still issue the rename — `os.rename(x, y)` will normalize
            # the on-disk surface form on case-insensitive APFS, which
            # is the user-visible result they're after.
            album_dir.rename(new_dir_path)
            dir_renamed = True
            # Compute the new id (sha1 of resolved new path) and
            # update the cache + every album_id reference atomically.
            from ..scanner import _make_id
            new_album_id = _make_id(str(new_dir_path.resolve()))
            try:
                dir_migration_summary = cache_module.rename_album_directory(
                    old_id=album_id,
                    new_id=new_album_id,
                    new_path=str(new_dir_path),
                    new_dir_name=new_dir_path.name,
                )
            except Exception:
                # Cache migration failed — roll back the dir rename
                # so we don't leave a moved-but-uncached album.
                log.exception("execute_renames: cache migration failed; rolling back dir rename")
                try:
                    new_dir_path.rename(album_dir)
                except OSError:
                    log.exception("execute_renames: rollback of dir rename also failed")
                return {
                    "ok": False,
                    "message": "Cache migration failed during dir rename; rolled back",
                    "renamed": 0,
                }
            album_dir = new_dir_path  # subsequent file ops use new path
            # Phase 1 of move E (issue #1): publish AlbumRenamed so
            # subscribers (resolver invalidate for both ids, thumb
            # drop for old id, scoped post-scan rerun against new id)
            # fire. The /apply endpoint also runs a scoped check
            # rerun directly — keep both during Phase 1.
            try:
                from .. import library_events
                library_events.publish(library_events.AlbumRenamed(
                    old_album_id=album_id, new_album_id=new_album_id,
                ))
            except Exception:
                log.exception(
                    "library_events publish failed for rename %s → %s",
                    album_id, new_album_id,
                )
        except OSError as e:
            return {
                "ok": False,
                "message": f"Couldn't rename directory: {e}",
                "renamed": 0,
            }

    # Filter the plan: drop no-ops, drop entries with missing names,
    # drop entries where the source doesn't actually exist on disk.
    # Track "already done" entries (source missing AND target present)
    # separately so we can give the user a clearer message than the
    # generic "Nothing to rename" — typically means a previous Resolve
    # click already moved the files and the finding's stale plan was
    # re-submitted.
    cleaned: list[tuple[Path, Path]] = []
    already_done = 0
    for entry in plan:
        old = (entry.get("old") or "").strip()
        new = (entry.get("new") or "").strip()
        if not old or not new or old == new:
            continue
        if "/" in new or "\\" in new:
            return {
                "ok": False,
                "message": f"Refusing rename target with path separator: {new!r}",
                "renamed": 0,
            }
        old_p = album_dir / old
        new_p = album_dir / new
        if not old_p.is_file():
            if new_p.is_file():
                # Looks like this entry already ran successfully on a
                # previous click — log at info, not warning.
                log.info("execute_renames: already renamed: %s", new_p.name)
                already_done += 1
            else:
                log.warning("execute_renames: source missing, skipping: %s", old_p)
            continue
        cleaned.append((old_p, new_p))

    if not cleaned and not dir_renamed:
        # Always publish AlbumRenamed(old==new) on a no-op apply so the
        # _filename_mismatch_rerun subscriber re-evaluates against the
        # current filesystem and the now-stale finding clears. Without
        # this, the dialog's deleted explicit FINDINGS invalidation
        # leaves the resolved finding visible until the user navigates.
        try:
            from .. import library_events
            library_events.publish(library_events.AlbumRenamed(
                old_album_id=new_album_id, new_album_id=new_album_id,
            ))
        except Exception:
            log.exception(
                "library_events publish failed for no-op rename %s",
                new_album_id,
            )
        if already_done and already_done == sum(
            1 for e in plan
            if (e.get("old") or "").strip()
            and (e.get("new") or "").strip()
            and (e.get("old") or "").strip() != (e.get("new") or "").strip()
        ):
            return {
                "ok":      True,
                "renamed": 0,
                # `already_renamed` is the signal the API layer uses to
                # trigger a check rerun so the now-stale finding clears.
                "already_renamed": True,
                "message": (
                    f"All {already_done} file(s) were already renamed by an "
                    f"earlier click; nothing to do. Re-running the check to "
                    f"clear the stale finding."
                ),
            }
        return {"ok": True, "message": "Nothing to rename", "renamed": 0}
    if not cleaned and dir_renamed:
        # Dir-only rename — refresh the cache and we're done.
        try:
            from ..scanner import album_to_dict, scan_one_album
            fresh = scan_one_album(str(album_dir))
            if fresh is not None:
                cache_module.upsert_album(cfg.library_dir, album_to_dict(fresh))
        except Exception:
            log.exception("execute_renames: post-dir-rename rescan failed (continuing)")
        return {
            "ok":             True,
            "renamed":        0,
            "errors":         [],
            "dir_renamed":    True,
            "new_album_id":   new_album_id,
            "dir_migration":  dir_migration_summary,
            "message":        "Renamed album directory",
        }

    # Reject collisions: if any new path already exists AND isn't one
    # of our own sources (i.e. an unrelated file in the dir), refuse.
    sources = {str(o) for o, _ in cleaned}
    for _, new_p in cleaned:
        if new_p.exists() and str(new_p) not in sources:
            return {
                "ok": False,
                "message": (
                    f"Refusing to overwrite an existing unrelated file: "
                    f"{new_p.name}"
                ),
                "renamed": 0,
            }

    # Two-pass rename: stage to .tmp names, then move to finals. Lets
    # us swap files (track 5 ↔ track 7 of an out-of-order rip)
    # without colliding mid-flight.
    staged: list[tuple[Path, Path, Path]] = []
    renamed = 0
    errors: list[str] = []
    try:
        for old_p, new_p in cleaned:
            tmp_p = old_p.with_name(f".{old_p.name}.renaming.tmp")
            old_p.rename(tmp_p)
            staged.append((old_p, tmp_p, new_p))
        for old_p, tmp_p, new_p in staged:
            try:
                tmp_p.rename(new_p)
                renamed += 1
                # Migrate track tags so per-track user tags follow the file.
                # Use new_album_id (post-dir-rename id, if applicable).
                try:
                    cache_module.rename_track_filename(
                        new_album_id, old_p.name, new_p.name,
                    )
                except Exception:
                    log.exception(
                        "execute_renames: track-tag migration failed for "
                        "%s → %s (continuing)", old_p.name, new_p.name,
                    )
            except OSError as e:
                errors.append(f"{old_p.name} → {new_p.name}: {e}")
                # Try to roll back: tmp → old
                try:
                    tmp_p.rename(old_p)
                except OSError:
                    log.exception(
                        "execute_renames: rollback failed for %s", old_p,
                    )
    except OSError as e:
        log.exception("execute_renames: stage phase failed; rolling back")
        # Undo any tmp moves.
        for old_p, tmp_p, _ in staged:
            if tmp_p.exists() and not old_p.exists():
                try:
                    tmp_p.rename(old_p)
                except OSError:
                    pass
        return {
            "ok": False,
            "message": f"Rename staging failed: {e}",
            "renamed": 0,
        }

    # Re-scan the album so the cache reflects new filenames.
    try:
        from ..scanner import album_to_dict, scan_one_album
        fresh = scan_one_album(str(album_dir))
        if fresh is not None:
            cache_module.upsert_album(cfg.library_dir, album_to_dict(fresh))
    except Exception:
        log.exception("execute_renames: post-rename rescan failed (continuing)")

    # Publish AlbumRenamed for the file-only path too (the dir-rename
    # branch above already published one with a NEW id). Setting
    # old_album_id == new_album_id signals "files were renamed but
    # the album identity didn't change." The _filename_mismatch_
    # rerun subscriber listens on this event and re-evaluates the
    # check against the album's current filesystem state.
    if renamed > 0 and not dir_renamed:
        try:
            from .. import library_events
            library_events.publish(library_events.AlbumRenamed(
                old_album_id=new_album_id, new_album_id=new_album_id,
            ))
        except Exception:
            log.exception(
                "library_events publish failed for file rename on %s",
                new_album_id,
            )

    return {
        "ok":          (renamed > 0 or dir_renamed) and not errors,
        "renamed":     renamed,
        "errors":      errors,
        "dir_renamed": dir_renamed,
        "new_album_id": new_album_id,
        "dir_migration": dir_migration_summary,
        "message":  (
            (("Renamed album directory + " if dir_renamed else "") if renamed else "Renamed album directory")
            + (f"{renamed} file{'s' if renamed != 1 else ''}" if renamed else "")
            + (f"; {len(errors)} error(s)" if errors else "")
        ).strip(),
    }
