-- JSON array of app ids whose data the token reaches. NULL: a person's token
-- not limited to projects reaches every app; any other token reaches none.
ALTER TABLE tokens ADD COLUMN apps TEXT;
