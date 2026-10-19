# Contributing to Claude HQ

Claude HQ is a **local-first, privacy-sensitive** dashboard that reads your own Claude Code sessions (`claude agents --json` + `~/.claude/projects/**/*.jsonl`) and renders them as a live command center with a creature-collection game layer on top. It runs entirely on `127.0.0.1`, has **no dependencies**, and has **no build step**.

These standards are load-bearing. Several of them (privacy, XSS discipline, the IP rule, no-build-step) are non-negotiable invariants — a PR that violates one will not be merged regardless of how nice the feature is. Read the whole document before your first change.

---

## 1. Architecture & the no-build-step constraint

Two files do everything:

| File | Role |
|---|---|
| `dashboard-rs/` | The HQ server (`claudehq`, Rust: axum + tokio). Reads live agents + transcripts, computes cost/creature/season data, serves the JSON API and the page. |
| `index.html` + `ui/` | The frontend: `index.html` is the markup template, `ui/` holds its CSS and script split by area; the server stitches them into one page (§2). |

Optional, and out of scope for most contributions: `arena.py` (opt-in multiplayer client, also stdlib-only), `backend/` + `backend-rs/` (the Arena server someone self-hosts), and the `*-wizard.sh` / `ops/` deploy tooling.

**The no-build-step rule is absolute.** The LaunchAgent and `claudehq` serve files *as they are on disk*. There is no bundler, transpiler, minifier, or `node_modules`. This means:

- **No npm, no package.json, no build/watch scripts.** If a change would require a build step to run, it does not belong here.
- **No TypeScript, JSX, SCSS, or any dialect that needs compilation.** Plain `.js`, plain `.css`, plain `.html`.
- **No external JS/CSS libraries.** No React, no jQuery, no Tailwind, no icon fonts, no Chart.js. Charts are hand-drawn SVG; icons are an inline SVG sprite (Lucide geometry, MIT). Keep it that way.
  - **One library exception (the 3D games):** games/vendor/ holds vendored MIT three.js (r186, unminified, only its import paths rewritten, reproducible with tools/vendor_three.py), served only from 127.0.0.1, loaded lazily by the 3D games (Mini Golf, Kart Racing, Platformer Rush, Blaster Arena), never fetched from a third party at runtime. It is not a precedent for other libraries. Game assets are covered in §7: each 3D game keeps its models in its own folder (`games/golf/`, `games/kart/`, `games/platformer/`, `games/fps/`). The `/games/` route serves only `vendor/<name>.js` and `<game>/<name>.glb|json|png` for those folders (tests in `tests/test_games.py`).
- **No import maps requiring a server rewrite, no bare specifiers.** ES module imports must be relative paths (`./foo.js`) that the plain static server can resolve directly.

`GET /` re-assembles `index.html` + `ui/` from disk on every change (frontend edits are live on refresh). After changing `dashboard-rs/` under the LaunchAgent, rebuild (`cargo build --release --locked --manifest-path dashboard-rs/Cargo.toml`) and reload the server:

```bash
launchctl kickstart -k gui/$(id -u)/com.claudehq.dashboard
```

### Backend security model (do not weaken)

Every request is gated by the loopback + CSRF machinery already in `dashboard-rs/src/server.rs`; preserve all of it when adding endpoints:

- Server binds to `127.0.0.1` **only** — never `0.0.0.0`.
- `_host_ok()` rejects any request whose `Host` header isn't `127.0.0.1`/`localhost`/empty with **403**. Every `do_GET`/`do_POST` calls it first.
- All state-changing requests are **POST** and require the per-process CSRF token (`X-HQ-Token`, checked with `secrets.compare_digest`-style equality) **plus** an Origin / `Sec-Fetch-Site` same-site check. The token is minted per process (`CSRF_TOKEN = secrets.token_hex(16)`) and injected into the page by replacing the `__HQ_CSRF__` placeholder — so only a same-origin page can read and echo it.
- File lookups are validated against UUID / known-folder allowlists — **no path traversal**. Any new endpoint that takes a path or id must validate it the same way.
- **Reads are GET, mutations are POST.** Never mutate state on a GET.

