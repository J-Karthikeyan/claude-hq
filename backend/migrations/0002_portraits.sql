-- 3D character portraits (HQ 2.4): one small PNG per user, rendered by their own
-- HQ from the character they built. While a row exists, users.avatar_url points at
-- /v1/portraits/<id>, so every list that shows an avatar shows the portrait; the
-- GitHub picture is kept here to put back when the portrait is removed.
CREATE TABLE IF NOT EXISTS portraits (
    user_id TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    png BLOB NOT NULL,
    version INTEGER NOT NULL,
    github_avatar_url TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL
);
