"""SQLite backend for Musaic.

A single database file at `~/.musaic/library.db` (override via
`MUSAIC_CONFIG_DIR`) holds the cached scan state. Tables:

  - `schema_version` : single-row pragma for migrations
  - `libraries`      : one row per scanned library directory
  - `albums`         : one row per album, FK → libraries.path

The current schema is deliberately a like-for-like swap of the JSON cache.
Per-track metadata, playlist criteria, AI suggestion history, etc. will
land as new tables when the corresponding features arrive.

Connections: each call opens its own connection (cheap with SQLite). WAL
journal mode is enabled the first time the DB is touched so concurrent
reads/writes (scan + cover endpoint + UI poll) don't block each other.
"""
from __future__ import annotations

import logging
import sqlite3
from contextlib import contextmanager
from typing import Iterator

from .config import CONFIG_DIR

DB_PATH = CONFIG_DIR / "library.db"
SCHEMA_VERSION = 16

log = logging.getLogger(__name__)


SCHEMA_STATEMENTS = [
    """
    CREATE TABLE IF NOT EXISTS schema_version (
        version INTEGER PRIMARY KEY
    )
    """,
    """
    CREATE TABLE IF NOT EXISTS libraries (
        path        TEXT PRIMARY KEY,
        scanned_at  TEXT NOT NULL DEFAULT ''
    )
    """,
    """
    CREATE TABLE IF NOT EXISTS albums (
        id                  TEXT PRIMARY KEY,
        library_path        TEXT NOT NULL,
        path                TEXT NOT NULL UNIQUE,
        dir_name            TEXT,
        title               TEXT,
        artist              TEXT,
        album_artist        TEXT,
        year                TEXT,
        genre               TEXT,
        track_count         INTEGER NOT NULL DEFAULT 0,
        total_seconds       REAL    NOT NULL DEFAULT 0,
        has_embedded_cover  INTEGER NOT NULL DEFAULT 0,
        cover_file          TEXT,
        sample_track        TEXT NOT NULL DEFAULT '',
        dir_mtime           REAL    NOT NULL DEFAULT 0,
        -- v2: parsed from the directory name (flac-jacket convention)
        dir_artist          TEXT,
        dir_album           TEXT,
        is_compilation      INTEGER NOT NULL DEFAULT 0,
        disc_number         INTEGER,
        disc_total          INTEGER,
        -- v3: audio format info (from FLAC StreamInfo)
        sample_rate         INTEGER,
        bits_per_sample     INTEGER,
        channels            INTEGER,
        -- v6: per-track number sequence, JSON list. Denormalized for the
        -- track-gaps check so it doesn't need to re-read every FLAC.
        track_numbers_json  TEXT,
        -- v7: cached MusicBrainz anchors. Populated on the first confident
        -- find_release() match; subsequent lookups short-circuit the search
        -- step. Refreshed when last_checked_at exceeds a staleness window.
        mb_release_group_mbid TEXT,
        mb_release_mbid       TEXT,
        mb_match_score        INTEGER,
        mb_last_checked_at    TEXT,
        -- v8: Vorbis sort variants. Read by the scanner from the FLAC's
        -- ALBUMARTISTSORT / ARTISTSORT / ALBUMSORT tags. NULL when the
        -- source files don't carry sort tags. Plex / Roon honor these
        -- for sidebar / library alphabetization.
        album_artist_sort   TEXT,
        artist_sort         TEXT,
        album_sort          TEXT,
        -- v11: per-track artist + artistsort, denormalized so the
        -- inconsistent-artist-sort check can cluster across album AND
        -- track levels without re-reading every FLAC. Stored as a JSON
        -- list of {filename, artist, artist_sort} dicts. Mirrors the
        -- track_numbers_json pattern; populated in the same per-FLAC
        -- loop in scanner._scan_album_dir, so reading is free.
        track_artists_json  TEXT,
        -- v12: timestamp of the most recent AI metadata job whose
        -- output the user acted on (applied OR discarded — both
        -- count as "I reviewed AI's suggestion"). NULL = never been
        -- through an AI metadata pass that the user reviewed.
        -- Used by the UI to show "AI reviewed: yes/no" so the user
        -- doesn't keep re-running AI on the same albums by accident,
        -- and (eventually) by Library filters to bucket the unreviewed
        -- backlog.
        metadata_ai_acted_on_at TEXT,
        -- v15: presence-as-data. ISO-8601 timestamp of the first scan
        -- that observed the album dir gone from disk; NULL when the
        -- album is currently Present. Lets us preserve the row (and
        -- its computed children — fingerprints, MB anchor, track
        -- tags) across drive outages, reorganizations, and transient
        -- FS errors. Only `cache.delete_album` actually removes a
        -- row, and that's only reachable via explicit user action
        -- (`DELETE /api/albums/{id}`). Scans never delete.
        missing_since TEXT,
        FOREIGN KEY (library_path) REFERENCES libraries(path) ON DELETE CASCADE
    )
    """,
    "CREATE INDEX IF NOT EXISTS idx_albums_library ON albums(library_path)",
    # NOTE: a partial index on `WHERE missing_since IS NULL` was
    # tried but removed — `SCHEMA_STATEMENTS` runs BEFORE migrations
    # in bootstrap(), and on an existing v14 install the
    # CREATE TABLE IF NOT EXISTS is a no-op (column not yet added by
    # the v15 migration), so any index referencing missing_since
    # crashes the bootstrap transaction before the migration can run.
    # The existing idx_albums_library covers the common
    # `WHERE library_path = ?` filter; the additional
    # `AND missing_since IS NULL` scan within that is fast enough for
    # typical libraries (few thousand albums). Add a partial index
    # later via a migration if profiling demands it — but the
    # bootstrap ordering means it can't live in SCHEMA_STATEMENTS.
    """
    CREATE TABLE IF NOT EXISTS ai_jobs (
        id                 TEXT PRIMARY KEY,         -- short hex
        album_id           TEXT NOT NULL,
        album_path         TEXT NOT NULL,            -- denormalized for display
        album_title        TEXT,                     -- denormalized for display
        job_type           TEXT NOT NULL,            -- "metadata" | "cover" | "metadata+cover"
        hint               TEXT NOT NULL DEFAULT '', -- user-supplied prompt context
        status             TEXT NOT NULL,            -- "queued"|"running"|"completed"|"failed"|"acted_on"
        result_json        TEXT,                     -- JSON-encoded MetadataSuggestion + CoverSuggestion combo
        error              TEXT,                     -- exception message when status="failed"
        acted_on_action    TEXT,                     -- "applied" | "discarded" once status="acted_on"
        created_at         TEXT NOT NULL,
        started_at         TEXT,
        completed_at       TEXT,
        acted_on_at        TEXT,
        -- v16: per-job toggle for whether the album cover art is sent
        -- with the suggest_metadata prompt. Default 1 (on) matches
        -- the post-cover-attach launch behavior. User can uncheck
        -- via the hint dialog when they want to A/B-test the
        -- cover's effect on identification quality / latency.
        attach_cover       INTEGER NOT NULL DEFAULT 1
    )
    """,
    "CREATE INDEX IF NOT EXISTS idx_ai_jobs_status ON ai_jobs(status)",
    "CREATE INDEX IF NOT EXISTS idx_ai_jobs_album  ON ai_jobs(album_id)",
    "CREATE INDEX IF NOT EXISTS idx_ai_jobs_created ON ai_jobs(created_at)",
    """
    CREATE TABLE IF NOT EXISTS settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    )
    """,
    # --- v6: maintenance / health checks ---
    """
    CREATE TABLE IF NOT EXISTS check_runs (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        check_id        TEXT NOT NULL,
        started_at      TEXT NOT NULL,
        completed_at    TEXT,
        status          TEXT NOT NULL,           -- "running"|"completed"|"failed"|"cancelled"
        library_path    TEXT NOT NULL,
        scope           TEXT NOT NULL,           -- "all"|"incremental"|JSON list of album_ids
        issue_count     INTEGER NOT NULL DEFAULT 0,
        error           TEXT,
        duration_ms     INTEGER
    )
    """,
    "CREATE INDEX IF NOT EXISTS idx_check_runs_check ON check_runs(check_id, started_at DESC)",
    """
    CREATE TABLE IF NOT EXISTS check_findings (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        check_id        TEXT NOT NULL,
        album_id        TEXT,                    -- NULL = library-level finding
        severity        TEXT NOT NULL DEFAULT 'warn',  -- "info"|"warn"|"error"
        summary         TEXT NOT NULL,
        detail_json     TEXT,
        first_seen_at   TEXT NOT NULL,
        last_seen_at    TEXT NOT NULL,
        resolved_at     TEXT,
        ignored_at      TEXT,
        ignored_reason  TEXT,
        UNIQUE(check_id, album_id, summary)
    )
    """,
    "CREATE INDEX IF NOT EXISTS idx_findings_check ON check_findings(check_id, resolved_at, ignored_at)",
    "CREATE INDEX IF NOT EXISTS idx_findings_album ON check_findings(album_id)",
    # --- v7: per-track AcoustID/Chromaprint fingerprints ---
    """
    CREATE TABLE IF NOT EXISTS track_fingerprints (
        album_id           TEXT NOT NULL,
        filename           TEXT NOT NULL,
        fingerprint        TEXT NOT NULL,            -- chromaprint hash
        duration_s         REAL NOT NULL,            -- needed for AcoustID submission
        acoustid_recording_mbid TEXT,                -- canonical recording MBID, if matched
        acoustid_score     REAL,                     -- 0.0–1.0 confidence
        last_computed_at   TEXT NOT NULL,
        PRIMARY KEY (album_id, filename),
        FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
    )
    """,
    # Lets us answer "find all tracks matching this AcoustID" cheaply for the
    # future duplicate-detection check (D2 in docs/maintenance-scripts.md).
    "CREATE INDEX IF NOT EXISTS idx_track_fingerprints_recording "
    "ON track_fingerprints(acoustid_recording_mbid) "
    "WHERE acoustid_recording_mbid IS NOT NULL",
    # --- v9: per-track user tags ---
    # User-applied tags for shuffling / playlist construction. Stored
    # PER TRACK (not per album) so the user can have an album mostly
    # tagged "Cocktail Hour" while flagging one specific track as
    # "Christmas". Album-level chips in the UI are computed as the
    # intersection of per-track tags. Sidecar JSON in the album dir
    # is the persistence layer that survives DB resets / app
    # reinstalls; DB is authoritative once a session has touched it.
    """
    CREATE TABLE IF NOT EXISTS track_tags (
        album_id   TEXT NOT NULL,
        filename   TEXT NOT NULL,
        tag        TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (album_id, filename, tag),
        FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
    )
    """,
    # Index for "find every track tagged X" (the playlist-source query).
    "CREATE INDEX IF NOT EXISTS idx_track_tags_tag ON track_tags(tag)",
    # --- v10: user-curated tag vocabulary ---
    # The approved list of tags. Distinct from track_tags so that imported
    # sidecar tags from before this feature existed (or any free-tag
    # data) can still live in track_tags as "orphan" tags until the user
    # decides whether to promote them into the vocabulary, rename them,
    # or strip them. No FK from track_tags(tag) → tag_vocabulary(tag) for
    # exactly this reason — the two tables drift independently and the
    # UI surfaces the gap.
    #
    # COLLATE NOCASE on the PK prevents the casing-drift problem that
    # motivated this whole layer ("Cocktail Hour" vs "cocktail hour"
    # silently fragmenting playlist queries). The picker UI never lets
    # the user create that drift; this collation is the safety net for
    # anyone editing the table directly.
    """
    CREATE TABLE IF NOT EXISTS tag_vocabulary (
        tag         TEXT PRIMARY KEY COLLATE NOCASE,
        description TEXT NOT NULL DEFAULT '',
        color       TEXT,
        sort_order  INTEGER NOT NULL DEFAULT 0,
        created_at  TEXT NOT NULL
    )
    """,
    "CREATE INDEX IF NOT EXISTS idx_tag_vocabulary_sort ON tag_vocabulary(sort_order, tag)",
    # --- v13: per-album MusicBrainz release-group blocklist ---
    # Populated by the "Wrong match" UI affordance: when the user
    # rejects a cached MB anchor for an album, the rejected
    # release_group_mbid is recorded here so the resolver's Tier 0b
    # (dir-search) and Tier 0c (fingerprint vote) skip it, and AI
    # MBID-persist refuses it. Without this, fingerprint vote would
    # immediately re-derive the same wrong release-group from the
    # cached AcoustID matches and the user would be stuck.
    """
    CREATE TABLE IF NOT EXISTS album_blocked_mb_release_groups (
        album_id           TEXT NOT NULL,
        release_group_mbid TEXT NOT NULL,
        blocked_at         TEXT NOT NULL,
        PRIMARY KEY (album_id, release_group_mbid),
        FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
    )
    """,
]


