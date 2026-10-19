-- Account board: a device's Claude-account group key, set by POST /v1/me/account.
-- The value is HMAC(ARENA_SECRET_KEY, client hash), never the client hash itself.
-- Additive only (sqlx checksums every applied file: never edit this once pushed).
ALTER TABLE devices ADD COLUMN acct_key VARCHAR(64) NULL;
CREATE INDEX IF NOT EXISTS ix_devices_acct_key ON devices (acct_key);
