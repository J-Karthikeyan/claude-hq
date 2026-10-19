# dashboard-rs (`claudehq`)

A Rust port of the local Claude HQ server: `dashboard.py` and everything it
imports (`arena.py`, `music.py`, `worksignals.py`, `ext_skills.py`,
`ext_loot.py`, `ext_prestige.py`, `ext_maps.py`, `ext_cups.py`,
`ext_boss.py`, `ext_skate.py`, `ext_notch.py`), at parity with
`dashboard.py` **2.6.2**: including the evolution picks (`POST /api/evo-choice`,
`evo-choices.json`, `creature.evo` in `/api/sessions`), the Overflow Vial
(`vial.json`, `payload.vial`, `vialPoured`, `POST /api/vial/spend`), archived
cards for prompt-less transcripts and error alerts that clear on a later good
reply. Same routes, same gates, same JSON, same state files, same Arena privacy
boundary. The frontend (`index.html`, `ui/`, `games/`) is unchanged and served
from the repo as before.

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
~/.cargo/bin/cargo test                                  # 166 tests
~/.cargo/bin/cargo clippy --all-targets -- -D warnings   # clean
```

The unit tests port the behavioural Python suites: the cost model, config
validation, atomic state and quarantine, the meal ledger, the fatigue contract
vectors, needs-you detection, slim cards, the payload memo, flexible streaks,
Cursor transcripts and titles, work-signal classification, music parsing and
the share loop, the Arena wire allowlists (pantry, cali, rooms, portrait,
gifts, nudges), every ext proxy (skills, loot, prestige, maps, cups, boss,
skate), the Overflow Vial ledger (`test_vial.py`), error recovery
(`test_scanner.py` RecoveryTests), the pantry eat flow, close/resume guards,
evolution picks (`test_evo_choice.py`),
HQ Notch status and install (`test_notch_invite.py`, with a stand-in installer)
and the self-update, including the rebuild (on throwaway git repos with a fake
`cargo`). `tests/http.rs` checks the gates on a real 127.0.0.1 server (also for
`/api/evo-choice`, `/api/vial/spend` and `/api/notch/install`), and `tests/scanner.rs` the
transcript pass, busy spans and work signals.

## Parity

`tools/parity.py` (stdlib only) starts `dashboard.py` and `claudehq` on two
ports (never 8765) against **one** temp copy of the Claude HQ folder: code
symlinked, state files copied, no `arena-link.json`, the Arena URL blanked and
work signals off, so nothing can pair or leave the machine. It GETs every
route on both (111 GET/HEAD checks, including `/api/notch/status`,
`/api/arena/skate` and `/api/sessions` again after seeding an evo pick for
every session that has a choice), diffs status, headers and normalized JSON,
then replays 73 safe POSTs on each (9 `/api/evo-choice`, 8 `/api/vial/spend`
against a seeded two-drop vial, 6 `/api/arena/skate/combo`; resetting the temp
state between servers), diffs the state files they leave (a spend's own clock
stamp in `vial.json` aside), and runs 11 checks on the POST
gates (CSRF, Origin, Host, body caps, bad bodies; `/api/notch/install` only
without a token or from a foreign Origin, so the real installer never runs).

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

**2026-10-19 (frozen, against `dashboard.py` 2.6.2, 153 sessions):** 195
checks, **194 identical**; 162 of 170 JSON bodies byte-identical, the rest
differing only in per-process values. `/api/sessions` (all 153 cards, the vial,
`vialPoured`, `creature.evo` on every card) matches; with clock-driven keys
kept, the only differences are the vial drops' `ttl` (seconds until each
drop dies, read off each server's own clock). The one remaining diff is the
bad `Content-Length` gap below.

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
- `/api/server-stats`: `"python"` reads `n/a (Rust 2.6.2)`, there is an extra
  `"impl": "rust"`, and `threads` is the OS thread count (`ps -M`).
- HTTP/1.1 with keep-alive instead of Python's HTTP/1.0; `Server: ClaudeHQ/2.6.2 Rust`.
- `vial.json` corner cases: a `NaN` / `Infinity` literal in a hand-edited
  ledger is dropped like any other bad entry; a `seenPrompts` of `Infinity` or a
  non-numeric `stage` string makes Python's read raise (and the payload build
  fail) where Rust reads 0.
- `POST /api/update` "stale" compares the running **binary's** mtime (Python
  watches `dashboard.py`), and is also true when this checkout's Rust sources
  (`Cargo.toml`, `Cargo.lock`, `src/`, `assets/`) are newer than the binary it
  runs. The restart re-execs the binary, or kickstarts launchd when launchd
  started it.
- **The self-update rebuilds itself.** When a pull changes anything under
  `dashboard-rs/` (or the build is already older than its sources), the update
  runs `cargo build [--release] --locked --manifest-path dashboard-rs/Cargo.toml`
  (cargo from PATH, else `~/.cargo/bin/cargo`; up to 900 s; the profile of the
  running binary) **before** restarting, so the restart runs the new code. If
  cargo is missing or the build fails, it does not restart: the pulled code
  stays, the answer is a `409` whose error ends with the command to run by
  hand, and "stale" stays on so the Update button offers the rebuild again.
  It only rebuilds `dashboard-rs/target/{release,debug}/claudehq` of this very
  checkout; a `claudehq` copied elsewhere gets the `409` instead.
- `POST /api/notch/install` runs `/usr/bin/python3 notch/tools/install.py`
  (cwd = the HQ folder), or `python3` from PATH when `/usr/bin/python3` is
  missing; Python uses its own `sys.executable`. The install already requires
  the Command Line Tools (for `swift`), which provide `/usr/bin/python3`, and
  the installer pins its hooks to that interpreter anyway. The job log is
  decoded as UTF-8 (lossy) where Python would fail on undecodable output.
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
| `src/notch.rs` | `ext_notch.py`: HQ Notch status + one-click install |
| `src/evo.rs` | evolution picks: `evo_tables`, `clean_evo_choice`, `load` / `save_evo_choice`, `merge_evo_choices` |
| `src/vial.rs` | the Overflow Vial: `_vial_read` / `_vial_write`, tick, grant, drip, cap, spend, sources, `payload.vial` |
| `src/server.rs` | `Handler`: routes, gates, static files, `assemble_index`, SSE |
| `src/actions.rs` | resume / reveal / close, self-update, launchd, notifications |
| `src/py.rs`, `src/pyjson.rs`, `src/difflib.rs`, `src/state.rs` | Python semantics the port relies on (str methods, `round`, `isoformat`, `json.dumps` byte format, `SequenceMatcher`, atomic writes) |
