-- What a person has set that every machine should agree on, one JSON value per name. The first is `notifications`
-- (docs/app-platform.md#notifications): levels, quiet hours, focus, the sound and the counts.
CREATE TABLE settings (
    workspace_id TEXT NOT NULL REFERENCES workspaces(id),
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (workspace_id, name)
);
