"""Scan a directory tree for music albums.

An "album" is any directory that directly contains one or more FLAC files.
Multi-disc sets organized as `Album/Disc 1/*.flac` and `Album/Disc 2/*.flac`
are reported as two distinct discs; merging them into a single logical album
is left to a later pass.

Album metadata comes from two sources:

  1. The directory name, parsed against flac-jacket's convention:
        Artist - Album [FLAC]
        Artist - Album (disc N of M) [FLAC]
        Compilations - Album [FLAC]      (compilation marker)
        Various Artists - Album [FLAC]   (compilation marker)
     Parsed values land in `dir_artist` / `dir_album` / `is_compilation` /
     `disc_number` / `disc_total`.

  2. The first FLAC's Vorbis comments, used to populate `title` / `artist` /
     etc. These can be wrong (the whole point of metadata cleanup), so the
     directory parse is treated as the more authoritative anchor.

Cover art preference order:
  1. cover.jpg / folder.jpg / front.jpg / cover.png in the album directory
  2. Embedded picture in the first FLAC
"""
from __future__ import annotations

import hashlib
import os
import re
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Iterator

from mutagen.flac import FLAC, Picture

AUDIO_EXTS = {".flac"}
COVER_FILENAMES = ("cover.jpg", "cover.jpeg", "cover.png", "folder.jpg", "folder.jpeg", "folder.png", "front.jpg", "front.jpeg", "front.png")

# ---------- Directory-name parser (lifted from flac-jacket) ----------

# Strings that, when in the artist position, mark the album as a compilation
# rather than a single-artist release. flac-jacket uses {"compilations",
# "various artists"}; the singular "compilation" is added because Paul's
# library has examples of that variant too.
_COLLECTION_NAMES = {"compilation", "compilations", "various artists", "various"}

# Patterns the regex below handles (case-insensitive, [FLAC] suffix expected
# but tolerated as bare "FLAC"):
#   "Artist - Album [FLAC]"
#   "Artist - Album (disc 1 of 3) [FLAC]"
#   "Artist - Album (1 of 3) [FLAC]"
#   "Artist - Album Disc 1 [FLAC]"
#   "Artist - Album - Disc 1 [FLAC]"
#   "Compilations - Album [FLAC]"
#   "Various Artists - Album [FLAC]"
_DIR_PATTERN = re.compile(
    r"^(?P<artist>.+?)\s*-\s*(?P<album>.+?)"
    r"(?:"
    r"\s+\(?disc\s+(?P<disc>\d+)(?:\s+of\s+(?P<disc_total>\d+))?\)?"  # (disc N of M), disc N of M, (disc N)
    r"|\s+\((?P<disc2>\d+)\s+of\s+(?P<disc_total2>\d+)\)"             # (N of M)
    r")?"
    r"\s+\[?FLAC\]?\s*$",
    re.IGNORECASE,
)


@dataclass
class DirectoryParse:
    """Result of parsing an album directory name. All fields may be None /
    False if the convention didn't match (e.g. legacy folder name)."""
    raw: str = ""
    matched: bool = False
    dir_artist: str | None = None      # None when is_compilation=True
    dir_album: str | None = None
    is_compilation: bool = False
    disc_number: int | None = None
    disc_total: int | None = None


def parse_album_dir(dirname: str) -> DirectoryParse:
    """Parse a directory name like `Artist - Album [FLAC]`. Returns a
    DirectoryParse with `matched=False` if the convention didn't apply."""
    parsed = DirectoryParse(raw=dirname)
    m = _DIR_PATTERN.match(dirname)
    if not m:
        return parsed
    parsed.matched = True
    artist_part = m.group("artist").strip()
    parsed.dir_album = m.group("album").strip()
    if artist_part.lower() in _COLLECTION_NAMES:
        parsed.is_compilation = True
        parsed.dir_artist = None
    else:
        parsed.dir_artist = artist_part
    disc = m.group("disc") or m.group("disc2")
    disc_total = m.group("disc_total") or m.group("disc_total2")
    if disc:
        parsed.disc_number = int(disc)
    if disc_total:
        parsed.disc_total = int(disc_total)
    return parsed


