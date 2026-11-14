//! The Python app's `musaic/db.py::MIGRATIONS` (steps v2..v16), verbatim, in version order.
//! Generated from the Python app's source (musaic/db.py at 6902c23); do not edit.
//! `ALTER TABLE ... ADD COLUMN` text is spliced into the table's `sqlite_master.sql`,
//! so the text must match the Python app's byte for byte
//! (rust/testdata/golden/persistence/old-schemas/).

pub(crate) const MIGRATIONS: &[(i64, &[&str])] = &[
    (
        2,
        &[
            r###"ALTER TABLE albums ADD COLUMN dir_artist TEXT"###,
            r###"ALTER TABLE albums ADD COLUMN dir_album TEXT"###,
            r###"ALTER TABLE albums ADD COLUMN is_compilation INTEGER NOT NULL DEFAULT 0"###,
            r###"ALTER TABLE albums ADD COLUMN disc_number INTEGER"###,
            r###"ALTER TABLE albums ADD COLUMN disc_total INTEGER"###,
        ],
    ),
    (
        3,
        &[
            r###"ALTER TABLE albums ADD COLUMN sample_rate INTEGER"###,
            r###"ALTER TABLE albums ADD COLUMN bits_per_sample INTEGER"###,
            r###"ALTER TABLE albums ADD COLUMN channels INTEGER"###,
        ],
    ),
    (
        4,
        &[
            r###"
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
        "###,
            r###"CREATE INDEX IF NOT EXISTS idx_ai_jobs_status ON ai_jobs(status)"###,
            r###"CREATE INDEX IF NOT EXISTS idx_ai_jobs_album  ON ai_jobs(album_id)"###,
            r###"CREATE INDEX IF NOT EXISTS idx_ai_jobs_created ON ai_jobs(created_at)"###,
        ],
    ),
    (
        5,
        &[r###"
        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
        "###],
    ),
    (
        6,
        &[
            r###"
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
        "###,
            r###"CREATE INDEX IF NOT EXISTS idx_check_runs_check ON check_runs(check_id, started_at DESC)"###,
            r###"
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
        "###,
            r###"CREATE INDEX IF NOT EXISTS idx_findings_check ON check_findings(check_id, resolved_at, ignored_at)"###,
            r###"CREATE INDEX IF NOT EXISTS idx_findings_album ON check_findings(album_id)"###,
            r###"ALTER TABLE albums ADD COLUMN track_numbers_json TEXT"###,
        ],
    ),
    (
        7,
        &[
            r###"ALTER TABLE albums ADD COLUMN mb_release_group_mbid TEXT"###,
            r###"ALTER TABLE albums ADD COLUMN mb_release_mbid TEXT"###,
            r###"ALTER TABLE albums ADD COLUMN mb_match_score INTEGER"###,
            r###"ALTER TABLE albums ADD COLUMN mb_last_checked_at TEXT"###,
            r###"
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
        "###,
            r###"CREATE INDEX IF NOT EXISTS idx_track_fingerprints_recording ON track_fingerprints(acoustid_recording_mbid) WHERE acoustid_recording_mbid IS NOT NULL"###,
        ],
    ),
    (
        8,
        &[
            r###"ALTER TABLE albums ADD COLUMN album_artist_sort TEXT"###,
            r###"ALTER TABLE albums ADD COLUMN artist_sort TEXT"###,
            r###"ALTER TABLE albums ADD COLUMN album_sort TEXT"###,
        ],
    ),
    (
        9,
        &[
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
        ],
    ),
    (
        10,
        &[
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
        ],
    ),
    (
        11,
        &[r###"ALTER TABLE albums ADD COLUMN track_artists_json TEXT"###],
    ),
    (
        12,
        &[r###"ALTER TABLE albums ADD COLUMN metadata_ai_acted_on_at TEXT"###],
    ),
    (
        13,
        &[r###"
        CREATE TABLE IF NOT EXISTS album_blocked_mb_release_groups (
            album_id           TEXT NOT NULL,
            release_group_mbid TEXT NOT NULL,
            blocked_at         TEXT NOT NULL,
            PRIMARY KEY (album_id, release_group_mbid),
            FOREIGN KEY (album_id) REFERENCES albums(id) ON DELETE CASCADE
        )
        "###],
    ),
    (
        14,
        &[r###"
        UPDATE check_findings
        SET resolved_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
        WHERE resolved_at IS NULL
          AND check_id IN (
            'missing-fingerprints', 'missing-mbid',
            'missing-core-tags', 'missing-sort-tags',
            'mb-canonical-mismatch'
          )
        "###],
    ),
    (
        15,
        &[r###"ALTER TABLE albums ADD COLUMN missing_since TEXT"###],
    ),
    (
        16,
        &[r###"ALTER TABLE ai_jobs ADD COLUMN attach_cover INTEGER NOT NULL DEFAULT 1"###],
    ),
];
