# dashboard-rs (`claudehq`)

A Rust port of the local Claude HQ server: `dashboard.py` and everything it
imports (`arena.py`, `music.py`, `worksignals.py`, `ext_skills.py`,
`ext_loot.py`, `ext_prestige.py`, `ext_maps.py`, `ext_cups.py`,
`ext_boss.py`). Same routes, same gates, same JSON, same state files, same
Arena privacy boundary. The frontend (`index.html`, `ui/`, `games/`) is
unchanged and served from the repo as before.

It runs **alongside** the Python server, the same way `backend-rs/` did for the
Arena: the Python files are untouched, both read and write the same state
files, and you can switch back and forth with no migration.

## Build and run

```bash
~/.cargo/bin/cargo build --release --manifest-path dashboard-rs/Cargo.toml
dashboard-rs/target/release/claudehq --no-open --port 8766   # try it next to the Python one on 8765
```

The CLI matches `dashboard.py`:

| flag | |
|---|---|
| `--port N` | port on 127.0.0.1 (default 8765) |
| `--no-open` | don't open a browser |
| `--install` / `--uninstall` | write / remove the launchd LaunchAgent (`com.claudehq.dashboard`), pointing at this binary |
| `--print-plist` | print that plist and exit |
| `--root DIR` | *(new)* the Claude HQ folder. Defaults to `$CLAUDEHQ_ROOT`, else the folder above the binary that holds `index.html` + `ui/` (so `dashboard-rs/target/release/claudehq` finds the repo) |

To make the Rust server the one that starts at login (it uses the same label,
so it replaces the Python agent; `python3 dashboard.py --install` switches back):

```bash
dashboard-rs/target/release/claudehq --install
```

## Tests

```bash
cd dashboard-rs
~/.cargo/bin/cargo test                                  # 124 tests
~/.cargo/bin/cargo clippy --all-targets -- -D warnings   # clean
```

The unit tests port the behavioural Python suites: the cost model, config
validation, atomic state and quarantine, the meal ledger, the fatigue contract
vectors, needs-you detection, slim cards, the payload memo, flexible streaks,
Cursor transcripts and titles, work-signal classification, music parsing and
the share loop, the Arena wire allowlists (pantry, cali, rooms, portrait,
gifts, nudges), every ext proxy (skills, loot, prestige, maps, cups, boss),
the pantry eat flow, close/resume guards and the self-update (on throwaway git
repos). `tests/http.rs` checks the gates on a real 127.0.0.1 server, and
`tests/scanner.rs` the transcript pass, busy spans and work signals.

## Parity

`tools/parity.py` (stdlib only) starts `dashboard.py` and `claudehq` on two
ports (never 8765) against **one** temp copy of the Claude HQ folder: code
symlinked, state files copied, no `arena-link.json`, the Arena URL blanked and
work signals off, so nothing can pair or leave the machine. It GETs every
route on both (107 GET/HEAD checks), diffs status, headers and normalized
JSON, then replays 50 safe POSTs on each (resetting the temp state between
servers), diffs the state files they leave, and runs 9 checks on the POST
gates (CSRF, Origin, Host, body caps, bad bodies).

```bash
# frozen: copies ~/.claude/projects + ~/.cursor/projects into a temp HOME and stubs
# `claude agents` with a fixed list, so both servers see identical, still data
python3 dashboard-rs/tools/parity.py --state-from /path/to/claude-hq --frozen-home
# live: the real ~/.claude, real `claude agents` (expect drift in actively growing sessions)
python3 dashboard-rs/tools/parity.py --state-from /path/to/claude-hq
```

It deliberately skips `/api/music/now` (AppleScript to music apps),
`/api/music/search` / `oembed` with real input (YouTube) and `POST /api/update`
(git fetch + restart).

**Current result (frozen, 153 sessions, ~430 MB of transcripts):** 166 checks,
**165 identical**; 135 of 142 JSON bodies byte-identical, the other 7 differing
only in values that must differ between two processes (`updated`, `generated`,
`boot`, `pid`, uptime, CPU/RSS). `/api/sessions` matches with every
clock-driven value (fatigue, sparks, ages) included. The one remaining diff is
the first known gap below. In a live run, the only extra differences come from
sessions being written between the two servers' reads.

Rough numbers on the same frozen data (M-series Mac, release build):

| | Python | Rust |
|---|---|---|
| first `/api/sessions` (cold scan of every transcript) | 6.0 s | 1.3 s |
| warm `/api/sessions` (after the 1.5 s memo) | 40 ms | 37 ms |
| RSS after warm-up | 140 MB | 81 MB |

## Known gaps and differences

- **Invalid `Content-Length`** (`abc`, `-5`): hyper rejects the request itself
  with an empty `400`; Python answers `400` with a JSON error. The status
  matches; the body doesn't. (A missing length is still `411`, too big `413`.)
- **Status 0 from the Arena** on POST routes that pass the code through
  (publish, ticket, nudge, quickplay, crews, cosmetics, market, portrait, HQ):
  Python writes a literal `HTTP/1.0 0` status line; Rust sends `502`.
- `/api/server-stats`: `"python"` reads `n/a (Rust 2.5.0)`, there is an extra
  `"impl": "rust"`, and `threads` is the OS thread count (`ps -M`).
- HTTP/1.1 with keep-alive instead of Python's HTTP/1.0; `Server: ClaudeHQ/2.5.0 Rust`.
- `POST /api/update` "stale" compares the running **binary's** mtime (Python
  watches `dashboard.py`); the restart re-execs the binary, or kickstarts
  launchd when launchd started it.
- Arena HTTPS uses rustls with the Mozilla root set, not the macOS keychain:
  an Arena behind a private CA won't verify.
- macOS asks once, separately, before `claudehq` may script Spotify / Music /
  browsers for Now Playing (it is a different app than `python3`).
- Python-only corner cases: JSON lines with `NaN`/`Infinity` literals are
  skipped (Python parses them); in malformed records (a non-string tool name,
  model or text block) a record can stop being counted at a slightly different
  point than Python's exception would; `str.casefold` is approximated by
  `to_lowercase` for the cali duplicate-diner check.
- Ordering Python leaves to set iteration (randomized per process) is fixed
  here to first-seen order: ties in `folderLeaderboard`, and the float sum
  order of a multi-day digest.

## Layout

| file | ports |
|---|---|
| `src/scan.rs` | `_scan_file_uncached`, the Cursor scanner, prompt/markdown/error helpers |
| `src/app.rs` | paths, caches, `scan_file`, config / meta / meals / game save, dex salt |
| `src/payload.rs` | `build_session`, archived cards, season, feed, payload memo + stream signature, session detail |
| `src/analytics.rs` | search, history, Pokédex, project, digest, insights, transcript events, exports |
| `src/fatigue.rs`, `src/creature.rs`, `src/cost.rs` | fatigue walk, creatures/levels, prices |
| `src/handlers.rs` | pantry bodies + eat flow, quest rewards, cali orders, room bodies, soundboard |
| `src/arena.rs`, `src/ext.rs`, `src/music.rs`, `src/worksignals.rs` | `arena.py`, `ext_*.py`, `music.py`, `worksignals.py` |
| `src/server.rs` | `Handler`: routes, gates, static files, `assemble_index`, SSE |
| `src/actions.rs` | resume / reveal / close, self-update, launchd, notifications |
| `src/py.rs`, `src/pyjson.rs`, `src/difflib.rs`, `src/state.rs` | Python semantics the port relies on (str methods, `round`, `isoformat`, `json.dumps` byte format, `SequenceMatcher`, atomic writes) |
