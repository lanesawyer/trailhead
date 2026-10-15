-- Events older than the 90-day undo window move here, keeping their ids.
-- Archiving moves a prefix of `events` by id, so no index on `at` is needed.
CREATE TABLE events_archive (
    id INTEGER PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    batch_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    on_behalf_of TEXT NOT NULL,
    task_id TEXT,
    entity TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    before TEXT,
    after TEXT,
    undoes INTEGER,
    at TEXT NOT NULL
);
CREATE INDEX events_archive_workspace ON events_archive (workspace_id, id);
CREATE INDEX events_archive_batch ON events_archive (batch_id);
CREATE INDEX events_archive_task ON events_archive (task_id, id);
