-- Arena City skatepark: each player's best combo per ISO week (UTC).
-- Additive only (sqlx checksums every applied file: never edit this once pushed).
CREATE TABLE IF NOT EXISTS skate_best (week VARCHAR(8) NOT NULL, user_id VARCHAR(36) NOT NULL, total INTEGER NOT NULL, tricks INTEGER NOT NULL, at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, last_post_ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(week, user_id));
CREATE INDEX IF NOT EXISTS skate_best_week_total ON skate_best (week, total DESC);