# Ordered list of column names matching the album rows we read/write. Kept in
# one place so insert/select stay in sync.
ALBUM_COLUMNS = (
    "id", "library_path", "path", "dir_name", "title", "artist",
    "album_artist", "year", "genre", "track_count", "total_seconds",
    "has_embedded_cover", "cover_file", "sample_track", "dir_mtime",
    "dir_artist", "dir_album", "is_compilation", "disc_number", "disc_total",
    "sample_rate", "bits_per_sample", "channels",
    "track_numbers_json",            # v6: per-track tracknumber sequence
    # v7: cached MusicBrainz anchors. NOT populated by the scanner — written
    # back from mb_lookup.find_release() on a confident match. Reads always
    # see whatever value the most recent successful match produced (or NULL
    # if the album hasn't been MB-identified yet).
    "mb_release_group_mbid",
    "mb_release_mbid",
    "mb_match_score",
    "mb_last_checked_at",
    # v8: Vorbis sort variants. Populated by the scanner from the FLAC's
    # ALBUMARTISTSORT / ARTISTSORT / ALBUMSORT tags. NULL when the source
    # files don't carry sort tags (which is the common case for libraries
    # that haven't been tagged with Picard / beets / similar). The
    # missing-sort-tags check flags albums whose primary tag starts with
    # "The "/"A "/"An " but has no corresponding *sort field, and offers
    # to populate it via article rotation. Plex / Roon / most modern
    # players use these for proper alphabetical sort.
    "album_artist_sort",
    "artist_sort",
    "album_sort",
    # v11: per-track artist credits. Populated by the scanner in the
    # same per-FLAC loop that reads tracknumbers + genres. Used by the
    # inconsistent-artist-sort check to cluster across album AND track
    # levels — compilation tracks credit individual artists, and those
    # credits drift the same way album_artist credits do.
    "track_artists_json",
    # v12: timestamp of most recent AI metadata job acted_on. NULL
    # for albums never reviewed via AI. Maintained by the worker
    # acted_on transition (see main.mark_acted_on); not populated by
    # the scanner.
    "metadata_ai_acted_on_at",
    # v15: presence-as-data. Non-null = album dir is currently absent
    # from disk; row + computed children (fingerprints, MB anchor,
    # track tags) preserved as-is until the user explicitly removes
    # the album. See db.py CREATE TABLE for the full rationale.
    "missing_since",
)