---

## 2. File & module layout

`index.html` is a template: markup, plus `@include` lines for its CSS and script, which live in `ui/`. The server (`dashboard-rs`) stitches them back in, in order, every time the page is served, so the browser still gets one page with one inline script (same globals, hoisting and `"use strict"` as before the split). There is no build step: edit a file in `ui/` and reload.

```
index.html             # markup + //@include lines (template)
ui/hq.css              # all styles: tokens, themes, components (see §4)
ui/orbs.js             # vendored thinking-orbs engine (MIT)
ui/app/00-core.js      # $, esc, state, render loop, boot helpers
ui/app/01-creatures.js # creature*/poke*/monster*/troop* helpers + sprite packs
ui/app/02-store-art.js … 21-arena-controls.js   # one file per area of the page, loaded in name order
games/*.js             # Valley minigames, loaded on first use
```

The files run as ONE script in name order, so a top-level function in a later file is still hoisted for code in an earlier one. Tests read the stitched page with `dashboard.assemble_index()`.

Rules for where new code goes:

- **New pure helper** (formatting, hashing, seeding, escaping) → `ui/app/00-core.js`. Keep it small and side-effect-free.
- **Anything touching creatures, sprites, evolution, packs** → `ui/app/01-creatures.js`. This is the single home for the `creature*`, `poke*`, `monster*`, `troop*`, `animo*` families.
- **A new view** → its own `ui/app/NN-<view>.js` plus an `//@include` line in `index.html`, wired into the view switcher (`ui/app/10-views.js`). Add the tab button with proper `role="tab"` + `aria-selected` (see §7) and map its number key.
- **A new API endpoint** → add the route in `dashboard-rs/src/server.rs` next to the existing ones, behind the same host/CSRF gates; document it in the README's HTTP API line.
- **Styles** → `ui/hq.css` (tokens at the top). Never inline a hex color (see §4).

During the refactor, when you touch a region that is still inline, **extract that region** rather than adding more inline code beside it. Don't start new inline `<style>`/`<script>` content.

Modules use native ESM: `export function foo(){}` / `import { foo } from './util.js'`. Because functions like `pokeErr`, `svgErr` are referenced from inline HTML `onerror=` attributes, any function invoked from a generated-HTML attribute must remain reachable from the global scope — attach it explicitly (`window.pokeErr = pokeErr`) rather than relying on it being a top-level function name. Prefer migrating those to `addEventListener` where practical, but if you keep the `onerror=` attribute the global binding is required.

---

## 3. JavaScript conventions (as actually used here)

This codebase has a deliberate, consistent house style. Match it — do not "modernize" it piecemeal.

- **`var`, not `let`/`const`.** The entire frontend is `var`-based function-scoped code. Keep new code `var`-based within the modules unless an entire file is being rewritten to `const`/`let` in one pass with review sign-off.
- **Defensive argument guards everywhere.** Every creature helper opens with `cr = cr || {};` and coerces before use. This is because creature objects arrive from the backend, from cached state, and from partial SSE frames, and may be missing fields. Follow the pattern:
  ```js
  function creatureStage(cr){ cr=cr||{}; var s=cr.stage!=null?cr.stage|0:3; return Math.max(0,Math.min(4,s)); }
  ```
  Coerce ints with `|0`, clamp with `Math.max/Math.min`, default with `!=null ? x : fallback`, and guard nested access (`(s.tokens||{}).output`, `(s.tags||[]).join(",")`). Never assume a field exists.
