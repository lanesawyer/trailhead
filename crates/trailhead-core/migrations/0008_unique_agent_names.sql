UPDATE agents SET name = name || '-' || substr(id, -6)
WHERE rowid NOT IN (SELECT min(rowid) FROM agents GROUP BY workspace_id, name);
CREATE UNIQUE INDEX agents_name ON agents (workspace_id, name);
