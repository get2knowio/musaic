CREATE TABLE library_epoch (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    epoch TEXT NOT NULL,
    root_bound INTEGER NOT NULL DEFAULT 0 CHECK (root_bound IN (0,1)),
    root TEXT
);

CREATE TABLE delegated_work (
    id TEXT PRIMARY KEY,
    schema_version INTEGER NOT NULL DEFAULT 1 CHECK (schema_version = 1),
    group_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('metadata.identify', 'metadata.correct', 'cover.research')),
    album_id TEXT NOT NULL,
    library_epoch TEXT NOT NULL,
    base_revision TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN
        ('queued','claimed','running','awaiting_review','applied','discarded','failed','cancelled')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    input_json TEXT NOT NULL CHECK (json_valid(input_json) AND length(CAST(input_json AS BLOB)) <= 262144),
    input_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    available_at INTEGER NOT NULL,
    principal_id TEXT,
    lease_token_hash TEXT,
    lease_expires_at INTEGER,
    claim_generation INTEGER NOT NULL DEFAULT 0 CHECK (claim_generation >= 0),
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL CHECK (max_attempts > 0 AND attempts BETWEEN 0 AND max_attempts),
    last_error TEXT,
    result_id TEXT,
    legacy_job_id TEXT,
    review_reserved INTEGER NOT NULL DEFAULT 0 CHECK (review_reserved IN (0,1)),
    CHECK ((state IN ('claimed', 'running') AND principal_id IS NOT NULL
        AND lease_token_hash IS NOT NULL AND lease_expires_at IS NOT NULL)
        OR (state NOT IN ('claimed', 'running') AND lease_token_hash IS NULL
        AND lease_expires_at IS NULL)),
    CHECK (review_reserved = 0 OR state = 'awaiting_review'),
    UNIQUE (legacy_job_id, kind)
);

CREATE UNIQUE INDEX delegated_work_unresolved
    ON delegated_work(library_epoch, album_id, kind, input_digest)
    WHERE state IN ('queued','claimed','running','awaiting_review');
CREATE INDEX delegated_work_eligible ON delegated_work(library_epoch, state, available_at, id);
CREATE INDEX delegated_work_groups ON delegated_work(group_id, id);
CREATE INDEX delegated_work_leases ON delegated_work(lease_expires_at) WHERE lease_expires_at IS NOT NULL;

CREATE TABLE work_group_aliases (
    old_group_id TEXT PRIMARY KEY,
    group_id TEXT NOT NULL CHECK (old_group_id <> group_id)
);

CREATE TABLE work_attempts (
    work_id TEXT NOT NULL REFERENCES delegated_work(id),
    generation INTEGER NOT NULL CHECK (generation > 0),
    principal_id TEXT NOT NULL,
    lease_token_hash TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    progress_sequence INTEGER NOT NULL DEFAULT -1 CHECK (progress_sequence >= -1),
    stage TEXT,
    completed INTEGER,
    total INTEGER CHECK (total IS NULL OR total >= 0),
    message TEXT,
    finished_at INTEGER,
    terminal_reason TEXT,
    CHECK ((finished_at IS NULL AND terminal_reason IS NULL) OR
        (finished_at IS NOT NULL AND terminal_reason IS NOT NULL)),
    CHECK (completed IS NULL OR (completed >= 0 AND (total IS NULL OR completed <= total))),
    PRIMARY KEY (work_id, generation)
);

CREATE TABLE work_results (
    id TEXT PRIMARY KEY,
    work_id TEXT NOT NULL REFERENCES delegated_work(id),
    input_digest TEXT NOT NULL,
    digest TEXT NOT NULL,
    result_json TEXT NOT NULL CHECK (json_valid(result_json) AND length(CAST(result_json AS BLOB)) <= 262144),
    validation TEXT NOT NULL CHECK (validation IN ('pending','verified','rejected')),
    created_at INTEGER NOT NULL
);

CREATE TABLE work_replays (
    library_epoch TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('claim','complete')),
    replay_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    response_json TEXT NOT NULL CHECK (json_valid(response_json)),
    PRIMARY KEY (library_epoch, principal_id, operation, replay_key)
);

CREATE TABLE delegation_audit (
    sequence INTEGER PRIMARY KEY,
    library_epoch TEXT NOT NULL,
    work_id TEXT,
    operation_id TEXT,
    transition TEXT NOT NULL,
    recorded_at INTEGER NOT NULL
);

CREATE TABLE delegation_outbox (
    sequence INTEGER PRIMARY KEY REFERENCES delegation_audit(sequence),
    library_epoch TEXT NOT NULL,
    work_id TEXT,
    operation_id TEXT
);
