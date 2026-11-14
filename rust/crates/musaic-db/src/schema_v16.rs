//! The Python app's `musaic/db.py::SCHEMA_STATEMENTS` (schema v16), verbatim.
//! Generated from the Python app's source (musaic/db.py at 6902c23); do not edit.
//! SQLite stores each CREATE statement's text in `sqlite_master`, so the text
//! must match the Python app's byte for byte (testdata/golden/persistence/fresh-db.json).

pub(crate) const STATEMENTS: &[&str] = &[
    r###"
    CREATE TABLE IF NOT EXISTS schema_version (
        version INTEGER PRIMARY KEY
    )
    "###,
    r###"
    CREATE TABLE IF NOT EXISTS libraries (
        path        TEXT PRIMARY KEY,
        scanned_at  TEXT NOT NULL DEFAULT ''
    )
    "###,
    r###"
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
    "###,
    r###"CREATE INDEX IF NOT EXISTS idx_albums_library ON albums(library_path)"###,
    r###"
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
    "###,
    r###"CREATE INDEX IF NOT EXISTS idx_ai_jobs_status ON ai_jobs(status)"###,
    r###"CREATE INDEX IF NOT EXISTS idx_ai_jobs_album  ON ai_jobs(album_id)"###,
    r###"CREATE INDEX IF NOT EXISTS idx_ai_jobs_created ON ai_jobs(created_at)"###,
    r###"
    CREATE TABLE IF NOT EXISTS settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    )
    "###,
    r###"
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
    "###,
    r###"CREATE INDEX IF NOT EXISTS idx_check_runs_check ON check_runs(check_id, started_at DESC)"###,
    r###"
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
    "###,
    r###"CREATE INDEX IF NOT EXISTS idx_findings_check ON check_findings(check_id, resolved_at, ignored_at)"###,
    r###"CREATE INDEX IF NOT EXISTS idx_findings_album ON check_findings(album_id)"###,
    r###"
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
    "###,
    r###"CREATE INDEX IF NOT EXISTS idx_track_fingerprints_recording ON track_fingerprints(acoustid_recording_mbid) WHERE acoustid_recording_mbid IS NOT NULL"###,
    r###"
    CREATE TABLE IF NOT EXISTS track_tags (
        album_id   TEXT NOT NULL,
        filename   TEXT NOT NULL,
        tag        TEXT NOT NULL,
        created_at TEXT NOT NULL,
        PRIMARY KEY (album_id, filename, tag),
        FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
    )
    "###,
    r###"CREATE INDEX IF NOT EXISTS idx_track_tags_tag ON track_tags(tag)"###,
    r###"
    CREATE TABLE IF NOT EXISTS tag_vocabulary (
        tag         TEXT PRIMARY KEY COLLATE NOCASE,
        description TEXT NOT NULL DEFAULT '',
        color       TEXT,
        sort_order  INTEGER NOT NULL DEFAULT 0,
        created_at  TEXT NOT NULL
    )
    "###,
    r###"CREATE INDEX IF NOT EXISTS idx_tag_vocabulary_sort ON tag_vocabulary(sort_order, tag)"###,
    r###"
    CREATE TABLE IF NOT EXISTS album_blocked_mb_release_groups (
        album_id           TEXT NOT NULL,
        release_group_mbid TEXT NOT NULL,
        blocked_at         TEXT NOT NULL,
        PRIMARY KEY (album_id, release_group_mbid),
        FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
    )
    "###,
];
