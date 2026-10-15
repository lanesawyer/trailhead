-- One row per (workspace, actor, key). `status` is NULL while the first request
-- is still running; afterwards the row holds the response to replay.
CREATE TABLE idempotency_keys (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    actor_id TEXT NOT NULL,
    key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    status INTEGER,
    headers TEXT,
    body BLOB,
    created_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, actor_id, key)
);
CREATE INDEX idempotency_keys_created ON idempotency_keys (created_at);
