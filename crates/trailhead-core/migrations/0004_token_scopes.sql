-- JSON array of project ids the token may touch; NULL means every project.
ALTER TABLE tokens ADD COLUMN projects TEXT;
-- Requests per minute; NULL means unlimited.
ALTER TABLE tokens ADD COLUMN rate_limit_per_minute INTEGER;
