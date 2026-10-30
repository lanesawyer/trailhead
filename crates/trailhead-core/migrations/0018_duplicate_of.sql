-- The task a cancelled task duplicates. NULL for every other task.
ALTER TABLE tasks ADD COLUMN duplicate_of TEXT REFERENCES tasks (id);
CREATE INDEX tasks_duplicate_of ON tasks (duplicate_of) WHERE duplicate_of IS NOT NULL;
