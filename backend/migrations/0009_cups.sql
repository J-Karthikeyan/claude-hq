-- HQ 2.5 tournaments: weekly cups per game, frozen standings and trophies.
-- Additive only (sqlx checksums every applied file: never edit this once pushed).
CREATE TABLE IF NOT EXISTS cups (id VARCHAR(32) PRIMARY KEY, week VARCHAR(8) NOT NULL, game VARCHAR(8) NOT NULL, key VARCHAR(40) NOT NULL, format VARCHAR(8) NOT NULL DEFAULT 'points', starts_at DATETIME NOT NULL, ends_at DATETIME NOT NULL, status VARCHAR(8) NOT NULL DEFAULT 'final', final_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS cup_standings (cup_id VARCHAR(32) NOT NULL, user_id VARCHAR(36) NOT NULL, points INTEGER NOT NULL, races INTEGER NOT NULL, place INTEGER NOT NULL, PRIMARY KEY(cup_id, user_id));
CREATE TABLE IF NOT EXISTS cup_trophies (user_id VARCHAR(36) NOT NULL, cup_id VARCHAR(32) NOT NULL, place INTEGER NOT NULL, label VARCHAR(48) NOT NULL, season VARCHAR(7) NOT NULL, at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(user_id, cup_id));
