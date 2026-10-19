-- HQ 2.5 skill tree: per-UTC-day category counts a paired HQ reports (only
-- the seven integers per day, classified locally from the user's transcripts),
-- and the one title each user chose to wear. Additive only.
CREATE TABLE IF NOT EXISTS skill_days (user_id VARCHAR(36) NOT NULL, day VARCHAR(10) NOT NULL, cat VARCHAR(12) NOT NULL, n INTEGER NOT NULL, PRIMARY KEY(user_id, day, cat));
CREATE TABLE IF NOT EXISTS skill_titles (user_id VARCHAR(36) PRIMARY KEY, title VARCHAR(24) NOT NULL, at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP);