@dataclass
class Album:
    id: str                          # stable hash of the album path
    path: str                        # absolute path to the album directory
    dir_name: str                    # basename of the album directory
    title: str | None
    artist: str | None
    album_artist: str | None
    year: str | None
    genre: str | None
    track_count: int                 # number of FLAC files found
    total_seconds: float             # sum of track durations
    has_embedded_cover: bool
    cover_file: str | None           # filename of cover art on disk, if any
    sample_track: str                # path of the FLAC we read tags from
    dir_mtime: float = 0.0           # directory mtime when scanned (for incremental rescans)
    # Parsed from the directory name (flac-jacket convention). Treated as the
    # most authoritative anchor for album identity — see the module docstring.
    dir_artist: str | None = None
    dir_album: str | None = None
    is_compilation: bool = False
    disc_number: int | None = None
    disc_total: int | None = None
    # Audio format from FLAC StreamInfo. Single value at album level on the
    # assumption (true for ripped CDs / LPs) that all tracks share the same
    # format. Per-track sample rate isn't surfaced — if mixed formats appear
    # in a single album we'd need to revisit.
    sample_rate: int | None = None       # Hz
    bits_per_sample: int | None = None   # e.g. 16, 24
    channels: int | None = None          # e.g. 1 (mono), 2 (stereo)
    # Per-track tracknumber values, in album-file order. Used by the
    # `track-gaps` health check to detect missing or duplicate numbers
    # without re-reading every FLAC. None entries mean the track had no
    # readable tracknumber tag. JSON-serialized for storage; a Python list
    # while in the dataclass.
    track_numbers: list[int | None] = field(default_factory=list)
    # v8: Vorbis sort variants. Read directly from the FLAC's
    # ALBUMARTISTSORT / ARTISTSORT / ALBUMSORT tags. None when the
    # source files don't carry sort tags — most non-Picard libraries
    # won't have them. Used by the missing-sort-tags check and surfaced
    # to Plex via apply-path tag writes.
    album_artist_sort: str | None = None
    artist_sort: str | None = None
    album_sort: str | None = None
    # v11: Per-track artist + artistsort credits, denormalized so the
    # inconsistent-artist-sort check can cluster across album AND track
    # levels without re-reading every FLAC. Each entry: {filename,
    # artist, artist_sort}. Empty string fields render as missing in
    # the check (rather than None) so JSON round-trips stay simple.
    track_artists: list[dict[str, str]] = field(default_factory=list)


def _make_id(path: str) -> str:
    return hashlib.sha1(path.encode("utf-8")).hexdigest()[:16]


def _parse_tracknum(raw: str | None) -> int | None:
    """Coerce a tracknumber tag value into an int, or None if unparseable.

    Handles the common variants we see in the wild: plain "5", zero-padded
    "05", "5/12" (track 5 of 12), and disc-prefixed forms like "1-05",
    "1.05", "01.05" (multi-disc collapsed to a single linear sequence per
    the user's library convention — we want the LAST integer in those).
    None / empty / non-numeric → None.
    """
    if raw is None:
        return None
    s = str(raw).strip()
    if not s:
        return None
    # Strip "/total" suffix.
    if "/" in s:
        s = s.split("/", 1)[0].strip()
    # Disc-prefixed: take the last numeric chunk after a separator.
    for sep in ("-", "."):
        if sep in s:
            tail = s.rsplit(sep, 1)[1].strip()
            if tail.isdigit():
                s = tail
                break
    try:
        return int(s)
    except (TypeError, ValueError):
        return None


def _first_tag(tags, *keys: str) -> str | None:
    """Return the first non-empty value for any of the given Vorbis tag keys."""
    if not tags:
        return None
    lowered = {k.lower(): v for k, v in tags.as_dict().items()}
    for key in keys:
        values = lowered.get(key.lower())
        if values:
            value = values[0] if isinstance(values, list) else values
            if value:
                return str(value).strip()
    return None


