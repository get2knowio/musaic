CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    principal_id TEXT NOT NULL REFERENCES agent_pairings(id),
    library_epoch TEXT NOT NULL,
    tool TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    command TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'awaiting_approval', 'running', 'succeeded', 'failed', 'cancelled', 'denied', 'expired'
    )),
    cancellable INTEGER NOT NULL CHECK (cancellable IN (0, 1)),
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    approval_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    finished_at INTEGER,
    counts TEXT NOT NULL DEFAULT '{"done":0,"total":0,"failed":0}',
    checkpoint TEXT,
    result TEXT,
    error TEXT,
    execution_failed INTEGER NOT NULL DEFAULT 0 CHECK (execution_failed IN (0, 1)),
    UNIQUE (library_epoch, principal_id, tool, idempotency_key),
    CHECK ((finished_at IS NULL) = (state IN ('awaiting_approval', 'running')))
);

CREATE INDEX operations_active ON operations (library_epoch, state)
    WHERE finished_at IS NULL;

CREATE TABLE mcp_tasks (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE REFERENCES operations(id),
    principal_id TEXT NOT NULL REFERENCES agent_pairings(id),
    profile TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    expired INTEGER NOT NULL DEFAULT 0 CHECK (expired IN (0, 1)),
    CHECK (expired = 0 OR expires_at IS NOT NULL)
);

CREATE INDEX mcp_tasks_retention ON mcp_tasks (expires_at)
    WHERE expires_at IS NOT NULL AND expired = 0;

CREATE TABLE mcp_task_inputs (
    task_id TEXT NOT NULL REFERENCES mcp_tasks(id),
    input_key TEXT NOT NULL,
    request TEXT NOT NULL,
    response_digest TEXT,
    PRIMARY KEY (task_id, input_key)
);
