-- The reminders that have fired, one row per task and kind. `due_at` is the due
-- date the reminder was for, so moving the due date lets it fire again.
CREATE TABLE reminders_sent (
    task_id TEXT NOT NULL REFERENCES tasks(id),
    kind TEXT NOT NULL,
    due_at TEXT NOT NULL,
    event_id INTEGER NOT NULL,
    sent_at TEXT NOT NULL,
    PRIMARY KEY (task_id, kind)
);
