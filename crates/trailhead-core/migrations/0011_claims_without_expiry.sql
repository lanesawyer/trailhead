-- Claims held by people don't expire, so expires_at becomes nullable.
CREATE TABLE claims_new (
    task_id TEXT PRIMARY KEY REFERENCES tasks(id),
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    holder_id TEXT NOT NULL,
    on_behalf_of TEXT NOT NULL,
    claimed_at TEXT NOT NULL,
    expires_at TEXT
);
INSERT INTO claims_new SELECT task_id, workspace_id, holder_id, on_behalf_of, claimed_at, expires_at FROM claims;
DROP TABLE claims;
ALTER TABLE claims_new RENAME TO claims;
CREATE INDEX claims_expiry ON claims (expires_at);
