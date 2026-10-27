-- Typed review verdicts: a `review` comment carries approve or request_changes,
-- and optionally the model that reviewed, so gates count records rather than prose.
ALTER TABLE comments ADD COLUMN verdict TEXT;
ALTER TABLE comments ADD COLUMN model TEXT;