- **Small pure helpers.** `esc`, `el`, `ico`, `orb`, `hashStr`, `relTime`, `stage_for`-equivalents. A helper should do one thing, take plain args, and (ideally) return a value with no DOM side-effects. Rendering functions may touch the DOM; computation helpers should not.
- **Deterministic PRNG seeding.** Sprite generation must be reproducible: the same creature always draws the same sprite. Use the vendored `mulberry32(seed)` PRNG seeded from a stable hash — never `Math.random()` for anything a user sees twice. Seeds derive from stable identity, not volatile state:
  ```js
  function creatureSeed(cr){ cr=cr||{}; return hashStr((cr.species||cr.name||"")+"|"+creatureIndex(cr)); }
  var rng = mulberry32(seed);
  ```
  The backend mirrors this determinism (`species_seed`, `shiny_for_species` use `sha256` over a fixed string). Shiny and species are **stable per-species** so the Pokédex and live cards always agree — do not introduce per-session randomness into anything catalog-facing.

### XSS discipline — `esc()` on every interpolation (mandatory)

The frontend builds HTML by string concatenation (`el.innerHTML = '...'`). **Every** value that originates from transcript content, session titles, tags, notes, aliases, folder names, prompts, or tool names — i.e. anything not a hard-coded literal — **must** pass through `esc()` before it enters an HTML string:

```js
sum.innerHTML = '<span class="dot '+g.dot+'"></span>'+esc(g.label)+' <span class="count-pill">'+vis.length+'</span>';
img... ' alt="'+esc(altName||"")+'"' ...
```

`esc()` escapes `& < > "`. Rules:

- Text content and **all** attribute values built from data go through `esc()`. Numbers you produced yourself (`|0`, `.length`) are safe to inline.
- When you can, prefer `node.textContent = value` (no escaping needed) over `innerHTML`.
- **Never** interpolate raw session/transcript data into `innerHTML`, an `on*` attribute, an `href`/`src`, or a `<style>` without escaping/validating. This is the number-one security-sensitive area of the frontend — transcript text is fully attacker-influenced if you ever open someone else's transcript, and even your own can contain HTML from tool output.
- A reviewer will grep your diff for `innerHTML` and unescaped `+` interpolation. Assume every one will be questioned.

---

## 4. CSS: semantic tokens, theming, and accessibility modes

### Semantic design tokens

All color and surface values come from CSS custom properties defined in `:root` (`--bg`, `--bg2`, `--panel`, `--panel2`, `--ink`, `--line`, `--faint`, `--brand`/accent, `--need`, etc.). **Never hard-code a hex color in a component rule** — reference the token. New surfaces should be composed from existing tokens (use `color-mix(in srgb, var(--bg) 86%, transparent)` for translucency, as the existing header does) rather than inventing new raw colors. If you genuinely need a new semantic role, add a token to `tokens.css` and define it under **every** theme.

### Theme-aware (aurora / midnight / forest / mono + contrast)

Themes are selected by `data-theme` on the root element. The default (aurora) lives in `:root`, and each alternative overrides tokens under `:root[data-theme="midnight"|"forest"|"mono"|"contrast"]`. Several themes also branch on `@media (prefers-color-scheme: dark)` for their light/dark pair. When you add a token or a themed surface:

- Define it for **all** themes (aurora default + midnight + forest + mono + high-contrast). A component that only looks right in one theme is incomplete.
- Respect the light/dark split where a theme has one.
- The accent color is user-adjustable (`--brand` via the accent slider/swatches) — build interactive/emphasis styling on `var(--brand)` and `accent-color:var(--brand)` so it tracks the user's choice.

### Reduced-motion / calm mode

Every animation must be disabled under both the user's OS setting and the in-app Calm toggle. The established pattern is a paired rule — replicate it for **any** new animation:

```css
html.hq-calm .my-thing{animation:none}
@media (prefers-reduced-motion: reduce){ .my-thing{animation:none} }
```

Animated sprites (SMIL/GIF/CSS "breathe"/"float") count — gate them too. Motion is decorative; the app must be fully usable and legible with all of it off.

### Large-text mode

