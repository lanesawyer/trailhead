-- What a person is told (docs/app-platform.md#notifications). `read_at` is the person having seen it; `resolved_at`
-- is the thing no longer needing them. `subject` is the raiser's name for what it is about, which it resolves by.
CREATE TABLE notifications (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    source TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    subject TEXT,
    task_id TEXT REFERENCES tasks(id),
    actions TEXT NOT NULL DEFAULT '[]',
    raised_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    read_at TEXT,
    resolved_at TEXT
);
CREATE INDEX notifications_open_task ON notifications (task_id) WHERE resolved_at IS NULL;
CREATE INDEX notifications_open_subject ON notifications (source, subject) WHERE resolved_at IS NULL;
