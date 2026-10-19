-- Loot from real work (HQ 2.5): a merged PR, a green test run or a long focus
-- session, detected on the user's own machine, drops a chest. Only the event TYPE,
-- a small count and the UTC day reach the Arena (work_events, from 0003). A chest
-- opens once into coins, a grant-only cosmetic or a Pokemon card; `reward` is the
-- journal of record for what it gave (loot coins bypass poke_ledger by design).
CREATE TABLE IF NOT EXISTS loot_chests (id VARCHAR(40) PRIMARY KEY, user_id VARCHAR(36) NOT NULL, source VARCHAR(16) NOT NULL, rarity VARCHAR(8) NOT NULL, created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP, opened_at DATETIME NULL, reward JSON NULL);
CREATE INDEX IF NOT EXISTS ix_loot_chests_user ON loot_chests(user_id, opened_at);
