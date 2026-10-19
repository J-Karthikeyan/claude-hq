# Arena backend

The Claude HQ Arena server (Rust: axum + sqlx + SQLite). It began as a port of the
Python/FastAPI backend, ran next to it for a while, and replaced it; the Python one is
gone.

```bash
cargo run --bin arena                        # http://127.0.0.1:8080 (see .env.example)
cargo test --locked && cargo clippy --locked --all-targets -- -D warnings
```

- `src/`: the server; `migrations/`: the schema, applied at startup (`sqlx::migrate!`).
- `data/`: game data embedded at build time with `include_str!` (kart tracks,
  platformer levels, Blaster and Strikezone maps, golf courses, Pokemon). Each one is a
  byte copy of the browser's file under `games/`, checked by `tests/test_game_data_sync.py`.
- `Dockerfile` builds from this folder alone; `docker-compose.yml` + `Caddyfile` run it
  on a VPS behind Caddy (`../deploy-vps.sh` first, then `../ops/release.sh deploy`).

## Where Rust improves on the original

**The tool allowlist is a type.** In Python, `mcp__acme_internal__query`
arrives as a string that a validator must remember to bucket. Here it
deserialises to `Tool::Other` and the original text is dropped — there is no
representable value that carries an MCP server name, so no code path can store
one.

**`deny_unknown_fields` is generated code**, not a runtime setting, so a client
that grows a field fails to parse rather than relying on config being right.

**`Option<f64>` for cost** makes "did not share" and "spent nothing" different
types rather than a nullable column you must remember to check.

## Status

| | |
|---|---|
| `scoring.rs` | done, cross-checked against Python |
| `schemas.rs` | done, privacy tests ported |
| `db.rs` | next — sqlx against the existing schema |
| `service.rs` | ingest + board queries |
| `auth.rs` | device tokens, OAuth, WS tickets |
| `rooms.rs` | websocket rooms; room-wide broadcast plus a bounded direct queue per connection (one socket / one user / a lobby) |
| `realtime.rs` | done: fixed-rate per-room tick (tokio), process-wide ticker budget, per-room bytes/s budget with snapshot thinning, overrun counting, token `Bucket` — port of `app/realtime.py` |
| `kart.rs` | done: Kart Racing — track compile/geometry, referee (grid → race → done, laps, sender-clock speed check, fixes, drop/restore), the kart lobby and its tick loop — port of `app/kart.py` + the kart branch of `app/valley.py`. Kart and the platformer are the only Valley games here; any other `g` gets `unknown game` |
| `platformer.rs` | done: Platformer Rush — level compile/geometry, referee (grid → run → done; race and co-op; sender-clock speed, rise/fall and jump-envelope checks, never inside a platform, checkpoints in order, coins once per player / once per room, the flag, fixes, respawns, drop/restore), the plat lobby and its tick loop (sharing the process-wide `realtime::Registry` with kart) — port of `app/platformer.py` + the plat branch of `app/valley.py` |

Kart tracks are read from `backend/data/kart_tracks.json` with `include_str!`
at build time, so both backends race on the same file; a test checks the
embedded copy against it.
Platformer levels come from `backend/data/platformer_levels.json` the same way.

```bash
cargo test    # 97 tests
```
