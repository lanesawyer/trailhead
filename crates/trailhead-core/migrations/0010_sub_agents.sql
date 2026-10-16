ALTER TABLE agents ADD COLUMN parent_agent_id TEXT REFERENCES agents(id);
ALTER TABLE agents ADD COLUMN revoked_at TEXT;
CREATE INDEX agents_parent ON agents (parent_agent_id);