def _find_cover_file(album_dir: Path) -> str | None:
    for name in COVER_FILENAMES:
        candidate = album_dir / name
        if candidate.is_file():
            return name
    # case-insensitive fallback
    try:
        existing = {entry.name.lower(): entry.name for entry in album_dir.iterdir() if entry.is_file()}
    except OSError:
        return None
    for name in COVER_FILENAMES:
        if name in existing:
            return existing[name]
    return None


def _list_flacs(album_dir: Path) -> list[Path]:
    try:
        entries = sorted(album_dir.iterdir())
    except OSError:
        return []
    return [
        e for e in entries
        if e.is_file()
        and e.suffix.lower() in AUDIO_EXTS
        and not e.name.startswith("._")
    ]


def compute_effective_mtime(album_dir: Path, flacs: list[Path] | None = None) -> float:
    """The 'effective' mtime of an album: max of the directory's mtime and
    all its FLAC file mtimes.

    Modifying a file in-place (which mutagen does on tag writes) bumps the
    file's mtime but not the parent directory's mtime — only dirent changes
    do that. Without including file mtimes, tag edits would never invalidate
    the scan cache. We use this single function in both `_read_album()` (for
    the value we store) and `incremental_scan()` (for the comparison value),
    so they're guaranteed to agree.
    """
    try:
        mtime = album_dir.stat().st_mtime
    except OSError:
        mtime = 0.0
    if flacs is None:
        flacs = _list_flacs(album_dir)
    for f in flacs:
        try:
            mtime = max(mtime, f.stat().st_mtime)
        except OSError:
            pass
    return mtime


def _read_album(album_dir: Path, flacs: list[Path]) -> Album | None:
    sample = flacs[0]
    try:
        audio = FLAC(str(sample))
    except Exception:
        return None

    tags = audio.tags
    title = _first_tag(tags, "album")
    artist = _first_tag(tags, "albumartist", "album_artist", "artist")
    album_artist = _first_tag(tags, "albumartist", "album_artist")
    year = _first_tag(tags, "date", "year", "originaldate", "originalyear")
    if year and len(year) >= 4:
        # normalize "1973-09-15" → "1973"
        head = year[:4]
        if head.isdigit():
            year = head
    # v8: Vorbis sort variants. Picard / beets / similar populate these;
    # ad-hoc-tagged libraries usually don't. None when absent — the
    # missing-sort-tags check uses None vs. populated to decide whether
    # to flag the album.
    album_artist_sort = _first_tag(tags, "albumartistsort")
    artist_sort = _first_tag(tags, "artistsort")
    album_sort = _first_tag(tags, "albumsort")

    # Genre is read in the per-file loop below since FLAC supports
    # multiple GENRE entries per file AND tracks within an album can
    # legitimately differ (compilations especially). We union across
    # all files with case-insensitive dedup but case-preserving order.
    # The `_genre_seen` lowercase set is just for dedup; `_genre_list`
    # preserves the first-seen casing the user picked.
    _genre_seen: set[str] = set()
    _genre_list: list[str] = []

    total_seconds = 0.0
    track_numbers: list[int | None] = []
    # v11: Per-track artist credits. We capture in the same loop so
    # the inconsistent-artist-sort check can find sort-tag drift at
    # the per-track level (compilations especially).
    track_artists: list[dict[str, str]] = []
    for flac_path in flacs:
        try:
            audio_t = FLAC(str(flac_path))
            if audio_t.info:
                total_seconds += float(audio_t.info.length)
            track_numbers.append(_parse_tracknum(_first_tag(audio_t.tags, "tracknumber")))
            # Pull every GENRE entry from this file. mutagen returns
            # them as a list when multiple are present (Picard / beets
            # convention); single-value files return a 1-element list.
            for raw_genre in (audio_t.tags or {}).get("genre", []):
                g = (raw_genre or "").strip()
                if not g:
                    continue
                key = g.lower()
                if key in _genre_seen:
                    continue
                _genre_seen.add(key)
                _genre_list.append(g)
            # Per-track artist credit. Empty string when missing so
            # downstream JSON serialization stays simple. We only
            # store an entry when at least one of artist / artistsort
            # is non-empty — pure-instrumental albums with no per-
            # track artist tags shouldn't pollute the dataset.
            t_artist = _first_tag(audio_t.tags, "artist") or ""
            t_artist_sort = _first_tag(audio_t.tags, "artistsort") or ""
            if t_artist or t_artist_sort:
                track_artists.append({
                    "filename":    flac_path.name,
                    "artist":      t_artist,
                    "artist_sort": t_artist_sort,
                })
        except Exception:
            track_numbers.append(None)
            continue

    # Album-level genre is the union joined with "; " — the same delimiter
    # the frontend chip parser splits on, so the chips just work. None
    # when no track in the album had any genre tag.
    genre = "; ".join(_genre_list) if _genre_list else None

    dir_mtime = compute_effective_mtime(album_dir, flacs)
    parsed = parse_album_dir(album_dir.name)

    # FLAC StreamInfo from the sample track — assumes all tracks in an album
    # share the same audio format (true for ripped CDs / LPs).
    sample_rate = audio.info.sample_rate if audio.info else None
    bits_per_sample = getattr(audio.info, "bits_per_sample", None) if audio.info else None
    channels = audio.info.channels if audio.info else None

    # Prefer the directory-parsed title when the convention matched, since
    # tags can be stale or wrong. Fall back to the tag value, then dir name.
    display_title = parsed.dir_album or title or album_dir.name

    return Album(
        id=_make_id(str(album_dir.resolve())),
        path=str(album_dir.resolve()),
        dir_name=album_dir.name,
        title=display_title,
        artist=artist,
        album_artist=album_artist,
        year=year,
        genre=genre,
        track_count=len(flacs),
        total_seconds=round(total_seconds, 1),
        has_embedded_cover=len(audio.pictures) > 0,
        cover_file=_find_cover_file(album_dir),
        sample_track=str(sample.resolve()),
        dir_mtime=dir_mtime,
        dir_artist=parsed.dir_artist,
        dir_album=parsed.dir_album,
        is_compilation=parsed.is_compilation,
        disc_number=parsed.disc_number,
        disc_total=parsed.disc_total,
        sample_rate=sample_rate,
        bits_per_sample=bits_per_sample,
        channels=channels,
        track_numbers=track_numbers,
        album_artist_sort=album_artist_sort,
        artist_sort=artist_sort,
        album_sort=album_sort,
        track_artists=track_artists,
    )


