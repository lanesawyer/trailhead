CREATE TABLE tokens (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    principal_kind TEXT NOT NULL CHECK (principal_kind IN ('user', 'agent')),
    principal_id TEXT NOT NULL,
    name TEXT NOT NULL,
    secret_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    revoked_at TEXT
);
CREATE INDEX tokens_principal ON tokens (principal_id);
