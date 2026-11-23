CREATE TABLE delegation_events (
    sequence INTEGER PRIMARY KEY,
    library_epoch TEXT NOT NULL,
    work_id TEXT NOT NULL,
    event_type TEXT NOT NULL CHECK (event_type IN ('requested','updated')),
    kind TEXT NOT NULL,
    state TEXT NOT NULL,
    revision INTEGER NOT NULL,
    committed_at INTEGER NOT NULL
);
CREATE INDEX delegation_events_epoch ON delegation_events(library_epoch, sequence);
CREATE INDEX delegation_events_age ON delegation_events(committed_at);
CREATE INDEX delegation_events_work ON delegation_events(work_id, sequence);

CREATE TABLE delegation_event_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    pruned_through INTEGER NOT NULL DEFAULT 0
);
INSERT INTO delegation_event_state(singleton, pruned_through) VALUES (1, 0);

CREATE TABLE agent_host_observations (
    pairing_id TEXT PRIMARY KEY REFERENCES agent_pairings(id) ON DELETE CASCADE,
    client_name TEXT,
    client_version TEXT,
    profile TEXT,
    tasks INTEGER NOT NULL DEFAULT 0 CHECK (tasks IN (0,1)),
    skills INTEGER NOT NULL DEFAULT 0 CHECK (skills IN (0,1)),
    events INTEGER NOT NULL DEFAULT 0 CHECK (events IN (0,1)),
    observed_at INTEGER NOT NULL
);

ALTER TABLE work_attempts ADD COLUMN last_heartbeat_at INTEGER;
