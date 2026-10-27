-- When a token stops working; NULL means never. Short-lived sub-agent tokens
-- clean themselves up if revoking them is missed.
ALTER TABLE tokens ADD COLUMN expires_at TEXT;