def scan_albums(root: str | os.PathLike) -> Iterator[Album]:
    """Yield an Album for every directory under `root` that contains FLACs."""
    root_path = Path(root).expanduser()
    if not root_path.exists():
        return
    for dirpath, dirnames, filenames in os.walk(root_path):
        dirnames[:] = [d for d in dirnames if not d.startswith(".")]
        album_dir = Path(dirpath)
        flacs = _list_flacs(album_dir)
        if not flacs:
            continue
        album = _read_album(album_dir, flacs)
        if album is not None:
            yield album


def scan_albums_list(root: str | os.PathLike) -> list[Album]:
    albums = list(scan_albums(root))
    # Sort by Vorbis sort variant when present (so "The Beatles" sorts
    # under "B" and Ray Conniff variants cluster together), falling
    # back through album_artist / artist when no sort tag is set.
    # Same precedence the frontend uses; keeps API response order
    # aligned with what the user sees in the sidebar.
    albums.sort(key=lambda a: (
        (a.album_artist_sort or a.artist_sort or a.album_artist or a.artist or "").lower(),
        (a.year or ""),
        (a.album_sort or a.title or "").lower(),
    ))
    return albums


def find_album_by_id(root: str | os.PathLike, album_id: str) -> Album | None:
    for album in scan_albums(root):
        if album.id == album_id:
            return album
    return None


def get_embedded_cover(flac_path: str) -> tuple[bytes, str] | None:
    """Return (image_bytes, mime_type) for the largest embedded picture, or None."""
    try:
        audio = FLAC(flac_path)
    except Exception:
        return None
    if not audio.pictures:
        return None
    # prefer the largest picture (typically front cover)
    best: Picture = max(audio.pictures, key=lambda p: len(p.data))
    return best.data, best.mime or "image/jpeg"


def album_to_dict(album: Album) -> dict:
    return asdict(album)


