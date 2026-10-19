-- Baseline schema for the Rust Arena.
--
-- Generated from the Python backend at Alembic revision 3d4e5f6a7b8c, which is
-- where schema ownership transfers. Everything after this point is a numbered
-- sqlx migration in this directory; Alembic is no longer consulted.
--
-- Written with IF NOT EXISTS throughout so it is a no-op against a database
-- Alembic already built. That is what makes the cutover reversible: run the
-- Rust server against the existing production file and nothing is recreated.


CREATE TABLE IF NOT EXISTS crew_members (
	user_id VARCHAR(36) NOT NULL, 
	crew_id VARCHAR(36) NOT NULL, 
	joined_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (user_id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE, 
	FOREIGN KEY(crew_id) REFERENCES crews (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS crews (
	id VARCHAR(36) NOT NULL, 
	name VARCHAR(32) NOT NULL, 
	tag VARCHAR(4) NOT NULL, 
	color VARCHAR(7) NOT NULL, 
	code VARCHAR(12) NOT NULL, 
	owner_id VARCHAR(36) NOT NULL, 
	created_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(owner_id) REFERENCES users (id) ON DELETE CASCADE, 
	UNIQUE (name), 
	UNIQUE (code)
);

CREATE TABLE IF NOT EXISTS daily_stats (
	id VARCHAR(36) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	stat_date DATE NOT NULL, 
	prompts INTEGER NOT NULL, 
	tools INTEGER NOT NULL, 
	artifacts INTEGER NOT NULL, 
	replies INTEGER NOT NULL, 
	tokens_input BIGINT NOT NULL, 
	tokens_output BIGINT NOT NULL, 
	tokens_cache_read BIGINT NOT NULL, 
	tokens_cache_creation BIGINT NOT NULL, 
	cost_usd NUMERIC(10, 4), 
	updated_at DATETIME DEFAULT (CURRENT_TIMESTAMP) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE, 
	CONSTRAINT uq_daily_user_date UNIQUE (user_id, stat_date)
);

CREATE TABLE IF NOT EXISTS daily_tool_stats (
	id VARCHAR(36) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	stat_date DATE NOT NULL, 
	tool_name VARCHAR(48) NOT NULL, 
	count INTEGER NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE, 
	CONSTRAINT uq_tool_user_date_name UNIQUE (user_id, stat_date, tool_name)
);

CREATE TABLE IF NOT EXISTS devices (
	id VARCHAR(36) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	token_hash VARCHAR(64) NOT NULL, 
	label VARCHAR(64) NOT NULL, 
	created_at DATETIME DEFAULT (CURRENT_TIMESTAMP) NOT NULL, 
	last_seen_at DATETIME, 
	revoked BOOLEAN NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS equipped_cosmetics (
	user_id VARCHAR(36) NOT NULL, 
	slots JSON NOT NULL, 
	updated_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (user_id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS game_results (
	id INTEGER NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	game VARCHAR(8) NOT NULL, 
	"key" VARCHAR(40) NOT NULL, 
	mode VARCHAR(16) DEFAULT '' NOT NULL, 
	place INTEGER NOT NULL, 
	players INTEGER NOT NULL, 
	value INTEGER, 
	extra JSON NOT NULL, 
	at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS hq_profiles (
	user_id VARCHAR(36) NOT NULL, 
	open BOOLEAN DEFAULT 0 NOT NULL, 
	look JSON NOT NULL, 
	crew JSON NOT NULL, 
	updated_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (user_id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS nudges (
	id VARCHAR(36) NOT NULL, 
	from_user_id VARCHAR(36) NOT NULL, 
	to_user_id VARCHAR(36) NOT NULL, 
	note VARCHAR(120) NOT NULL, 
	created_at DATETIME DEFAULT (CURRENT_TIMESTAMP) NOT NULL, 
	delivered_at DATETIME, 
	PRIMARY KEY (id), 
	FOREIGN KEY(from_user_id) REFERENCES users (id) ON DELETE CASCADE, 
	FOREIGN KEY(to_user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS pair_codes (
	code VARCHAR(32) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	expires_at DATETIME NOT NULL, 
	used_at DATETIME, 
	PRIMARY KEY (code), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS poke_balances (
	id VARCHAR(36) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	item VARCHAR(16) NOT NULL, 
	qty INTEGER DEFAULT '0' NOT NULL, 
	updated_at DATETIME DEFAULT (CURRENT_TIMESTAMP) NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE, 
	CONSTRAINT uq_poke_balances_user_item UNIQUE (user_id, item), 
	CONSTRAINT ck_poke_balances_qty_nonneg CHECK (qty >= 0)
);

CREATE TABLE IF NOT EXISTS "poke_ledger" (
                id          VARCHAR(36) NOT NULL PRIMARY KEY,
                user_id     VARCHAR(36) NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                request_id  VARCHAR(100) NOT NULL,
                op          VARCHAR(10) NOT NULL,
                op_date     DATE NOT NULL,
                kind        VARCHAR(20),
                qty         INTEGER NOT NULL DEFAULT 0 CHECK (qty >= 0),
                coins       INTEGER NOT NULL DEFAULT 0 CHECK (coins >= 0),
                to_user_id  VARCHAR(36) REFERENCES users(id) ON DELETE SET NULL,
                note        VARCHAR(80) NOT NULL DEFAULT '',
                created_at  DATETIME NOT NULL,
                delivered_at DATETIME,
                UNIQUE (user_id, request_id),
                CHECK (op IN ('claim','buy','eat','give','quest','cosmetic','sell'))
            );

CREATE TABLE IF NOT EXISTS private_room_join_attempts (
	id VARCHAR(36) NOT NULL, 
	room_id VARCHAR(24) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	created_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(room_id) REFERENCES private_rooms (id) ON DELETE CASCADE, 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS private_room_members (
	id VARCHAR(36) NOT NULL, 
	room_id VARCHAR(24) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	role VARCHAR(8) DEFAULT 'member' NOT NULL, 
	joined_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (id), 
	CONSTRAINT uq_private_room_member UNIQUE (room_id, user_id), 
	CONSTRAINT ck_private_room_member_role CHECK (role IN ('owner','member','banned')), 
	FOREIGN KEY(room_id) REFERENCES private_rooms (id) ON DELETE CASCADE, 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS private_rooms (
	id VARCHAR(24) NOT NULL, 
	name VARCHAR(40) NOT NULL, 
	name_key VARCHAR(255) NOT NULL, 
	owner_user_id VARCHAR(36) NOT NULL, 
	password_hash VARCHAR(200) NOT NULL, 
	created_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	updated_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (id), 
	CONSTRAINT uq_private_rooms_name_key UNIQUE (name_key), 
	FOREIGN KEY(owner_user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS room_farms (
	room_id VARCHAR(64) NOT NULL, 
	data JSON NOT NULL, 
	updated_at DATETIME DEFAULT CURRENT_TIMESTAMP NOT NULL, 
	PRIMARY KEY (room_id)
);

CREATE TABLE IF NOT EXISTS seasons (
	id VARCHAR(36) NOT NULL, 
	slug VARCHAR(64) NOT NULL, 
	name VARCHAR(128) NOT NULL, 
	starts_on DATE NOT NULL, 
	ends_on DATE NOT NULL, 
	is_active BOOLEAN NOT NULL, 
	PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS stat_snapshots (
	id VARCHAR(36) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	device_id VARCHAR(36), 
	received_at DATETIME DEFAULT (CURRENT_TIMESTAMP) NOT NULL, 
	payload JSON NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS taco_diners (
	id VARCHAR(36) NOT NULL, 
	order_id VARCHAR(36) NOT NULL, 
	user_id VARCHAR(36), 
	diner_name VARCHAR(40) DEFAULT '' NOT NULL, 
	mild_hard INTEGER DEFAULT '0' NOT NULL, 
	mild_soft INTEGER DEFAULT '0' NOT NULL, 
	wild_hard INTEGER DEFAULT '0' NOT NULL, 
	wild_soft INTEGER DEFAULT '0' NOT NULL, items JSON DEFAULT '{}' NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(order_id) REFERENCES taco_orders (id) ON DELETE CASCADE, 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE SET NULL, 
	CONSTRAINT ck_taco_diners_identity CHECK (user_id IS NOT NULL OR diner_name <> ''), 
	CONSTRAINT ck_taco_diners_counts_nonneg CHECK (mild_hard >= 0 AND mild_soft >= 0 AND wild_hard >= 0 AND wild_soft >= 0)
);

CREATE TABLE IF NOT EXISTS taco_orders (
	id VARCHAR(36) NOT NULL, 
	user_id VARCHAR(36) NOT NULL, 
	request_id VARCHAR(64) NOT NULL, 
	order_date DATE NOT NULL, 
	total_tacos INTEGER DEFAULT '0' NOT NULL, 
	paid_tacos INTEGER DEFAULT '0' NOT NULL, 
	note VARCHAR(80) DEFAULT '' NOT NULL, 
	created_at DATETIME NOT NULL, 
	PRIMARY KEY (id), 
	FOREIGN KEY(user_id) REFERENCES users (id) ON DELETE CASCADE, 
	CONSTRAINT uq_taco_orders_user_request UNIQUE (user_id, request_id), 
	CONSTRAINT ck_taco_orders_total_nonneg CHECK (total_tacos >= 0), 
	CONSTRAINT ck_taco_orders_paid_nonneg CHECK (paid_tacos >= 0), 
	CONSTRAINT ck_taco_orders_paid_le_total CHECK (paid_tacos <= total_tacos)
);

CREATE TABLE IF NOT EXISTS users (
	id VARCHAR(36) NOT NULL, 
	github_id BIGINT NOT NULL, 
	handle VARCHAR(64) NOT NULL, 
	display_name VARCHAR(128) NOT NULL, 
	avatar_url VARCHAR(512) NOT NULL, 
	trainer_name VARCHAR(32) NOT NULL, 
	is_active BOOLEAN NOT NULL, 
	created_at DATETIME DEFAULT (CURRENT_TIMESTAMP) NOT NULL, 
	PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS ix_crew_members_crew_id ON crew_members (crew_id);

CREATE INDEX IF NOT EXISTS ix_daily_date ON daily_stats (stat_date);

CREATE INDEX IF NOT EXISTS ix_daily_stats_user_id ON daily_stats (user_id);

CREATE INDEX IF NOT EXISTS ix_daily_tool_stats_user_id ON daily_tool_stats (user_id);

CREATE UNIQUE INDEX IF NOT EXISTS ix_devices_token_hash ON devices (token_hash);

CREATE INDEX IF NOT EXISTS ix_devices_user_id ON devices (user_id);

CREATE INDEX IF NOT EXISTS ix_game_results_board ON game_results (game, "key", value);

CREATE INDEX IF NOT EXISTS ix_game_results_user ON game_results (user_id, at);

CREATE INDEX IF NOT EXISTS ix_nudges_from_user_id ON nudges (from_user_id);

CREATE INDEX IF NOT EXISTS ix_nudges_to_undelivered ON nudges (to_user_id, delivered_at);

CREATE INDEX IF NOT EXISTS ix_nudges_to_user_id ON nudges (to_user_id);

CREATE INDEX IF NOT EXISTS ix_pair_codes_user_id ON pair_codes (user_id);

CREATE INDEX IF NOT EXISTS ix_poke_ledger_to_date ON poke_ledger (to_user_id, op_date);

CREATE INDEX IF NOT EXISTS ix_poke_ledger_to_undelivered ON poke_ledger (to_user_id, delivered_at);

CREATE INDEX IF NOT EXISTS ix_poke_ledger_user_date ON poke_ledger (user_id, op_date);

CREATE INDEX IF NOT EXISTS ix_private_room_attempts_room_at ON private_room_join_attempts (room_id, created_at);

CREATE INDEX IF NOT EXISTS ix_private_room_attempts_user_at ON private_room_join_attempts (user_id, created_at);

CREATE INDEX IF NOT EXISTS ix_private_room_attempts_user_room_at ON private_room_join_attempts (user_id, room_id, created_at);

CREATE INDEX IF NOT EXISTS ix_private_room_members_room_id ON private_room_members (room_id);

CREATE INDEX IF NOT EXISTS ix_private_room_members_user_id ON private_room_members (user_id);

CREATE INDEX IF NOT EXISTS ix_private_rooms_owner_user_id ON private_rooms (owner_user_id);

CREATE UNIQUE INDEX IF NOT EXISTS ix_seasons_slug ON seasons (slug);

CREATE INDEX IF NOT EXISTS ix_stat_snapshots_received_at ON stat_snapshots (received_at);

CREATE INDEX IF NOT EXISTS ix_stat_snapshots_user_id ON stat_snapshots (user_id);

CREATE INDEX IF NOT EXISTS ix_taco_diners_order_id ON taco_diners (order_id);

CREATE INDEX IF NOT EXISTS ix_taco_diners_user ON taco_diners (user_id);

CREATE INDEX IF NOT EXISTS ix_taco_orders_date ON taco_orders (order_date);

CREATE INDEX IF NOT EXISTS ix_taco_orders_user_id ON taco_orders (user_id);

CREATE UNIQUE INDEX IF NOT EXISTS ix_users_github_id ON users (github_id);

CREATE UNIQUE INDEX IF NOT EXISTS ix_users_handle ON users (handle);
