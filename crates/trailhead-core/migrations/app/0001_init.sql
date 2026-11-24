-- One app's data file: its records, their event log, and what the store keeps
-- about the file itself (its workspace, the declaration last registered).
CREATE TABLE meta (
    name TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE records (
    id TEXT PRIMARY KEY,
    collection TEXT NOT NULL,
    key TEXT,
    data TEXT NOT NULL,
    version INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    created_by TEXT NOT NULL
);
CREATE UNIQUE INDEX records_key ON records (collection, key) WHERE key IS NOT NULL;
CREATE INDEX records_collection ON records (collection, id);

CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    on_behalf_of TEXT NOT NULL,
    collection TEXT NOT NULL,
    record_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    before TEXT,
    after TEXT,
    undoes INTEGER,
    at TEXT NOT NULL
);
CREATE INDEX events_record ON events (record_id, id);
CREATE INDEX events_batch ON events (batch_id);
CREATE INDEX events_actor ON events (actor_id, at);
CREATE INDEX events_collection ON events (collection, id);