# Per-version forward migrations, applied in numeric order whenever the DB's
# stored version is below SCHEMA_VERSION. Each list of statements runs inside
# a single transaction. Statements should be idempotent OR guarded by version
# checks at the call site. Keep entries minimal and append-only.
MIGRATIONS: dict[int, list[str]] = {
    2: [
        # ALTER TABLE ... ADD COLUMN works in SQLite. Each ADD must be its own
        # statement. New columns get their default value for existing rows.
        "ALTER TABLE albums ADD COLUMN dir_artist TEXT",
        "ALTER TABLE albums ADD COLUMN dir_album TEXT",
        "ALTER TABLE albums ADD COLUMN is_compilation INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE albums ADD COLUMN disc_number INTEGER",
        "ALTER TABLE albums ADD COLUMN disc_total INTEGER",
    ],
    3: [
        # Audio format info (FLAC StreamInfo). Existing rows get NULL until
        # the next scan picks them up — that's intentional, the user's next
        # rescan will populate them naturally.
        "ALTER TABLE albums ADD COLUMN sample_rate INTEGER",
        "ALTER TABLE albums ADD COLUMN bits_per_sample INTEGER",
        "ALTER TABLE albums ADD COLUMN channels INTEGER",
    ],
    4: [
        # AI job queue. The CREATE TABLE in SCHEMA_STATEMENTS handles fresh
        # installs (IF NOT EXISTS); this migration creates it for upgraded
        # DBs that didn't have it. Indexes likewise.
        """
        CREATE TABLE IF NOT EXISTS ai_jobs (
            id                 TEXT PRIMARY KEY,
            album_id           TEXT NOT NULL,
            album_path         TEXT NOT NULL,
            album_title        TEXT,
            job_type           TEXT NOT NULL,
            hint               TEXT NOT NULL DEFAULT '',
            status             TEXT NOT NULL,
            result_json        TEXT,
            error              TEXT,
            acted_on_action    TEXT,
            created_at         TEXT NOT NULL,
            started_at         TEXT,
            completed_at       TEXT,
            acted_on_at        TEXT
        )
        """,
        "CREATE INDEX IF NOT EXISTS idx_ai_jobs_status ON ai_jobs(status)",
        "CREATE INDEX IF NOT EXISTS idx_ai_jobs_album  ON ai_jobs(album_id)",
        "CREATE INDEX IF NOT EXISTS idx_ai_jobs_created ON ai_jobs(created_at)",
    ],
    5: [
        # Generic key/value store. First users: ai_max_parallel and
        # ai_queue_paused. Future-proofing for things like sort order, hint
        # presets, etc., without one column per setting.
        """
        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
        """,
    ],
    7: [
        # Cached MusicBrainz anchors per album — populated lazily on the
        # first confident find_release() match. Lets all subsequent fix
        # paths short-circuit the search step.
        "ALTER TABLE albums ADD COLUMN mb_release_group_mbid TEXT",
        "ALTER TABLE albums ADD COLUMN mb_release_mbid TEXT",
        "ALTER TABLE albums ADD COLUMN mb_match_score INTEGER",
        "ALTER TABLE albums ADD COLUMN mb_last_checked_at TEXT",
        # Per-track AcoustID/Chromaprint fingerprints — see the table comment
        # in SCHEMA_STATEMENTS for full notes.
        """
        CREATE TABLE IF NOT EXISTS track_fingerprints (
            album_id           TEXT NOT NULL,
            filename           TEXT NOT NULL,
            fingerprint        TEXT NOT NULL,
            duration_s         REAL NOT NULL,
            acoustid_recording_mbid TEXT,
            acoustid_score     REAL,
            last_computed_at   TEXT NOT NULL,
            PRIMARY KEY (album_id, filename),
            FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
        )
        """,
        "CREATE INDEX IF NOT EXISTS idx_track_fingerprints_recording "
        "ON track_fingerprints(acoustid_recording_mbid) "
        "WHERE acoustid_recording_mbid IS NOT NULL",
    ],
    8: [
        # Vorbis sort variants. Read by the scanner when present in the
        # source FLAC's tags; populated by the missing-sort-tags fix path
        # (deterministic article rotation) and the AI metadata flow when
        # Claude identifies the canonical sort form via MusicBrainz.
        "ALTER TABLE albums ADD COLUMN album_artist_sort TEXT",
        "ALTER TABLE albums ADD COLUMN artist_sort TEXT",
        "ALTER TABLE albums ADD COLUMN album_sort TEXT",
    ],
    9: [
        # Per-track user tags. Mirrors the SCHEMA_STATEMENTS entry so
        # upgraded DBs that didn't have this table get it. See the table
        # comment in SCHEMA_STATEMENTS for the design rationale (per-track
        # granularity + sidecar JSON authority on first scan, DB
        # authority once a session has touched it).
        """
        CREATE TABLE IF NOT EXISTS track_tags (
            album_id   TEXT NOT NULL,
            filename   TEXT NOT NULL,
            tag        TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (album_id, filename, tag),
            FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
        )
        """,
        "CREATE INDEX IF NOT EXISTS idx_track_tags_tag ON track_tags(tag)",
    ],
    10: [
        # User-curated tag vocabulary. Mirrors SCHEMA_STATEMENTS.
        """
        CREATE TABLE IF NOT EXISTS tag_vocabulary (
            tag         TEXT PRIMARY KEY COLLATE NOCASE,
            description TEXT NOT NULL DEFAULT '',
            color       TEXT,
            sort_order  INTEGER NOT NULL DEFAULT 0,
            created_at  TEXT NOT NULL
        )
        """,
        "CREATE INDEX IF NOT EXISTS idx_tag_vocabulary_sort ON tag_vocabulary(sort_order, tag)",
    ],
    11: [
        # Denormalized per-track artist + artistsort for the
        # inconsistent-artist-sort check. NULL on existing rows; the
        # next scan populates them naturally.
        "ALTER TABLE albums ADD COLUMN track_artists_json TEXT",
    ],
    12: [
        # Timestamp of most recent AI metadata job acted_on. Lets
        # the UI show "AI reviewed: yes/no" so the user doesn't
        # re-run AI on already-reviewed albums by accident.
        "ALTER TABLE albums ADD COLUMN metadata_ai_acted_on_at TEXT",
    ],
    13: [
        # Per-album MB release-group blocklist (the "Wrong match"
        # escape hatch). The CREATE TABLE in SCHEMA_STATEMENTS handles
        # fresh installs (IF NOT EXISTS); this mirrors it for upgraded
        # DBs.
        """
        CREATE TABLE IF NOT EXISTS album_blocked_mb_release_groups (
            album_id           TEXT NOT NULL,
            release_group_mbid TEXT NOT NULL,
            blocked_at         TEXT NOT NULL,
            PRIMARY KEY (album_id, release_group_mbid),
            FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
        )
        """,
    ],
    6: [
        # Maintenance / health-check infrastructure. The CREATE TABLEs in
        # SCHEMA_STATEMENTS handle fresh installs; these mirror them so
        # upgraded DBs that didn't have them get them.
        """
        CREATE TABLE IF NOT EXISTS check_runs (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            check_id        TEXT NOT NULL,
            started_at      TEXT NOT NULL,
            completed_at    TEXT,
            status          TEXT NOT NULL,
            library_path    TEXT NOT NULL,
            scope           TEXT NOT NULL,
            issue_count     INTEGER NOT NULL DEFAULT 0,
            error           TEXT,
            duration_ms     INTEGER
        )
        """,
        "CREATE INDEX IF NOT EXISTS idx_check_runs_check ON check_runs(check_id, started_at DESC)",
        """
        CREATE TABLE IF NOT EXISTS check_findings (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            check_id        TEXT NOT NULL,
            album_id        TEXT,
            severity        TEXT NOT NULL DEFAULT 'warn',
            summary         TEXT NOT NULL,
            detail_json     TEXT,
            first_seen_at   TEXT NOT NULL,
            last_seen_at    TEXT NOT NULL,
            resolved_at     TEXT,
            ignored_at      TEXT,
            ignored_reason  TEXT,
            UNIQUE(check_id, album_id, summary)
        )
        """,
        "CREATE INDEX IF NOT EXISTS idx_findings_check ON check_findings(check_id, resolved_at, ignored_at)",
        "CREATE INDEX IF NOT EXISTS idx_findings_album ON check_findings(album_id)",
        # The Phase 2 track-gaps check (M3) needs to inspect per-track number
        # sequences without re-reading every FLAC. Denormalize them at scan
        # time. Existing rows get NULL; the next scan populates.
        "ALTER TABLE albums ADD COLUMN track_numbers_json TEXT",
    ],
    14: [
        # Health-check consolidation: missing-fingerprints + missing-mbid
        # collapsed into 'unidentified-album'; missing-core-tags +
        # missing-sort-tags + mb-canonical-mismatch collapsed into
        # 'tags-out-of-sync'. The merged checks emit fresh findings under
        # the new ids; resolve any rows still open under the old ids so
        # they don't linger forever (the runner only auto-resolves
        # findings whose check actually runs again, and the old check
        # ids are gone from the registry).
        """
        UPDATE check_findings
        SET resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        WHERE resolved_at IS NULL
          AND check_id IN (
            'missing-fingerprints', 'missing-mbid',
            'missing-core-tags', 'missing-sort-tags',
            'mb-canonical-mismatch'
          )
        """,
    ],
    15: [
        # Presence-as-data: each album row gets a `missing_since` column
        # that's NULL when the album's dir is currently on disk and an
        # ISO timestamp when it isn't. Scans set the timestamp instead
        # of deleting the row; the row + its computed children (track
        # fingerprints, MB anchor, track tags) survive drive outages
        # and reorganizations. Only `cache.delete_album` (reached only
        # via explicit user action `DELETE /api/albums/{id}`) actually
        # removes data.
        "ALTER TABLE albums ADD COLUMN missing_since TEXT",
    ],
    16: [
        # Per-job toggle for whether the album cover art is sent with
        # the suggest_metadata prompt. Default 1 (on) preserves the
        # post-cover-attach launch behavior. User can uncheck via the
        # hint dialog to A/B-test whether attaching the cover hurts
        # job reliability / latency.
        "ALTER TABLE ai_jobs ADD COLUMN attach_cover INTEGER NOT NULL DEFAULT 1",
    ],
}


