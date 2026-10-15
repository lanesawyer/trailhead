CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    default_claim_ttl INTEGER NOT NULL DEFAULT 1800,
    created_at TEXT NOT NULL
);

CREATE TABLE users (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    name TEXT NOT NULL,
    email TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    owner_user_id TEXT NOT NULL REFERENCES users(id),
    name TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    key TEXT NOT NULL,
    name TEXT NOT NULL,
    brief TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'active',
    metadata TEXT NOT NULL DEFAULT '{}',
    claim_ttl INTEGER,
    require_review INTEGER NOT NULL DEFAULT 0,
    next_task_seq INTEGER NOT NULL DEFAULT 1,
    version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT,
    UNIQUE (workspace_id, key)
);

CREATE TABLE tasks (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    key TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'open',
    priority INTEGER NOT NULL DEFAULT 3,
    due_at TEXT,
    due_tz TEXT,
    parent_id TEXT REFERENCES tasks(id),
    metadata TEXT NOT NULL DEFAULT '{}',
    version INTEGER NOT NULL DEFAULT 1,
    source TEXT,
    external_id TEXT,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT,
    UNIQUE (workspace_id, key)
);
CREATE UNIQUE INDEX tasks_external ON tasks (workspace_id, source, external_id) WHERE source IS NOT NULL;
CREATE INDEX tasks_ready ON tasks (workspace_id, status, priority, due_at) WHERE deleted_at IS NULL;
CREATE INDEX tasks_project ON tasks (project_id);
CREATE INDEX tasks_parent ON tasks (parent_id);

CREATE TABLE dependencies (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    blocker_id TEXT NOT NULL REFERENCES tasks(id),
    blocked_id TEXT NOT NULL REFERENCES tasks(id),
    created_at TEXT NOT NULL,
    PRIMARY KEY (blocker_id, blocked_id),
    CHECK (blocker_id <> blocked_id)
);
CREATE INDEX dependencies_blocked ON dependencies (blocked_id);

CREATE TABLE claims (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    holder_id TEXT NOT NULL,
    on_behalf_of TEXT NOT NULL,
    claimed_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX claims_expiry ON claims (expires_at);

CREATE TABLE comments (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    task_id TEXT NOT NULL REFERENCES tasks(id),
    author_id TEXT NOT NULL,
    on_behalf_of TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'note',
    addressed_to TEXT,
    body TEXT NOT NULL,
    resolved_at TEXT,
    created_at TEXT NOT NULL,
    deleted_at TEXT
);
CREATE INDEX comments_task ON comments (task_id, created_at);

CREATE TABLE events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
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
CREATE INDEX events_workspace ON events (workspace_id, id);
CREATE INDEX events_batch ON events (batch_id);
CREATE INDEX events_entity ON events (entity, entity_id, id);
CREATE INDEX events_actor ON events (actor_id, at);
CREATE INDEX events_task ON events (task_id, id);

CREATE VIRTUAL TABLE tasks_fts USING fts5(title, description, content='tasks', content_rowid='rowid');
CREATE TRIGGER tasks_fts_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts (rowid, title, description) VALUES (new.rowid, new.title, new.description);
END;
CREATE TRIGGER tasks_fts_ad AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts (tasks_fts, rowid, title, description) VALUES ('delete', old.rowid, old.title, old.description);
END;
CREATE TRIGGER tasks_fts_au AFTER UPDATE OF title, description ON tasks BEGIN
    INSERT INTO tasks_fts (tasks_fts, rowid, title, description) VALUES ('delete', old.rowid, old.title, old.description);
    INSERT INTO tasks_fts (rowid, title, description) VALUES (new.rowid, new.title, new.description);
END;

CREATE VIRTUAL TABLE comments_fts USING fts5(body, content='comments', content_rowid='rowid');
CREATE TRIGGER comments_fts_ai AFTER INSERT ON comments BEGIN
    INSERT INTO comments_fts (rowid, body) VALUES (new.rowid, new.body);
END;
CREATE TRIGGER comments_fts_ad AFTER DELETE ON comments BEGIN
    INSERT INTO comments_fts (comments_fts, rowid, body) VALUES ('delete', old.rowid, old.body);
END;
CREATE TRIGGER comments_fts_au AFTER UPDATE OF body ON comments BEGIN
    INSERT INTO comments_fts (comments_fts, rowid, body) VALUES ('delete', old.rowid, old.body);
    INSERT INTO comments_fts (rowid, body) VALUES (new.rowid, new.body);
END;
