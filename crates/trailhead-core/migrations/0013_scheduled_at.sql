-- An optional time on a task's scheduled day ("today at 1pm"): the instant in
-- UTC plus the IANA zone it was planned in. `scheduled_on` stays the day.
ALTER TABLE tasks ADD COLUMN scheduled_at TEXT;
ALTER TABLE tasks ADD COLUMN scheduled_tz TEXT;
