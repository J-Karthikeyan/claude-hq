# HQ Notch

Your Claude HQ party, living in the Mac's notch. Every live Claude Code session shows up as
the **same Pokémon HQ gives it**. It hops while it works, bounces with a `!` when it needs
you, jumps when it's done, and gets knocked out (grey) when a turn fails. Rest the cursor on the notch for half a second to
see every session: what it's doing right now, its last prompt, and a live ticker of tool
calls with `+N −M` line counts for edits.

When Claude asks for permission, the notch opens on its own. Answer from there:
**Allow**, **Always** (adds the rule Claude Code suggests), **Deny**, or **Terminal** to answer
in the CLI as usual. `AskUserQuestion` prompts show their choices (single or multi-select)
and you can answer them right in the notch. A plan waiting for approval (plan mode's
`ExitPlanMode`) shows as a **plan card**: read the whole plan, then **Approve**, **Keep
planning** (Claude stays in plan mode) or **Terminal**. Click a session card to jump to its
terminal window (kitty, Terminal, iTerm2, tmux); click an edit in its ticker (the `›` rows) to
see what changed: removed lines in red, added in green, a couple of unchanged lines around them.

Away from the Mac? Turn on **Phone alerts** in Settings and subscribe to your topic in the
[ntfy](https://ntfy.sh) app: you get a push when a session needs you, finishes or fails (see
Privacy below for exactly what is sent).

macOS 14+, Apple silicon or Intel. Builds with the Xcode Command Line Tools alone.

## Install

```sh
python3 notch/tools/install.py            # build, install, add hooks, start at login
python3 notch/tools/install.py --no-login # same, without the login item
python3 notch/tools/install.py --status
python3 notch/tools/install.py --uninstall
```

The installer builds with `swift build -c release`, puts `HQ Notch.app` in
`~/Applications`, copies the hook relay to `~/Library/Application Support/HQNotch/`, and adds
one hook entry per event (plus the `--ask` question hook) to `~/.claude/settings.json`. It backs the file up first
(`settings.json.bak-hqnotch-<time>`) and only ever adds or removes entries that point at its
own relay. New Claude Code sessions report to the notch right away. Sessions already open
pick the hooks up once they reload their settings.

Claude HQ (`python3 dashboard.py`) is optional. When it is running, the notch reads
`/api/sessions` on `127.0.0.1` for each session's creature, title and prompt count, and lists
live sessions that started before the notch. Without HQ, the notch computes the same
species/stage/shiny itself (it reads HQ's `.dex-seed` for the shiny roll when it can find
it).

## How it works

```
Claude Code ──hooks──▶ hq_notch_hook.py ──Unix socket──▶ HQ Notch.app
                                ▲  allow / always / deny / answers ◀──┘   ├─ GET 127.0.0.1:8765/api/sessions
                                                                          └─ sprite cache ~/Library/Caches/HQNotch
```

- **Never blocks Claude Code.** Every event except `PermissionRequest` is fire-and-forget
  with a 0.3 s budget. A `PermissionRequest` waits for the notch for up to 115 s, then prints
  nothing, so Claude Code asks in the terminal exactly as it would without the notch. If the
  app isn't running, the socket is gone, or the JSON is bad, the hook prints nothing and exits 0.
- **Questions are caught twice.** A blocking `PreToolUse` hook matching `AskUserQuestion`
  (`hq_notch_hook.py --ask`) shows them first; an answer there allows the tool, so no permission
  step follows. If you send a question to the terminal instead, a `PermissionRequest` for that
  same question within a minute is passed straight through rather than asked again.
- **Answers are checked twice.** The relay only forwards an `AskUserQuestion` answer that
  answers exactly the questions asked, with labels from their options (`answers_fit`).
- **Local only.** The socket lives in a `0700` directory and is `0600`, and the app checks
  every peer's uid. The only network requests are the sprite hotlinks HQ already uses
  (jsDelivr's mirror of PokeAPI, and PkParaíso for the 3D pack; the URL carries only a Dex
  number or a species slug) and HQ on
  `127.0.0.1`. Prompts, commands and file paths never leave the machine. (Edit diffs and plans
  travel the same local socket, capped: 4 000 characters per side of an edit, 8 edits, 12 000
  for a plan.)
- **Phone alerts (opt-in, off by default).** When on, the app POSTs to `<server>/<topic>`
  (default server `https://ntfy.sh`; any `http(s)` ntfy server you run works, and the URL
  may not carry credentials, a query or a fragment). The body and the `Title` / `Tags` /
  `Priority` headers are fixed strings, one of: "A Claude session needs you", "A Claude
  session finished", "A Claude session had an error" (plus "Test from HQ Notch" when you press
  *Send a test*). No prompt, title, folder, command, file path, session id or count goes in.
  At most one push of each kind per 30 s; with *Only when I'm away from the Mac*, none while
  you used the keyboard or mouse in the last 2 minutes. The topic is `hq-` and 20 random
  letters (100 bits from `SecRandomCopyBytes`), made once per Mac; anyone who knows it can read
  the alerts, so keep it to yourself, or press *New* for another. The ntfy server sees your IP
  address and when you get alerts.
- **Same Pokémon as HQ.** `Creature.swift` ports `creature_for()` / `shiny_for_species()`
  from `dashboard.py` and `pokeDexFor()` from `ui/app/06-trainer-packs.js`. The evolution
  lines are generated from the JS (`tools/gen_pokedata.py`). Like HQ's cards, a species never
  shows an earlier form than the highest stage you've reached with it: the notch reads each
  species' `maxStage` from HQ's `/api/pokedex` (at most once a minute, remembered while HQ is
  down). Not mirrored: manual branch picks (Eevee etc.), which live only in the browser.
- **2D or 3D, your pick.** Settings → Sprites: **2D** (animated Gen 5; Apex = the line's final
  form), **3D** (HQ's `pokemon3d` pack, with the Mega form at Apex when the species has one,
  same default X/Y pick as HQ), or **Match HQ** (follows HQ's own `creaturePack`).
- **Sprites are hotlinked, never bundled.** They are downloaded on first use and cached in
  `~/Library/Caches/HQNotch`. If the CDN is blocked, an original drawn blob takes their place.

Settings: the gear in the notch's header. Sprites (2D / 3D / Match HQ, with a live preview of
your sessions), sounds, show idle sessions, approve from the notch (off = every request goes
straight to the terminal), phone alerts (server, topic with Copy / New, away-only, Send a
test), quit. A permission prompt always closes settings (and a diff) so it is never hidden.
The menu bar icon (hexagon) has the sounds / idle / approvals toggles.

## Develop

```sh
swift build -c release --package-path notch
python3 notch/tools/parity_vectors.py > /tmp/vectors.json            # needs node
notch/.build/release/hqnotch-selftest /tmp/vectors.json              # core + parity with HQ
python3 -m unittest tests.test_notch_hook tests.test_notch_install   # relay + installer
python3 notch/tools/gen_pokedata.py          # after changing HQ's evolution lines
```

`hqnotch-selftest` stands in for XCTest, which the Command Line Tools don't ship. Debug aids
for checking the UI without clicking (both off unless the variable is set):
`HQNOTCH_SNAPSHOT_DIR=<dir>` writes PNGs of the panel's own views (closed, then expanded),
with no Screen Recording permission needed. `HQNOTCH_DEBUG_REPLY=allow|always|deny|answer|pass`
answers each request after 3 s, as a click would. `HQNOTCH_DEBUG_LOG=1` logs the store after
every hook message.

The `.app` is ad-hoc signed, which is fine on your own Mac. Sharing a build needs a
Developer ID signature.
