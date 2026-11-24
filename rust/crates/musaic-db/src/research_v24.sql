CREATE TABLE IF NOT EXISTS research_history (
    id TEXT PRIMARY KEY,
    album_id TEXT NOT NULL,
    album_path TEXT NOT NULL,
    album_title TEXT,
    job_type TEXT NOT NULL,
    hint TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL,
    acted_on_action TEXT,
    error TEXT,
    result_json TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    acted_on_at TEXT,
    attach_cover INTEGER NOT NULL DEFAULT 1,
    migrated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS research_history_album ON research_history(album_id);
CREATE INDEX IF NOT EXISTS research_history_created ON research_history(created_at, id);

CREATE TABLE IF NOT EXISTS work_batches (
    id TEXT PRIMARY KEY,
    library_epoch TEXT NOT NULL,
    check_id TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS work_batches_epoch ON work_batches(library_epoch, started_at);

ALTER TABLE delegated_work ADD COLUMN batch_id TEXT REFERENCES work_batches(id);
CREATE INDEX IF NOT EXISTS delegated_work_batch ON delegated_work(batch_id, state, id)
    WHERE batch_id IS NOT NULL;

ALTER TABLE delegation_events ADD COLUMN batch_id TEXT;

CREATE TABLE work_replays_v24 (
    library_epoch TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('claim','complete','claim_many')),
    replay_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    response_json TEXT NOT NULL CHECK (json_valid(response_json)),
    PRIMARY KEY (library_epoch, principal_id, operation, replay_key)
);
INSERT INTO work_replays_v24 SELECT * FROM work_replays;
DROP TABLE work_replays;
ALTER TABLE work_replays_v24 RENAME TO work_replays;
