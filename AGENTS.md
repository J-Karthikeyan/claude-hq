# AGENTS.md — working in Claude HQ

Guidance for anyone (human or AI agent) making changes here. **Read [`CONTRIBUTING.md`](./CONTRIBUTING.md) in full before your first change** — this file is only the fast path to it.

## What this is
A **local-first, privacy-sensitive** dashboard that reads your own Claude Code sessions and renders them as a live command center with a creature-collection game layer. Two files do the work: `dashboard.py` (stdlib-only HTTP server on `127.0.0.1`) and `index.html` + `ui/` (frontend template; CSS and script split by area in `ui/`, stitched into one page by `assemble_index()`). `arena.py` is the opt-in multiplayer client.

## Non-negotiable invariants (a PR violating one will not merge)
1. **No build step.** No npm/bundler/transpiler; no TypeScript/JSX/SCSS; no external JS/CSS libraries (one narrow, documented exception: games/vendor/ holds vendored MIT three.js (r186, unminified, only its import paths rewritten, reproducible with tools/vendor_three.py) used by the 3D games; it is served only from 127.0.0.1, loaded lazily, never fetched from a third party at runtime; see CONTRIBUTING.md). Plain `.html`/`.css`/`.js` served as-is. Backend is Python **stdlib only** — no `pip install`, ever.
2. **Privacy.** Transcript content — prompts, replies, tool I/O, file paths, project/folder names, session ids, titles — **must never leave the machine.** The only outbound traffic allowed is `<img src>` sprite hotlinks (URL carries only a non-reversible `hash(sessionId)%48` slug) and the **opt-in** Arena channel (daily aggregate counts only). Every other `fetch`/`EventSource` targets local `/api/...`. Prove any new network call carries no transcript-derived data, or don't send it. Music (2.2) is the documented exception set: the Now Playing track (`music.TRACK_KEYS` only) over the paired Arena link, YouTube search/oEmbed from `music.py`, the YouTube embed or a `yt-dlp` fetch of the room's song on this machine when someone uses a listening room (opt-in: yt-dlp may read one browser's YouTube cookies, `musicCookies`, sent only to youtube.com), and WebRTC audio between browsers when someone goes live. None of it reads transcripts. The 3D character (2.4) adds two more: the character spec (ten small numbers, `look` in hq presence) and its portrait PNG (`/v1/me/portrait`), both generated from the user's own Settings choices; no transcript, session or path data goes into either. HQ 2.5 adds, all opt-in: work-signal **counts** (seven skill categories per UTC day via `/v1/skills/report`, and loot events `{requestId, type, n, day}` with type `tests_green`/`focus_long`/`pr_merged` via `/v1/loot/events`), only when `workSignals` is on (PRs also need `workSignalsPRs`; `gh` runs locally and only salted URL hashes are kept, locally); user-made maps (geometry + the name the user typed, `{kind, name, data, scope, roomId?}`) when the user saves to the gallery or starts a room on one; and boss-fight teams (species/stage numbers). Classification lives in `worksignals.py` and must only ever return integers per category. HQ 2.6.3 adds the opt-in **account board** key (`arenaAccountBoard`): a sha256 of `oauthAccount.accountUuid` from `.claude.json` with a fixed prefix, nothing else from that file, sent to `/v1/me/account` and stored by the Arena only as an HMAC.
3. **XSS discipline.** The frontend builds HTML by string concatenation. **Every** data-derived value entering an HTML string must pass through `esc()` (or use `textContent`). Transcript text is attacker-influenced.
4. **IP rule.** Only original art (generated `monsterSVG`, original Village `troopSVG`, MIT Lucide icons/thinking-orbs) or **pre-existing runtime hotlinks** (never bundled). **Third-party models, textures and sounds may be vendored** when their license allows redistribution (e.g. Kenney's CC0 kits in `games/golf/`, `games/kart/`): each game's folder ships the license text alongside, files are served only from 127.0.0.1 and never hotlinked at runtime, and the PR names the source. **No new copyrighted characters/assets/names — specifically no Supercell / Clash assets, characters, or names.** Invent your own.
5. **Backend security.** Bind `127.0.0.1` only; `_host_ok()` gate on every request; mutations are POST + CSRF (`X-HQ-Token`) + same-site Origin check; validate every path/id against an allowlist (no traversal). Reads GET, mutations POST.

## The Arena backend is Rust (`backend-rs/`)
The multiplayer Arena runs on the **Rust** server in `backend-rs/` (axum + sqlx; it owns the schema through `backend-rs/migrations/`). **Every Arena backend change goes there**: new routes, game engines (`backend-rs/src/valley/`), room/socket behaviour, results, schema. `backend/` (Python/FastAPI) is frozen: it is the rollback target for `ops/release.sh rollback` and nothing else. Do not add features to it; do not "port back". The local server `dashboard.py` / `arena.py` (the HQ app on your machine) is unchanged by this and stays Python stdlib.
- Before a PR: `cd backend-rs && cargo test && cargo clippy --locked --all-targets -- -D warnings`; for socket changes also `uv run python backend/scripts/socket_parity.py`.
- Deploy: `ops/release.sh deploy` (keeps the running impl, now `rs`); `ARENA_IMPL=rs ops/release.sh check` to try a build first.

## House style (match it, don't "modernize")
- `var`, function-scoped. Defensive guards on every helper (`cr = cr || {}`, `|0`, `Math.max/Math.min`, `!=null ? x : fallback`).
- Small pure helpers; deterministic sprites via `mulberry32(hashStr(...))` — **never `Math.random()`** for anything a user sees twice.
- CSS: semantic tokens only (no hard-coded hex); style **all** themes (aurora/midnight/forest/mono/contrast, light+dark); gate **every** animation behind `html.hq-calm` **and** `@media (prefers-reduced-motion: reduce)`.
- Accessibility is shipped: roving tabindex, `role`/`aria-*` in sync, `announce()` for live status, `aria-hidden` decorative art, full keyboard reach.

## Performance norms
- **`partySig`** rebuilds `#party` only on card-relevant changes; never add volatile per-tick values (`now`/`ageSecs`/`tokens`) to the signature — it restarts every animated sprite.
- **`scan_file`** is the single mtime/size-cached transcript pass; route new per-transcript computation through it, don't re-parse files.

## Before you open a PR
- `python3 -m unittest discover -s tests -v` (green; add tests for backend logic you change).
- Manually verify the changed view across themes, Calm on, Large-text on, keyboard-only, and blocked-CDN (sprites fall back to generated SVG). No console errors. No un-`esc()`'d interpolation in the diff.
- Bump `APP_VERSION` in `dashboard.py` + add a README Changelog entry for user-facing changes. Call out anything touching privacy / egress / security / third-party assets explicitly.

## Sprite roster (reference)
The `pokemon`/`pokemon3d`/`aniimo` packs hotlink pre-existing public sprite libraries at runtime; `monsters` and `village` are 100% locally-generated SVG (zero network). Every hotlink must degrade to `monsterSVG` via the `onerror` chain — a sprite with no fallback is a bug.
