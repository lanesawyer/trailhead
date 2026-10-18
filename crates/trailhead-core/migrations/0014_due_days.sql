-- Deadlines become a day with an optional time, like scheduling. `due_on` is the
-- day; `due_at` stays the optional time. `due_by` is when the deadline passes
-- (`due_at`, else the end of `due_on` in `due_tz`), kept for reminders, sorting
-- and `due_before`. Existing rows are filled in by `backfill_due_days` in db.rs,
-- since placing a time on a day needs time zone rules.
ALTER TABLE tasks ADD COLUMN due_on TEXT;
ALTER TABLE tasks ADD COLUMN due_by TEXT;
CREATE INDEX tasks_due ON tasks (workspace_id, due_by) WHERE deleted_at IS NULL AND due_by IS NOT NULL;