def _cached_row_is_complete(cached: dict) -> bool:
    """Heuristic: did this cached album row come from a scan recent
    enough to have populated the v11 denormalized fields?

    The cache layer hands us back dicts that always have every key —
    missing values come through as None / [] rather than KeyError.
    What distinguishes a pre-v11 row from a fresh one is the
    `track_artists` field: pre-v11 rows have an empty list because
    the scanner wasn't capturing it yet, fresh rows from v11+ have
    a populated list whenever the album has any per-track ARTIST or
    ARTISTSORT tags.

    The catch: a fresh scan of an album with NO per-track artist
    tags also produces an empty list. We can't distinguish "scanned
    on v11 with empty result" from "scanned pre-v11 and stale". So
    we lean on this conservative rule: if the album has tracks and
    is_compilation is true, we expect track_artists to be populated;
    if not, force a re-read. Single-artist albums where the per-track
    artist matches album_artist often carry no per-track ARTIST tag
    at all, so we leave those alone.
    """
    track_artists = cached.get("track_artists")
    if track_artists:
        return True
    # Compilations almost always have per-track artist tags. Empty
    # track_artists on a compilation is a strong "stale row" signal.
    if cached.get("is_compilation"):
        return False
    return True


# ---------- Incremental scanning ----------

def discover_album_dirs(root: str | os.PathLike) -> list[str]:
    """Walk `root` and return absolute paths of every directory that directly
    contains FLAC files. No tag reads — just filesystem listing. Used to know
    the total album count up front so the UI can show progress.
    """
    root_path = Path(root).expanduser()
    if not root_path.exists():
        return []
    found: list[str] = []
    for dirpath, dirnames, filenames in os.walk(root_path):
        dirnames[:] = [d for d in dirnames if not d.startswith(".")]
        # Cheap check: any non-dotfile FLAC?
        for name in filenames:
            if name.lower().endswith(".flac") and not name.startswith("._"):
                found.append(str(Path(dirpath).resolve()))
                break
    return found


def scan_one_album(album_dir: str | os.PathLike) -> Album | None:
    """Scan a single album directory. Returns None if no FLACs / unreadable."""
    album_path = Path(album_dir)
    flacs = _list_flacs(album_path)
    if not flacs:
        return None
    return _read_album(album_path, flacs)


def incremental_scan(
    library_dir: str | os.PathLike,
    cached_by_path: dict[str, dict],
):
    """Generator over album candidates. Yields (status, album_dict) tuples:

      - "cached"  → the cached entry is still valid (mtime unchanged)
      - "fresh"   → the directory was new or modified, freshly scanned
      - "removed" → previously cached directory no longer exists / no FLACs

    All `album_dict`s have the same shape as `album_to_dict(Album)`.

    First yields a single ("discovered", {"total": N, "library_dir": ...})
    so callers can size their progress UI.
    """
    candidate_paths = discover_album_dirs(library_dir)
    candidate_set = set(candidate_paths)

    yield "discovered", {
        "total": len(candidate_paths),
        "library_dir": str(Path(library_dir).expanduser().resolve()),
    }

    for path in candidate_paths:
        cached = cached_by_path.get(path)
        # Use the same mtime computation as `_read_album` (max of dir + file
        # mtimes), otherwise tag edits would never invalidate the cache.
        current_mtime = compute_effective_mtime(Path(path))

        if cached and abs(cached.get("dir_mtime", 0.0) - current_mtime) < 0.001:
            # The mtime hasn't moved since the last scan, BUT new schema
            # versions sometimes add denormalized fields that didn't
            # exist when the row was originally written (track_numbers,
            # track_artists, …). When we see one of those missing, we
            # can't trust the cached row to be complete — force a fresh
            # read so the new field gets populated. This heals the gap
            # silently on the next scan instead of requiring the user
            # to know to force-rescan after every schema bump.
            if _cached_row_is_complete(cached):
                yield "cached", cached
                continue

        album = scan_one_album(path)
        if album is None:
            continue
        yield "fresh", album_to_dict(album)

    # Anything in the cache that's no longer on disk
    for stale_path, cached in cached_by_path.items():
        if stale_path not in candidate_set:
            yield "removed", cached