Large-text mode is a root class toggle. Size type and spacing in relative units so the large-text scale flows through; don't pin critical text to fixed pixel sizes that ignore it (chart *axis labels* are a deliberate fixed-size exception).

---

## 5. PRIVACY INVARIANTS (the point of the whole project)

Claude HQ's entire value proposition is that **your conversations never leave your computer.** This is the single most important standard in the repo.

**The invariant:** only **non-transcript, non-identifying, aggregate** data may ever cross the network — and only over an explicitly opt-in channel. **Transcript content — prompts, replies, tool inputs/outputs, file paths, project/folder names, session ids, titles — must NEVER be sent to any network, period.**

Concretely:

- **All same-origin traffic is fine and is the norm.** Every `fetch()` and the `EventSource('/api/stream')` in the frontend targets local `/api/...` paths served by `claudehq` on `127.0.0.1`. That is not egress. Keep it that way — never point a `fetch` at an external host.
- **The only outbound requests to the public internet are `<img src>` sprite loads** (see §6) and the **opt-in Arena** channel. Nothing else may leave the machine.
- **Sprite CDNs are privacy-safe by construction:** an `<img>` request carries only a **Pokédex number / creature slug** in the URL path — a value derived from `hash(sessionId) % 48`, which is not reversible into any session content and reveals nothing about what you were doing. No query params, no headers with your data, no beacons. That's why the "pokemon" packs are acceptable. The "monsters" (and Village troop) packs are 100% locally generated SVG and send nothing at all — they exist precisely so a fully-offline user leaks zero bytes.
- **Arena is off by default** and, when enabled, publishes **daily counts only** (prompts, tool calls, artifacts, tokens) — never prompt text, replies, file paths, project/folder names, session ids, or titles. Tool names are allowlisted (anything not a built-in Claude Code tool — e.g. `mcp__<server>__<tool>`, which often carries an employer/client name — is bucketed as `Other` before the wire). Cost sharing and live status are separately opt-in. The Arena device token lives in `arena-link.json`, **outside** `config.json`, so it is never served to the page. The wire format rejects unknown fields so a future change can't silently start leaking one.

**When reviewing or writing any code that makes a network request, or that adds a field to an Arena payload, the burden is on you to prove it carries no transcript-derived content.** If you can't prove it, don't send it. A new outbound request to any host other than the documented sprite CDNs (over an `<img>`) or the user-configured Arena server is a privacy regression and will be rejected.

`config.json`, `sessions-meta.json`, `arena-link.json`, and the log files are git-ignored and must stay that way — never commit runtime state or tokens.

---

## 6. Sprites, assets & the IP rule

### The IP rule (hard line)

**Three kinds of asset are permitted:**

1. **Original art** authored for this repo — the generated pixel-monster SVGs (`monsterSVG`), the original Village troop creatures (`troopSVG`), the inline Lucide-geometry icon sprite (MIT), and the vendored thinking-orbs (RareFormLabs, MIT).
2. **Pre-existing hotlinks** to already-established public sprite libraries, loaded at runtime via `<img src>` and **never bundled into the repo**.
3. **Vendored third-party game assets** (models, textures, sounds) whose license allows redistribution — e.g. Kenney's public-domain (CC0) kits. Each game keeps them in its own folder under `games/` with the license text alongside (`LICENSE-<source>.txt`, checked by `tests/test_games.py`), they are served only from 127.0.0.1 and never hotlinked, and the PR says where they came from.

**No new copyrighted characters, assets, or names may be added** — and specifically **no Supercell / Clash of Clans / Clash Royale assets, characters, or names.** The Village pack deliberately implements base-building/army *mechanics* with **100% original art and invented names** (Thwack, Pipp, Zephry, …) precisely to avoid this. If you build another game layer, invent your own creatures and names; do not import someone's IP.

