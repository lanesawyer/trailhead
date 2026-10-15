CREATE TABLE webhooks (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    owner_user_id TEXT NOT NULL REFERENCES users(id),
    url TEXT NOT NULL,
    event_kinds TEXT NOT NULL DEFAULT '[]',
    secret TEXT NOT NULL,
    cursor INTEGER NOT NULL,
    failure_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TEXT,
    created_at TEXT NOT NULL,
    disabled_at TEXT
);
CREATE INDEX webhooks_workspace ON webhooks (workspace_id);

CREATE TABLE webhook_deliveries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    webhook_id TEXT NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    event_id INTEGER,
    status_code INTEGER,
    error TEXT,
    at TEXT NOT NULL
);
CREATE INDEX webhook_deliveries_webhook ON webhook_deliveries (webhook_id, id);