def _ensure_dir() -> None:
    CONFIG_DIR.mkdir(parents=True, exist_ok=True)


def _connect_raw() -> sqlite3.Connection:
    """Open a connection without bootstrap. Used by `bootstrap()` itself."""
    _ensure_dir()
    conn = sqlite3.connect(str(DB_PATH), isolation_level=None, timeout=30)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    return conn


_bootstrap_done = False


def bootstrap() -> None:
    """Create tables / set pragmas / run migrations if needed. Idempotent."""
    global _bootstrap_done
    if _bootstrap_done and DB_PATH.is_file():
        return
    conn = _connect_raw()
    try:
        # WAL gives readers and a writer concurrent access without blocking.
        conn.execute("PRAGMA journal_mode = WAL")
        conn.execute("PRAGMA synchronous = NORMAL")
        with _transaction(conn):
            for stmt in SCHEMA_STATEMENTS:
                conn.execute(stmt)
            row = conn.execute("SELECT version FROM schema_version LIMIT 1").fetchone()
            current = row["version"] if row else 0
            if current == 0:
                conn.execute("INSERT INTO schema_version(version) VALUES (?)", (SCHEMA_VERSION,))
            elif current < SCHEMA_VERSION:
                _migrate(conn, current, SCHEMA_VERSION)
            elif current > SCHEMA_VERSION:
                log.warning("DB schema_version=%s is newer than expected %s — proceeding read-only-ish", current, SCHEMA_VERSION)
    finally:
        conn.close()
    _bootstrap_done = True


