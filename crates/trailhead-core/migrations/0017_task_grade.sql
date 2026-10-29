-- An optional risk grade, 1 to 4, set by a human and only raised by agents.
-- NULL means ungraded.
ALTER TABLE tasks ADD COLUMN grade INTEGER CHECK (grade IS NULL OR grade BETWEEN 1 AND 4);
CREATE INDEX tasks_grade ON tasks (workspace_id, grade) WHERE deleted_at IS NULL AND grade IS NOT NULL;