Pokémon names/sprites remain the property of Nintendo/Game Freak/The Pokémon Company; the "pokemon" packs hotlink them from public sprite libraries for personal use, which is why they're a runtime hotlink and not committed. Bundled third-party art follows rule 3 above: a redistributable license, the license text next to the files, no runtime hotlinks — today Kenney's CC0 kits for Mini Golf (`games/golf/`), Kart Racing (`games/kart/`), Platformer Rush (`games/platformer/`) and Blaster Arena (`games/fps/`).

### The sprite fallback chain

Every hotlinked sprite **must** degrade gracefully to locally-generated art so an offline or ad-blocked user always sees *something*. The chain is implemented via `data-*` attributes + `onerror` handlers (`pokeErr`, `svgErr`), and the final fallback is always `monsterSVG(...)` reconstructed from a `data-fb` seed payload:

- **3D (`pokemon3d`) pack:** X/Y GIF (Gen 6, animated, near-full national dex — covers every species ≤649 used here) → HOME static PNG → generated `monsterSVG`. (X/Y leads deliberately: leading with SwSh/Galar 404'd for the many curated species not in the Galar dex, which caused a render→dim→re-render flicker.)
- **Gen-5 "pokemon" pack:** animated GIF → static PNG → generated `monsterSVG`.
- **Aniimo pack:** hotlinked WebP → generated `monsterSVG`.
- **monsters / village packs:** generated SVG only — no network.

The current hotlink hosts (for reference; don't add new ones without discussion and a privacy review): `cdn.jsdelivr.net` (jsDelivr mirror of the PokéAPI sprites repo — chosen over `raw.githubusercontent.com` because ad/content blockers like Opera GX don't block it), `www.pkparaiso.com` (Gen 8 SwSh + Gen 6 X/Y animated 3D), `aniimotools.dev` (Aniimo fan DB).

When adding a sprite path, always: pass `loading="lazy"`, an `esc()`'d `alt`, the `data-fb` seed array, and an `onerror` that lands on the generated SVG. A sprite with no fallback is a bug.

---

## 7. Accessibility

Accessibility is a shipped feature, not an afterthought. Maintain:

- **Roving tabindex** for grid/list navigation (Pokédex, card grids): one element is `tabindex="0"`, the rest `tabindex="-1"`, arrow keys move focus. Don't leave a grid where every cell is a tab stop.
- **ARIA roles & state:** the view switcher is a `role="tablist"` with `role="tab"` buttons carrying `aria-selected`/`aria-current`; keep those in sync when you add or switch views. Interactive non-button elements get `role="button"` + `tabindex="0"` + a keyboard handler.
- **Live regions:** transient status goes through the polite live region via `announce(msg)` (which clears then re-sets `#a11yStatus` so identical consecutive messages re-fire). Use it for "copied", "resumed", "saved", etc.
- **Decorative elements are hidden:** orbs, icons, sprites, dots get `aria-hidden="true"` / `focusable="false"`. Icons via `ico()` are already hidden; keep it.
- **Keyboard:** every action reachable by mouse must be reachable by keyboard. Preserve the shortcut map (`?` help, `1`–`7` views, `/` search, `r` refresh, `⌘/Ctrl-K` palette), skip link, focus-visible rings (`:focus-visible`), and focus trapping in the drawer/modals. Don't hijack browser-native shortcuts (a past bug — see the 1.1.1 changelog).
- Provide meaningful `alt`/`aria-label` (escaped) on images and icon-only buttons — `setActLabel` already sets `aria-label` from the action label; follow that pattern.

---

## 8. Testing & manual verification

Tests live in `tests/` and are **stdlib `unittest` only** (to keep the no-`pip` promise). Run:

```bash
python3 -m unittest discover -s tests -v
```

CI (`.github/workflows/tests.yml`) runs exactly this on Python 3.12 for every push to `main` and every PR — nothing is installed.

- **Add/extend tests for backend logic you change**, especially the cost model, config validation, creature/stage math, and any parsing of transcript JSONL. Server logic is tested in Rust (`cd dashboard-rs && cargo test`): unit tests next to the code, `tests/http.rs` for the gates on a real server. Prefer testing pure functions — that's another reason to keep logic in small pure helpers.
- Backend defaults must be safe on garbage input (see `_price_for(None) → sonnet`, `_usage_cost("opus", None) → zeros`). New parsers should be tested against `None`, `""`, and non-dict inputs.
- **Frontend has no automated harness** (no build = no bundled test runner). Verify manually: run `dashboard-rs/target/release/claudehq`, exercise the changed view, and check:
  - every theme (aurora/midnight/forest/mono/contrast), light and dark;
  - Calm mode on (animations stop) and Large-text on (layout holds);
  - keyboard-only navigation of the changed area;
  - offline / blocked-CDN behavior (sprites must fall back to generated SVG);
  - no console errors, and no un-`esc()`'d interpolation in your diff.
- If your change has a meaningful visual surface, attach a screenshot to the PR (the repo already uses `verify_*.png`, which is git-ignored — don't commit it, attach it).

---

## 9. Commit messages & pull requests

- **Commit subject: imperative, present tense, capitalized, one line, no trailing period**, describing the user-visible change. Match the existing log style:
  - `Add Village view: a base-building/army game screen driven by your Claude stats`
  - `Fix sprite flicker + 3D pack Gen-8 only + bump SW cache`
  - `Keep analytics trend-chart labels a fixed size at any width`
- Keep commits focused; explain *why* in the body when the subject can't. Reference issues/PRs (`#30`) where relevant.
- **Do not commit runtime state or secrets:** `config.json`, `sessions-meta.json`, `arena-link.json`, `claude-hq*.log`, `verify_*.png`, `__pycache__/` are all git-ignored — keep them so.
- **PRs:** state what changed and why, call out anything touching **privacy, network egress, security (Host/CSRF), or third-party assets** explicitly (these get the closest review), list how you verified (tests run + manual checks from §8), and attach screenshots for visual changes. Bump `APP_VERSION` in `dashboard-rs/src/app.rs` and add a Changelog entry in `README.md` for user-facing releases (patch = fixes/polish, minor = features). Credit external contributors in the README, as the existing entries do.
- CI must be green (the unittest workflow) before merge.

---

## 10. Performance norms

- **`partySig` gating to prevent sprite flicker.** The live view rebuilds `#party` only when a *card-relevant* signature changes. `partySig()` hashes only fields that affect the rendered card (id, status, title, alias, pinned, tags, note, folder, species, stage, shiny, plus the active query/sort/filter/pack) — **deliberately excluding volatile fields** (`now`, `ageSecs`, `tokens`) that tick on every SSE frame. If they were included, every animated sprite would restart on each tick. **When you add data to a card, decide consciously whether it belongs in the signature.** Add it only if a change to it should force a re-render; keep high-frequency, non-structural values out. Never rebuild the whole party for a value that changes every second.
- **`scan_file` mtime/size caching.** `scan_file(path)` is the single per-file transcript scan feeding both the session view and the season/analytics computations. It caches the rich aggregate keyed by `(st_mtime, st_size)` in `_scan_cache` and re-reads a `.jsonl` only when that key changes. **Route any new per-transcript computation through `scan_file`'s single pass** — add fields to the aggregate rather than opening and re-parsing the file in a second pass. Re-reading every transcript on every request would make the dashboard unusable once history accumulates.
- **General:** the SSE stream is the live path; keep per-frame work cheap. Large lists (archived/stale history can be huge) should be lazily built/collapsed as the stale group already is. Prefer incremental DOM updates over full re-renders. Use `loading="lazy"` on off-screen images. Keep the backend O(changed files), not O(all files), per request wherever the cache allows.

---

*Claude HQ is MIT-licensed. By contributing you agree your contribution is your own original work (or a permitted pre-existing hotlink per §6) and is licensed under the same terms.*