def _migrate(conn: sqlite3.Connection, from_version: int, to_version: int) -> None:
    """Run forward migrations from `from_version` (exclusive) to `to_version`."""
    for v in range(from_version + 1, to_version + 1):
        statements = MIGRATIONS.get(v)
        if not statements:
            continue
        log.info("Migrating DB schema %d → %d", v - 1, v)
        for stmt in statements:
            try:
                conn.execute(stmt)
            except sqlite3.OperationalError as e:
                # Idempotent re-runs may hit "duplicate column" — ignore those.
                if "duplicate column name" in str(e).lower():
                    log.debug("skip already-applied: %s", stmt)
                    continue
                raise
    conn.execute("UPDATE schema_version SET version = ?", (to_version,))


@contextmanager
def _transaction(conn: sqlite3.Connection) -> Iterator[sqlite3.Connection]:
    """Wrap a block in BEGIN/COMMIT (rollback on error)."""
    conn.execute("BEGIN")
    try:
        yield conn
        conn.execute("COMMIT")
    except Exception:
        conn.execute("ROLLBACK")
        raise


@contextmanager
def connect() -> Iterator[sqlite3.Connection]:
    """Open a connection (bootstrapping the DB if needed) and close on exit."""
    bootstrap()
    conn = _connect_raw()
    try:
        yield conn
    finally:
        conn.close()


def transaction(conn: sqlite3.Connection):
    """Public wrapper around the BEGIN/COMMIT context manager."""
    return _transaction(conn)
