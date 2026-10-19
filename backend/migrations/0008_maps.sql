-- HQ 2.5 map gallery: user-made kart tracks, platformer levels and blaster maps,
-- shared to a room or published to the Arena, with likes, reports and a weekly
-- featured pick. Additive only (CREATE ... IF NOT EXISTS).
CREATE TABLE IF NOT EXISTS user_maps (id VARCHAR(16) PRIMARY KEY, owner_id VARCHAR(36) NOT NULL, kind VARCHAR(8) NOT NULL, name VARCHAR(32) NOT NULL, data TEXT NOT NULL, ckey VARCHAR(16) NOT NULL, scope VARCHAR(8) NOT NULL, room_id VARCHAR(64) NULL, likes INTEGER NOT NULL DEFAULT 0, reports INTEGER NOT NULL DEFAULT 0, hidden INTEGER NOT NULL DEFAULT 0, created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE INDEX IF NOT EXISTS ix_user_maps_pub ON user_maps(kind, scope, hidden, created_at);
CREATE INDEX IF NOT EXISTS ix_user_maps_owner ON user_maps(owner_id);
CREATE INDEX IF NOT EXISTS ix_user_maps_ckey ON user_maps(ckey);
CREATE TABLE IF NOT EXISTS user_map_likes (map_id VARCHAR(16) NOT NULL, user_id VARCHAR(36) NOT NULL, at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(map_id, user_id));
CREATE TABLE IF NOT EXISTS user_map_reports (map_id VARCHAR(16) NOT NULL, user_id VARCHAR(36) NOT NULL, reason VARCHAR(12) NOT NULL, at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(map_id, user_id));
CREATE TABLE IF NOT EXISTS map_publishes (user_id VARCHAR(36) NOT NULL, day VARCHAR(10) NOT NULL, n INTEGER NOT NULL, PRIMARY KEY(user_id, day));
CREATE TABLE IF NOT EXISTS map_features (week VARCHAR(8) NOT NULL, kind VARCHAR(8) NOT NULL, map_id VARCHAR(16) NOT NULL, score INTEGER NOT NULL, at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY(week, kind));
