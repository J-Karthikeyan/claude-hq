"""Room-socket parity: drive a live Arena and check the frames it sends back.

The same checks run against either implementation, so a divergence shows up as
a FAIL on one side and a PASS on the other rather than as a reading of two
transcripts:

    (cd backend-rs && cargo build)
    uv run python scripts/socket_parity.py              # the Rust Arena
    IMPL=py uv run python scripts/socket_parity.py      # the Python Arena

It starts its own server on a throwaway database (schema from the Rust baseline
migration, which was generated from the Alembic head), makes two users with
device tokens, and exercises presence, nudges, WebRTC signalling, lobby chat
(cleaning, the rate limit and the backlog a joiner catches up on), unknown
message types, oversized and malformed frames, every game the Arena\nadvertises, the loadtest stats route
and the Quick Play queue.

Where the two are deliberately different the check says so and asserts both
sides: today that is Mini Golf, which Python referees and the Rust Arena does
not yet, so Rust must refuse the queue rather than match people into a room
where no game ever starts.

PORT= picks the port (default 8791). The server log is kept, and printed on
a failure.
"""
import asyncio, hashlib, json, os, sqlite3, subprocess, sys, tempfile, time, uuid
import httpx, websockets

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
IMPL = os.environ.get("IMPL", "rs")
PORT = int(os.environ.get("PORT", "8791"))
BASE = f"http://127.0.0.1:{PORT}"
WS = f"ws://127.0.0.1:{PORT}"

def mkuser(db, handle, gh):
    uid = str(uuid.uuid4())
    tok = "hqd_test_" + handle
    h = hashlib.sha256(tok.encode()).hexdigest()
    c = sqlite3.connect(db)
    c.execute("INSERT INTO users (id,github_id,handle,display_name,avatar_url,trainer_name,"
              "is_active,created_at) VALUES (?,?,?,?,'','',1,datetime('now'))",
              (uid, gh, handle, handle.title()))
    c.execute("INSERT INTO devices (id,user_id,token_hash,label,revoked) VALUES (?,?,?,'t',0)",
              (str(uuid.uuid4()), uid, h))
    c.commit(); c.close()
    return uid, tok

async def ticket(tok):
    async with httpx.AsyncClient(base_url=BASE) as c:
        r = await c.post("/v1/auth/ticket", headers={"Authorization": f"Bearer {tok}"})
        r.raise_for_status()
        return r.json()["ticket"]

async def recv(ws, timeout=1.0):
    out = []
    while True:
        try:
            out.append(json.loads(await asyncio.wait_for(ws.recv(), timeout)))
        except (asyncio.TimeoutError, websockets.ConnectionClosed):
            return out

FAILS = []
def check(name, ok, detail=""):
    print(("  PASS  " if ok else "  FAIL  ") + name + (f"   {detail}" if detail and not ok else ""))
    if not ok:
        FAILS.append(name)

