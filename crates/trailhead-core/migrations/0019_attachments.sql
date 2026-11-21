-- Picture bytes, stored once per distinct content however many attachments name them. Attachments are only ever
-- soft-deleted, so a blob is never orphaned while its undo is possible.
CREATE TABLE blobs (
    sha256 TEXT PRIMARY KEY,
    size INTEGER NOT NULL,
    bytes BLOB NOT NULL,
    created_at TEXT NOT NULL
);

-- Pictures on a task, metadata only, so listing them never reads the bytes.
CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    task_id TEXT NOT NULL REFERENCES tasks(id),
    name TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha256 TEXT NOT NULL REFERENCES blobs(sha256),
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    role TEXT,
    set_name TEXT,
    created_by TEXT NOT NULL,
    on_behalf_of TEXT NOT NULL,
    created_at TEXT NOT NULL,
    deleted_at TEXT
);
CREATE INDEX attachments_task ON attachments (task_id, created_at);
CREATE INDEX attachments_sha256 ON attachments (sha256);
