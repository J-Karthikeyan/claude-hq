-- World boss (HQ 2.5): one row per ISO week, created lazily on first read, plus
-- one row per fight. HP is never stored: it is computed on read from
-- work_events, game_results and boss_fights.
CREATE TABLE IF NOT EXISTS boss_weeks (week VARCHAR(8) PRIMARY KEY, boss_dex INTEGER NOT NULL, max_hp INTEGER NOT NULL, created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, defeated_at DATETIME NULL, final_blow_user VARCHAR(36) NULL);
CREATE TABLE IF NOT EXISTS boss_fights (id INTEGER PRIMARY KEY AUTOINCREMENT, week VARCHAR(8) NOT NULL, user_id VARCHAR(36) NOT NULL, day VARCHAR(10) NOT NULL, dmg INTEGER NOT NULL, request_id VARCHAR(64) NOT NULL, at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE(user_id, request_id));
CREATE INDEX IF NOT EXISTS ix_boss_fights_week ON boss_fights(week, user_id);