async def main():
    tmp = tempfile.mkdtemp(prefix="arena-parity-")
    db = os.path.join(tmp, "a.db")
    # One schema for both: the Rust baseline migration, which was generated
    # from the Alembic head and is all CREATE ... IF NOT EXISTS.
    sqlite3.connect(db).executescript(
        open(f"{REPO}/backend-rs/migrations/0001_baseline.sql").read())
    url = f"sqlite+aiosqlite:///{db}" if IMPL == "py" else f"sqlite:///{db}"
    env = dict(os.environ, ARENA_DATABASE_URL=url, ARENA_BIND_PORT=str(PORT),
               ARENA_BIND_HOST="127.0.0.1", ARENA_SECRET_KEY="parity-secret",
               ARENA_EXPOSE_REALTIME_STATS="1", RUST_LOG="warn")
    log = open(os.path.join(tmp, "server.log"), "w")
    if IMPL == "py":
        cmd = ["uv", "run", "--no-dev", "uvicorn", "app.main:app", "--host", "127.0.0.1",
               "--port", str(PORT), "--log-level", "warning"]
        cwd = f"{REPO}/backend"
    else:
        cmd = [f"{REPO}/backend-rs/target/debug/arena"]
        cwd = f"{REPO}/backend-rs"
    print(f"--- {IMPL} ---")
    proc = subprocess.Popen(cmd, env=env, cwd=cwd, stderr=log, stdout=log)
    try:
        for _ in range(200):
            try:
                if httpx.get(BASE + "/health", timeout=0.5).status_code == 200:
                    break
            except Exception:
                pass
            time.sleep(0.1)
        else:
            sys.exit("server never came up: " + open(os.path.join(tmp, "server.log")).read())

        u1, t1 = mkuser(db, "ada", 9001)
        u2, t2 = mkuser(db, "linus", 9002)
        k1, k2 = await ticket(t1), await ticket(t2)
        room = "lobby"

        async with websockets.connect(f"{WS}/v1/rooms/{room}/ws?ticket={k1}") as a:
            w1 = json.loads(await a.recv())
            check("welcome carries a chat backlog", w1.get("chat") == [], repr(w1.get("chat")))
            async with websockets.connect(f"{WS}/v1/rooms/{room}/ws?ticket={k2}") as b:
                w2 = json.loads(await b.recv())
                await recv(a, 0.3)   # drain the join event

                # --- nudge -------------------------------------------------
                await a.send(json.dumps({"type": "nudge", "to": u2, "note": "come look\x07"}))
                got_b, got_a = await recv(b, 0.6), await recv(a, 0.6)
                nud = [m for m in got_b if m.get("type") == "nudge"]
                ack = [m for m in got_a if m.get("type") == "nudge_ack"]
                check("nudge reaches the target", len(nud) == 1, repr(got_b))
                check("nudge note is stripped of control chars",
                      nud and nud[0]["note"] == "come look", repr(nud))
                check("nudge names the sender", nud and nud[0]["from"]["userId"] == u1)
                check("nudge_ack reports one delivery",
                      ack == [{"type": "nudge_ack", "to": u2, "delivered": 1}], repr(ack))
                check("the sender does not receive its own nudge",
                      not [m for m in got_a if m.get("type") == "nudge"])

                await a.send(json.dumps({"type": "nudge"}))
                e = await recv(a, 0.5)
                check("a nudge with no target is an error",
                      e == [{"type": "error", "error": "nudge needs a target userId"}], repr(e))

                # --- signal ------------------------------------------------
                await b.send(json.dumps({"type": "signal", "to": u1, "data": {"sdp": "v=0"}}))
                got_a, got_b = await recv(a, 0.6), await recv(b, 0.4)
                sig = [m for m in got_a if m.get("type") == "signal"]
                check("signal reaches only the target",
                      len(sig) == 1 and sig[0]["data"] == {"sdp": "v=0"}, repr(got_a))
                check("signal is never echoed to the sender",
                      not [m for m in got_b if m.get("type") == "signal"], repr(got_b))
                check("signal names the sender", sig and sig[0]["from"]["userId"] == u2)

                await b.send(json.dumps({"type": "signal", "to": u1}))
                e = await recv(b, 0.5)
                check("a signal with no data is an error",
                      e == [{"type": "error", "error": "signal data must be an object"}], repr(e))
                await b.send(json.dumps({"type": "signal", "data": {}}))
                e = await recv(b, 0.5)
                check("a signal with no target is an error",
                      e == [{"type": "error", "error": "signal needs a target userId"}], repr(e))

                # --- chat --------------------------------------------------
                await a.send(json.dumps({"type": "say",
                                         "data": {"kind": "chat", "text": "  hi\tthere\n\nall  "}}))
                got_b = await recv(b, 0.6)
                await recv(a, 0.3)
                chat = [m for m in got_b if m.get("type") == "say"]
                check("chat text is cleaned and folded",
                      chat and chat[0]["data"] == {"kind": "chat", "text": "hi there all"}, repr(chat))
                check("chat carries a server id and timestamp",
                      chat and len(chat[0].get("id", "")) == 12
                      and chat[0].get("at", "").endswith("+00:00"), repr(chat))

                await a.send(json.dumps({"type": "say", "data": {"kind": "chat", "text": "   "}}))
                e = await recv(a, 0.5)
                check("an empty chat message is an error",
                      e == [{"type": "error", "error": "chat: empty message"}], repr(e))
                await a.send(json.dumps({"type": "say", "data": {"kind": "chat", "text": 7}}))
                e = await recv(a, 0.5)
                check("a non-string chat message is an error",
                      e == [{"type": "error", "error": "chat: text must be a string"}], repr(e))

                # str.isprintable() rejects bidi overrides, zero-width spaces
                # and joiners, the BOM and private-use code points; none of it
                # may ride along in text people read.
                sneaky = "hi\u202ereversed\u200b\ufeff\U000f0000 there"
                await a.send(json.dumps({"type": "say",
                                         "data": {"kind": "chat", "text": sneaky}}))
                got_b = await recv(b, 0.6)
                await recv(a, 0.3)
                chat = [m for m in got_b if m.get("type") == "say"]
                check("chat strips invisible format and private-use characters",
                      chat and chat[0]["data"]["text"] == "hi reversed there",
                      repr(chat[0]["data"] if chat else got_b))

                # Two chats are already spent, so six of these eight land.
                for i in range(8):
                    await a.send(json.dumps({"type": "say", "data": {"kind": "chat", "text": f"m{i}"}}))
                got_a = await recv(a, 0.8)
                errs = [m for m in got_a if m.get("type") == "error"]
                check("the ninth message in ten seconds is rate-limited",
                      len(errs) == 2 and errs[0]["error"]
                      == "chat: slow down \u2014 at most 8 messages every 10 seconds",
                      repr(errs))
                await recv(b, 0.4)

                # --- ping --------------------------------------------------
                await a.send(json.dumps({"type": "ping"}))
                got_a, got_b = await recv(a, 0.5), await recv(b, 0.4)
                check("ping answers the asking socket",
                      got_a == [{"type": "pong"}], repr(got_a))
                check("pong is not broadcast to the room",
                      not [m for m in got_b if m.get("type") == "pong"], repr(got_b))

                # --- shared state ------------------------------------------
                await a.send(json.dumps({"type": "state", "patch": {"turn": 3}}))
                got_b = await recv(b, 0.6)
                await recv(a, 0.3)
                st = [m for m in got_b if m.get("type") == "state"]
                check("a state patch merges and is broadcast",
                      st and st[0]["state"] == {"turn": 3}
                      and st[0]["by"]["userId"] == u1, repr(got_b))

                await a.send(json.dumps({"type": "state", "patch": "nope"}))
                e = await recv(a, 0.5)
                check("a non-object state patch is an error",
                      e == [{"type": "error", "error": "patch must be an object"}], repr(e))

                # 64 KiB of shared state, reached in frames that each fit under
                # the 16 KiB frame cap, so this tests the state cap and not that one.
                over = None
                for i in range(6):
                    await a.send(json.dumps({"type": "state",
                                             "patch": {f"k{i}": "x" * 15000}}))
                    for m in await recv(a, 0.6):
                        if m.get("type") == "error":
                            over = m
                    if over:
                        break
                check("a state patch over the cap is an error",
                      over == {"type": "error", "error": "state too large"}, repr(over))
                await recv(b, 0.6)

                # --- frame intake ------------------------------------------
                await a.send(json.dumps({"type": "ping", "pad": "x" * 17000}))
                e = await recv(a, 0.6)
                check("an oversized frame is answered, not dropped",
                      e == [{"type": "error", "error": "frame too large"}], repr(e))
                await a.send("{not json")
                e = await recv(a, 0.6)
                check("malformed json is answered, not dropped",
                      e == [{"type": "error", "error": "malformed json"}], repr(e))

                # --- unknown type ------------------------------------------
                await a.send(json.dumps({"type": "wat"}))
                e = await recv(a, 0.5)
                check("an unknown message type is answered",
                      e == [{"type": "error", "error": "unknown message type: 'wat'"}], repr(e))

            # --- chat history for a joiner ---------------------------------
            k3 = await ticket(t2)
            async with websockets.connect(f"{WS}/v1/rooms/{room}/ws?ticket={k3}") as c:
                w3 = json.loads(await c.recv())
                texts = [m["data"]["text"] for m in w3.get("chat", [])]
                # Eight: the allowance is 8 per 10s and ten chats were sent,
                # so the last two were refused and never stored.
                check("a joiner catches up on the backlog, oldest first",
                      texts == ["hi there all", "hi reversed there"]
                      + [f"m{i}" for i in range(6)], repr(texts))

        # --- every game the Arena advertises, and a join with each ---------
        # The welcome's `arena` block is what games/multi.js gates each panel
        # on: a game missing from it makes the page say "This Arena doesn't run
        # <game> yet", however well the engine works.
        GAMES = ["pond", "race", "duel", "mines", "farm", "golf", "kart", "plat",
                 "fps", "strike", "hq", "type"]
        # Refereed only by the Rust Arena (HQ 2.5): left out of the order check
        # and the per-game join loop, so both Arenas still compare equal.
        RUST_ONLY = ("td", "bowl")
        # The event a join answers with, beyond the shared "lobby" broadcast.
        # From valley.py:1036-1076. race answers only when a round is already
        # running, so it has none on a fresh join.
        SNAP = {"pond": "pond", "duel": "duel", "mines": "mines", "farm": "farm",
                "golf": "golf", "kart": "kart", "plat": "plat", "fps": "fps", "strike": "strike",
                "hq": "snap", "type": "type", "race": None}

        k4 = await ticket(t1)
        async with websockets.connect(f"{WS}/v1/rooms/{room}/ws?ticket={k4}") as d:
            w = json.loads(await d.recv())
            info = w.get("arena") or {}
            check("the welcome advertises every game, in order",
                  [k for k in (info.get("games") or {}) if k not in RUST_ONLY] == GAMES,
                  repr(list((info.get("games") or {}).keys())))
            check("kart is still protocol 2 with its caps",
                  (info.get("games") or {}).get("kart") == {"v": 2, "caps": ["scale", "tracks"]},
                  repr((info.get("games") or {}).get("kart")))
            check("the welcome carries the party playlist",
                  info.get("party") == {"v": 1, "order": ["kart", "plat", "fps", "golf"]},
                  repr(info.get("party")))

            for g in GAMES:
                if g == "hq" or g in RUST_ONLY:
                    continue        # hq needs an hq_ room; done separately below
                await d.send(json.dumps({"type": "game", "g": g, "op": "join"}))
                got = await recv(d, 1.2)
                evs = [m.get("ev") for m in got if m.get("g") == g]
                errs = [m for m in got if m.get("ev") == "error"]
                check(f"{g}: join is accepted",
                      not errs and "lobby" in evs, repr(got)[:300])
                want = SNAP[g]
                if want:
                    check(f"{g}: join answers with its {want} snapshot",
                          want in evs, repr(evs))

            # A game nothing runs is still refused, by name.
            await d.send(json.dumps({"type": "game", "g": "chess", "op": "join"}))
            got = await recv(d, 0.8)
            check("an unknown game is refused",
                  [m for m in got if m.get("ev") == "error"
                   and m.get("error") == "unknown game"], repr(got)[:200])

            # Party Mode is not a Valley game and has no lobby: it is
            # short-circuited ahead of every other check (valley.py:1000).
            await d.send(json.dumps({"type": "game", "g": "party", "op": "view"}))
            got = await recv(d, 0.8)
            party = [m for m in got if m.get("g") == "party"]
            check("party answers without joining any lobby",
                  party and not [m for m in party if m.get("ev") == "error"],
                  repr(got)[:300])

        # HQ presence lives in an HQ room, and says so anywhere else.
        k5 = await ticket(t1)
        async with websockets.connect(f"{WS}/v1/rooms/hq_{u1}/ws?ticket={k5}") as h:
            await h.recv()
            await h.send(json.dumps({"type": "game", "g": "hq", "op": "join"}))
            got = await recv(h, 1.2)
            evs = [m.get("ev") for m in got if m.get("g") == "hq"]
            check("hq: join inside an HQ room answers with a presence snapshot",
                  "snap" in evs and "lobby" in evs, repr(got)[:300])
        k6 = await ticket(t1)
        async with websockets.connect(f"{WS}/v1/rooms/{room}/ws?ticket={k6}") as h2:
            await h2.recv()
            await h2.send(json.dumps({"type": "game", "g": "hq", "op": "join"}))
            got = await recv(h2, 0.8)
            check("hq: refused outside an HQ room, with Python's wording",
                  [m for m in got if m.get("error") == "HQ presence lives in an HQ room"],
                  repr(got)[:200])

        # --- the env-gated loadtest route ---------------------------------
        async with httpx.AsyncClient(base_url=BASE) as c:
            r = await c.get("/v1/realtime/stats")
            check("GET /v1/realtime/stats answers when the flag is set",
                  r.status_code == 200 and set(r.json()) == {"running", "max", "rooms"},
                  f"{r.status_code} {r.text[:120]}")
            r = await c.post("/v1/quickplay/join", json={"game": "golf"},
                             headers={"Authorization": f"Bearer {t1}"})
            # Both Arenas referee Mini Golf now, so both must queue it. This
            # was the one deliberate divergence while the Rust Arena had no
            # golf engine; it is gone.
            check("Quick Play queues Golf on both Arenas",
                  r.json().get("state") == "waiting", f"{r.status_code} {r.text[:160]}")
            r = await c.post("/v1/quickplay/join", json={"game": "chess"},
                             headers={"Authorization": f"Bearer {t1}"})
            check("Quick Play still refuses a game nobody runs",
                  r.json().get("state") == "error"
                  and r.json().get("error", "").startswith("Quick Play has"),
                  f"{r.status_code} {r.text[:160]}")
            r = await c.post("/v1/quickplay/join", json={"game": "kart"},
                             headers={"Authorization": f"Bearer {t1}"})
            check("Quick Play still queues a game it can run",
                  r.status_code == 200 and r.json().get("state") == "waiting",
                  f"{r.status_code} {r.text[:160]}")
    finally:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()
        log.close()
    print()
    if FAILS:
        print(f"{len(FAILS)} failed: " + ", ".join(FAILS))
        print("--- server log ---")
        print(open(os.path.join(tmp, "server.log")).read()[-2000:])
        sys.exit(1)
    print("all checks passed")

asyncio.run(main())
