-- The day a task is planned to be worked, separate from its deadline. A plain
-- calendar date (YYYY-MM-DD) with no zone: "today" is whatever day it is for
-- the person planning.
ALTER TABLE tasks ADD COLUMN scheduled_on TEXT;
CREATE INDEX tasks_scheduled ON tasks (workspace_id, scheduled_on) WHERE deleted_at IS NULL AND scheduled_on IS NOT NULL;
