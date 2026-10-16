"""
Valley multiplayer: server-refereed minigames on top of the room websockets.

Clients send {"type": "game", "g": <game>, "op": <op>, ...}; this module owns the
rules and answers with {"type": "game", "g": <game>, "ev": <event>, ...}. Every game
has a lobby (who is in it, who hosts) with join notices and invites. The games:

  pond   shared fishing dock: the server picks each bite, times the reel and scores
         the catch; a room goal; a boss fish the whole lobby reels in together.
  race   live code-puzzle race: the server holds the word and marks each guess.
  duel   1v1 Pokemon-style battle with real moves/stats (app/pokebattle.py): both pick
         a move or switch at once, the server resolves the turn (priority, speed,
         accuracy, crits, damage, status) with its own RNG and broadcasts the events.
  mines  co-op mine floor: one shared grid, server-side loot and slimes.
  farm   one shared garden per room, stored in the database and watered by the
         members' real published activity (daily_stats prompts).
  golf   3D mini golf: the server rolls every putt with integer physics (app/golf.py)
         that the clients replay identically; walking positions are relayed to the
         lobby only, at most ~10 per second per player.
  kart   kart racing (app/kart.py), the first real-time game: each player drives
         locally and streams positions; a fixed-rate server tick (app/realtime.py)
         checks them, counts laps and sends the lobby one snapshot per tick.
  plat   platformer rush (app/platformer.py): the same real-time loop; each player runs
         and jumps locally, the server checks every move against the level, owns the
         checkpoints, the coins (per player in a race, once for the room in co-op) and
         the flag, and sends respawns back to the last checkpoint.
  fps    Blaster Arena (app/fps.py): a first-person free-for-all on the same 20 Hz
         tick; the server checks every move and judges every shot with lag
         compensation (it rewinds the targets to what the shooter saw).

Only game state lives here; nothing transcript-derived ever reaches this server.
State is in process memory except the farm (same single-instance trade as rooms.py).
"""
import asyncio
import json
import random
import re
import secrets
import time
from datetime import UTC, date, datetime, timedelta
from typing import Any

from sqlalchemy import func, select

from . import golf as golfmod
from . import fps as fpsmod
from . import strike as strikemod
from . import hqpresence as hqmod
from . import typerace as typemod
from . import results as resultsmod
from . import party as partymod
from . import kart as kartmod
from . import platformer as platmod
from . import realtime
from . import pokebattle as pb
from .db import SessionLocal
from .models import DailyStat, RoomFarm
from .rooms import Member, Room, manager

GAMES = ("pond", "race", "duel", "mines", "farm", "golf", "kart", "plat", "fps", "strike", "hq", "type")

# Protocol version + capabilities per game. Sent in the room's welcome (so a client
# can tell "this Arena is too old for me" from "I am too old for this Arena" before it
# joins) and as "pv" on every game message. Bump v when a message changes shape in a
# way an older client or server would misread; add a cap for an optional extra.
# Keep in step with backend-rs (arena_info) and games/multi.js (CLIENT_PROTO).
PROTOCOL: dict[str, dict] = {g: {"v": 1, "caps": []} for g in GAMES}
PROTOCOL["kart"] = {"v": 2, "caps": ["scale", "tracks"]}   # v2: geometry scale + server track list
# strike is the one game where the two referees genuinely differ: the frozen Python one
# judges hitscan only (v1), the Rust one (backend-rs, protocol.rs) also flies the launcher's
# rockets (v2). test_protocol.py allows exactly this pair.
PROTOCOL["strike"] = {"v": 1, "caps": []}


def arena_info() -> dict:
    """What this Arena runs, for the room welcome."""
    return {"impl": "py", "games": {g: dict(PROTOCOL[g]) for g in GAMES}, "party": {"v": 1, "order": list(partymod.ORDER)}}
GAME_NAMES = {"pond": "Fishing Pond", "race": "Puzzle Race", "duel": "Creature Duel",
              "mines": "Co-op Mines", "farm": "Shared Farm", "golf": "Mini Golf",
              "kart": "Kart Racing", "plat": "Platformer Rush", "fps": "Blaster Arena", "strike": "Strikezone",
              "hq": "HQ", "type": "Code Typing Race"}
MAX_LOBBY = 8
SEND_TIMEOUT = 0.5            # seconds one socket may take to accept a lobby fan-out
now = time.monotonic          # patched in tests
HOST_GRACE = golfmod.GRACE    # a host back from a socket drop this soon is host again
wall = time.time              # patched in tests

# ----------------------------------------------------------------- catalogs --
# Mirrors games/core.js (fish rarity) and the crop table; keep in sync.
FISH = {
    "minnow": 1, "perch": 1, "carp": 1, "mudcat": 1, "bream": 2, "trout": 2, "pike": 2,
    "eel": 2, "sunfish": 2, "koi": 3, "sturgeon": 3, "angler": 3, "glowfin": 3,
    "lumen": 4, "leviathan": 4, "ghostfish": 4,
}
FISH_WEIGHT = {1: 10, 2: 5, 3: 2, 4: 0.6}
FISH_POINTS = {1: 1, 2: 3, 3: 8, 4: 20}
MIN_REEL_MS = {1: 1500, 2: 2200, 3: 3000, 4: 4000}   # faster than this is not a real reel
MAX_REEL_MS = 90_000
POND_GOAL = 20
BOSS_EVERY = 8                 # a boss surfaces every 8th room catch
BOSS_SECS = 60
BOSS_HP_PER_PLAYER = 40
PULL_RATE = 8                  # pulls per second per player, at most
POND_OP_RATE = 6               # cast / hook / land / lose per second per player, at most
CHEST_CHANCE = 0.15            # a cast with a treasure chest on the reel
CHEST_LOOT = (("quartz", 50), ("copper", 25), ("amethyst", 12), ("gold", 8), ("emerald", 4), ("ruby", 1))
HOOK_EARLY_MS = 250            # a hook this much before the bite still counts (latency)
CROPS = {   # id: (hours, water per gardener, season)
    "radish": (4, 6, "spring"), "pea": (6, 8, "spring"), "tulip": (8, 10, "spring"),
    "tomato": (6, 10, "summer"), "melon": (12, 16, "summer"), "corn": (10, 12, "summer"),
    "pumpkin": (12, 16, "autumn"), "grape": (8, 12, "autumn"), "kale": (6, 8, "winter"),
}
FARM_PLOTS = 9
FARM_SEEDS_PER_DAY = 6
MINE_COLS, MINE_ROWS, MINE_MAX_DEPTH = 12, 8, 12
RACE_GUESSES, RACE_SECS = 6, 180
RACE_WORDS = (
    'admin',
    'agent',
    'alias',
    'array',
    'async',
    'audit',
    'await',
    'batch',
    'bench',
    'blobs',
    'brand',
    'build',
    'bytes',
    'cache',
    'catch',
    'chain',
    'chmod',
    'class',
    'clone',
    'close',
    'cloud',
    'codec',
    'const',
    'count',
    'crash',
    'crate',
    'cycle',
    'dates',
    'debit',
    'debug',
    'defer',
    'delta',
    'draft',
    'drive',
    'embed',
    'entry',
    'epoch',
    'error',
    'event',
    'fetch',
    'field',
    'flags',
    'float',
    'flush',
    'frame',
    'graph',
    'group',
    'guard',
    'heaps',
    'hooks',
    'index',
    'infer',
    'input',
    'istio',
    'items',
    'joins',
    'kafka',
    'label',
    'latch',
    'layer',
    'limit',
    'lines',
    'linux',
    'local',
    'logic',
    'loops',
    'macro',
    'merge',
    'model',
    'mount',
    'mutex',
    'nginx',
    'nodes',
    'order',
    'owner',
    'paged',
    'parse',
    'patch',
    'pivot',
    'pixel',
    'popup',
    'ports',
    'print',
    'proxy',
    'qubit',
    'query',
    'queue',
    'ratio',
    'react',
    'redis',
    'regex',
    'rerun',
    'reset',
    'route',
    'rules',
    'scope',
    'serde',
    'shape',
    'shard',
    'shell',
    'sigma',
    'sleep',
    'slice',
    'spawn',
    'split',
    'stack',
    'state',
    'stdin',
    'store',
    'style',
    'swift',
    'table',
    'tasks',
    'tests',
    'timer',
    'token',
    'tools',
    'trace',
    'trees',
    'tuple',
    'types',
    'union',
    'unzip',
    'utils',
    'value',
    'vault',
    'views',
    'watch',
    'where',
    'while',
    'write',
    'xpath',
    'yield',
    'zones',
)


def season_of(d: date) -> str:
    m = d.month
    return "winter" if m in (12, 1, 2) else "spring" if m <= 5 else "summer" if m <= 8 else "autumn"


def _clip(v: Any, n: int) -> str:
    return "".join(ch for ch in v if ch.isprintable()).strip()[:n] if isinstance(v, str) else ""


# ------------------------------------------------------------------ outbox --
class Out:
    """Messages to send after a rule ran: to the whole room, one socket, or one user."""

    def __init__(self, g: str):
        self.g = g
        self.items: list[tuple[str, Any, dict]] = []

    def _m(self, ev: str, data: dict) -> dict:
        pv = PROTOCOL.get(self.g, {}).get("v", 1)
        return {"type": "game", "g": self.g, "ev": ev, "pv": pv, **data}

    def all(self, ev: str, **data: Any) -> None:
        self.items.append(("all", None, self._m(ev, data)))

    def to(self, ws: Any, ev: str, **data: Any) -> None:
        self.items.append(("ws", ws, self._m(ev, data)))

    def user(self, user_id: str, ev: str, **data: Any) -> None:
        self.items.append(("user", user_id, self._m(ev, data)))

    def lobby(self, member_ids: Any, ev: str, skip_user: str | None = None, **data: Any) -> None:
        """Only to the sockets of these lobby members (optionally not back to one user)."""
        self.items.append(("lobby", (frozenset(member_ids), skip_user), self._m(ev, data)))

    def err(self, ws: Any, msg: str) -> None:
        self.to(ws, "error", error=msg)


# ------------------------------------------------------------------- lobby --
class Lobby:
    def __init__(self) -> None:
        self.members: dict[str, dict] = {}     # user_id -> public profile, in join order
        self.host: str | None = None
        # (user_id, when) of a host whose socket dropped: a quick rejoin takes the host back
        self.prev_host: tuple[str, float] | None = None
        # user_id -> when their socket dropped: a quick rejoin is a reconnect, not a "joined" toast
        self.blips: dict[str, float] = {}

    def roster(self) -> list[dict]:
        return [dict(p, host=(uid == self.host)) for uid, p in self.members.items()]


class RoomValley:
    def __init__(self, room_id: str):
        self.room_id = room_id
        self.lobbies = {g: Lobby() for g in GAMES}
        self.pond = Pond()
        self.race = Race()
        self.duel = Duel()
        self.mines = Mines()
        self.golf = golfmod.Golf()
        self.kart = kartmod.Kart()
        self.plat = platmod.Plat()
        self.fps = fpsmod.Fps()
        self.strike = strikemod.Strike()
        self.hq = hqmod.Presence()
        self.type = typemod.TypeRace()


_rooms: dict[str, RoomValley] = {}


def valley_for(room_id: str) -> RoomValley:
    v = _rooms.get(room_id)
    if v is None:
        v = _rooms[room_id] = RoomValley(room_id)
    return v


# -------------------------------------------------------------------- pond --
class Pond:
    def __init__(self) -> None:
        self.casts: dict[str, dict] = {}       # user_id -> {token, fish, bite_ms, at, aim, hooked, chest}
        self.scores: dict[str, int] = {}
        self.goal = 0
        self.catches = 0
        self.boss: dict | None = None
        self.pulls: dict[str, list[float]] = {}
        self.ops: dict[str, list[float]] = {}  # recent line ops per player (rate limit)
        self.seq = 0                   # public cast id: lets every page match events to a line
        self.last_boss_push = 0.0
        self.boss_at = 0               # catch count that last spawned a boss

    def snapshot(self) -> dict:
        # "casting" stays for older pages; "lines" lets a late joiner draw every line.
        return {"casting": list(self.casts), "scores": self.scores, "goal": self.goal,
                "goalTarget": POND_GOAL, "boss": self._boss_view(),
                "lines": [{"userId": uid, "id": c["id"], "aim": c["aim"], "hooked": c["hooked"]}
                          for uid, c in self.casts.items()]}

    def _boss_view(self) -> dict | None:
        b = self.boss
        if not b:
            return None
        return {"fish": b["fish"], "hp": max(0, round(b["hp"])), "max": b["max"],
                "left": max(0, round(b["until"] - now()))}

    def _expire_boss(self, out: Out) -> None:
        if self.boss and now() > self.boss["until"]:
            out.all("bossgone", fish=self.boss["fish"])
            self.boss = None
            self.pulls = {}

    def throttled(self, user_id: str) -> bool:
        """At most POND_OP_RATE line ops per second per player; the rest are dropped."""
        t = now()
        mine = [x for x in self.ops.get(user_id, []) if t - x < 1.0]
        if len(mine) >= POND_OP_RATE:
            self.ops[user_id] = mine
            return True
        mine.append(t)
        self.ops[user_id] = mine
        return False

    def cast(self, m: Member, msg: dict, out: Out, rng: random.Random) -> None:
        self._expire_boss(out)
        old = self.casts.pop(m.user_id, None)
        if old:                      # a line left out by an earlier socket or tab: reel it in for everyone
            out.all("lost", user=m.public(), id=old["id"], fish=old["fish"])
        aim = msg.get("aim")
        if isinstance(aim, (int, float)) and not isinstance(aim, bool) and aim == aim:
            aim = round(min(1.0, max(0.0, float(aim))), 3)
        else:
            aim = round(rng.random(), 3)
        ids = list(FISH)
        fish = rng.choices(ids, weights=[FISH_WEIGHT[FISH[f]] for f in ids])[0]
        self.seq += 1
        c = {"token": secrets.token_urlsafe(8), "id": self.seq, "fish": fish, "bite_ms": rng.randint(800, 3000),
             "at": now(), "aim": aim, "hooked": False, "chest": rng.random() < CHEST_CHANCE}
        self.casts[m.user_id] = c
        out.to(m.ws, "cast", token=c["token"], id=c["id"], fish=fish, rarity=FISH[fish], biteIn=c["bite_ms"],
               chest=c["chest"])
        out.all("casting", user=m.public(), id=c["id"], aim=aim)

    def hook(self, m: Member, msg: dict, out: Out) -> None:
        """Cosmetic: the angler struck on the bite, so the others see the fight start."""
        c = self.casts.get(m.user_id)
        if not c or msg.get("token") != c["token"] or c["hooked"]:
            return
        if (now() - c["at"]) * 1000 < c["bite_ms"] - HOOK_EARLY_MS:
            out.err(m.ws, "not yet")
            return
        c["hooked"] = True
        out.all("hooked", user=m.public(), id=c["id"])

    def land(self, m: Member, msg: dict, out: Out, rng: random.Random) -> None:
        self._expire_boss(out)
        c = self.casts.get(m.user_id)
        if not c or msg.get("token") != c["token"]:
            out.err(m.ws, "no such cast")
            return
        r = FISH[c["fish"]]
        elapsed = (now() - c["at"]) * 1000
        if elapsed < c["bite_ms"] + MIN_REEL_MS[r]:
            out.err(m.ws, "too fast — that wasn't a real reel")
            return
        del self.casts[m.user_id]
        if elapsed > c["bite_ms"] + MAX_REEL_MS:
            out.all("lost", user=m.public(), id=c["id"], fish=c["fish"])
            return
        perfect = msg.get("perfect") is True     # cosmetic only: the reel runs on the client, so it can't score
        pts = FISH_POINTS[r]
        if msg.get("chest") is True and c["chest"]:
            items, weights = zip(*CHEST_LOOT)
            out.to(m.ws, "loot", item=rng.choices(items, weights=weights)[0])
        self.scores[m.user_id] = self.scores.get(m.user_id, 0) + pts
        self.goal += 1
        self.catches += 1
        out.all("caught", user=m.public(), id=c["id"], fish=c["fish"], points=pts, perfect=perfect,
                scores=self.scores, goal=self.goal, goalTarget=POND_GOAL)
        if self.goal >= POND_GOAL:
            out.all("goal", reward="gold")
            self.goal = 0

    def lose(self, m: Member, msg: dict, out: Out) -> None:
        c = self.casts.get(m.user_id)
        if c and msg.get("token") == c["token"]:
            del self.casts[m.user_id]
            out.all("lost", user=m.public(), id=c["id"], fish=c["fish"])

    def maybe_boss(self, lobby: Lobby, out: Out) -> None:
        if self.boss or not self.catches or self.catches % BOSS_EVERY or self.catches == self.boss_at:
            return
        self.boss_at = self.catches
        players = max(1, len(lobby.members))
        hp = BOSS_HP_PER_PLAYER * players
        self.boss = {"fish": "leviathan", "hp": float(hp), "max": hp, "until": now() + BOSS_SECS}
        self.pulls = {}
        out.all("boss", boss=self._boss_view())

    def pull(self, m: Member, out: Out) -> None:
        self._expire_boss(out)
        b = self.boss
        if not b:
            return
        t = now()
        mine = [x for x in self.pulls.get(m.user_id, []) if t - x < 1.0]
        if len(mine) >= PULL_RATE:
            return
        mine.append(t)
        self.pulls[m.user_id] = mine
        together = sum(1 for xs in self.pulls.values() if any(t - x < 1.0 for x in xs))
        b["hp"] -= 1 + 0.5 * (together - 1)          # more people pulling at once = stronger
        if b["hp"] <= 0:
            helpers = [uid for uid, xs in self.pulls.items() if xs]
            for uid in helpers:
                self.scores[uid] = self.scores.get(uid, 0) + 10
            out.all("bossdown", fish=b["fish"], helpers=helpers, scores=self.scores, reward="leviathan")
            self.boss = None
            self.pulls = {}
        elif t - self.last_boss_push > 0.15:
            self.last_boss_push = t
            out.all("bosshp", boss=self._boss_view(), pulling=together)

    def drop(self, user_id: str) -> dict | None:
        """Forget a player who left; returns the line they still had out, if any."""
        self.pulls.pop(user_id, None)
        self.ops.pop(user_id, None)
        return self.casts.pop(user_id, None)


# -------------------------------------------------------------------- race --
def mark(guess: str, word: str) -> list[str]:
    res = ["miss"] * 5
    left: list[str | None] = list(word)
    for i in range(5):
        if guess[i] == word[i]:
            res[i] = "hit"
            left[i] = None
    for i in range(5):
        if res[i] != "hit" and guess[i] in left:
            res[i] = "near"
            left[left.index(guess[i])] = None
    return res


class Race:
    def __init__(self) -> None:
        self.round = 0
        self.word: str | None = None
        self.guesses: dict[str, int] = {}
        self.until = 0.0
        self.wins: dict[str, int] = {}

    def running(self) -> bool:
        return self.word is not None and now() < self.until

    def start(self, m: Member, out: Out, rng: random.Random) -> None:
        if self.running():
            out.err(m.ws, "a race is already running")
            return
        self.round += 1
        self.word = rng.choice(RACE_WORDS)
        self.guesses = {}
        self.until = now() + RACE_SECS
        out.all("start", round=self.round, by=m.public(), secs=RACE_SECS)

    def guess(self, m: Member, msg: dict, out: Out) -> None:
        if not self.running() or msg.get("round") != self.round:
            if self.word and not self.running():
                out.all("timeout", round=self.round, word=self.word)
                self.word = None
            out.err(m.ws, "no race running")
            return
        g = msg.get("word")
        if not isinstance(g, str) or not re.fullmatch(r"[a-z]{5}", g):
            out.err(m.ws, "five letters, a-z")
            return
        n = self.guesses.get(m.user_id, 0)
        if n >= RACE_GUESSES:
            out.err(m.ws, "out of guesses")
            return
        self.guesses[m.user_id] = n + 1
        marks = mark(g, self.word)
        out.to(m.ws, "mark", round=self.round, word=g, marks=marks, n=n + 1)
        out.all("progress", round=self.round, user=m.public(), n=n + 1,
                hits=marks.count("hit"))     # how close, never which letters
        if g == self.word:
            self.wins[m.user_id] = self.wins.get(m.user_id, 0) + 1
            out.all("win", round=self.round, user=m.public(), word=self.word, n=n + 1, wins=self.wins)
            self.word = None


# -------------------------------------------------------------------- duel --
DUEL_FIRST_SECS = 60           # the first choice (time to read the screen)
DUEL_CHOOSE_SECS = 30          # each later move-or-switch choice; then the server picks for you
DUEL_REPLACE_SECS = 30         # to send in the next creature after a faint
DUEL_GRACE_SECS = 20           # a dropped connection may come back within this and play on
DUEL_AFK_PICKS = 2             # consecutive server picks for one side = that side forfeits
DUEL_SLACK_SECS = 2            # a pick sent as a visible countdown ends still counts (network trip)
DUEL_ANIM_SECS = 2             # extra choosing time per animated move/switch/faint of the last turn
DUEL_ANIM_CAP = 12             # ...capped, so a long turn never stalls the match
DUEL_RESULT_SECS = 600         # an ending a dropped player missed is replayed when they rejoin


def _anim_allowance(events: list[dict]) -> int:
    """Seconds the client spends animating a turn before it shows the menu again."""
    n = sum(1 for e in events if e.get("t") in ("move", "switch", "faint"))
    return min(DUEL_ANIM_CAP, DUEL_ANIM_SECS * n)


def _team(raw: Any) -> list[dict] | None:
    """1-6 creatures as {sp, st, br, mg, sh, name}; every number is derived server-side."""
    if not isinstance(raw, list) or not 1 <= len(raw) <= 6:
        return None
    specs = [pb.clean_spec(c) for c in raw]
    if any(c is None for c in specs):
        return None
    return [pb.build_mon(c) for c in specs]


class Duel:
    """One live duel per room. Both players choose at once; the turn resolves when both are
    in (or a deadline passes). There is no server tick: deadlines are enforced lazily on the
    next message from anyone in the match, and clients send 'poke' when a countdown ends.
    A player whose last socket drops gets DUEL_GRACE_SECS to come back before forfeiting;
    rejoining the lobby returns the full match view, so a refresh resumes mid-battle."""

    def __init__(self) -> None:
        self.pending: dict[str, dict] = {}     # challenged user_id -> {from, team}
        self.match: dict | None = None
        self.missed: dict[str, tuple[float, dict]] = {}   # user_id -> (at, duelend payload) they missed

    def view(self) -> dict | None:
        mt = self.match
        if not mt:
            return None
        st = mt["state"]
        t = now()
        return {"mid": mt["mid"], "a": mt["a"], "b": mt["b"], "ids": mt["ids"], "turn": st["turn"],
                "phase": mt["phase"], "deadline_in": max(0, round(mt["deadline"] - t)),
                "deadline_ms": max(0, int((mt["deadline"] - t) * 1000)),
                "slack_ms": DUEL_SLACK_SECS * 1000, "waiting": self._waiting(),
                "away": {uid: max(0, int((d - t) * 1000)) for uid, d in mt["away"].items()},
                "sides": {uid: {"active": st["sides"][i]["active"],
                                "team": [pb.view_mon(m) for m in st["sides"][i]["team"]]}
                          for i, uid in enumerate(mt["ids"])}}

    def _waiting(self) -> list[str]:
        mt = self.match
        if not mt:
            return []
        if mt["phase"] == "replace":
            return [mt["ids"][i] for i in mt["need"] if i not in mt["choices"]]
        return [uid for i, uid in enumerate(mt["ids"]) if i not in mt["choices"]]

    def challenge(self, m: Member, msg: dict, out: Out, lobby: Lobby) -> None:
        if self.match:
            out.err(m.ws, "a duel is already on")
            return
        to = msg.get("to")
        team = _team(msg.get("team"))
        if to not in lobby.members or to == m.user_id:
            out.err(m.ws, "they need to be in the duel lobby")
            return
        if team is None:
            out.err(m.ws, "bring a team of 1 to 6 creatures")
            return
        self.pending[to] = {"from": m.user_id, "team": team}
        out.user(to, "challenge", **{"from": m.public()})
        out.to(m.ws, "challenged", to=lobby.members[to])

    def decline(self, m: Member, out: Out, lobby: Lobby) -> None:
        p = self.pending.pop(m.user_id, None)
        if p:
            out.user(p["from"], "declined", by=m.public())

    def accept(self, m: Member, msg: dict, out: Out, lobby: Lobby) -> None:
        if self.match:
            out.err(m.ws, "a duel is already on")
            return
        p = self.pending.pop(m.user_id, None)
        team = _team(msg.get("team"))
        if not p or p["from"] not in lobby.members:
            out.err(m.ws, "that challenge is gone")
            return
        if team is None:
            out.err(m.ws, "bring a team of 1 to 6 creatures")
            return
        a, b = p["from"], m.user_id
        self.missed.pop(a, None)
        self.missed.pop(b, None)
        self.match = {"mid": secrets.token_hex(4), "a": lobby.members[a], "b": lobby.members[b], "ids": [a, b],
                      "state": pb.new_battle(p["team"], team), "phase": "choose", "choices": {}, "need": [],
                      "deadline": now() + DUEL_FIRST_SECS, "auto": [0, 0], "away": {}}
        out.all("duel", duel=self.view())

    def act(self, m: Member, msg: dict, out: Out, rng: random.Random) -> None:
        mt = self.match
        if not mt or m.user_id not in mt["ids"]:
            out.err(m.ws, "you are not in this duel")
            return
        if self.tick(out, rng):
            if self.match is mt:
                out.to(m.ws, "late", turn=mt["state"]["turn"])
            return
        side = mt["ids"].index(m.user_id)
        if msg.get("turn") != mt["state"]["turn"]:
            out.to(m.ws, "late", turn=mt["state"]["turn"])     # a stale or replayed choice: ignore quietly
            return
        if mt["phase"] == "replace" and side not in mt["need"]:
            out.err(m.ws, "wait for the other player")
            return
        act = pb.legal(mt["state"], side, msg.get("a"))
        if act is None or (mt["phase"] == "replace" and act["k"] != "switch"):
            out.err(m.ws, "you can't do that now")
            return
        fresh = side not in mt["choices"]
        mt["choices"][side] = act          # a resend or a changed mind overwrites until both are in
        mt["auto"][side] = 0
        if fresh or len(mt["choices"]) == 2:
            self._advance(out, rng)

    def tick(self, out: Out, rng: random.Random) -> bool:
        """Enforce lazy deadlines. True if something happened (a forfeit or a forced turn)."""
        mt = self.match
        if not mt:
            return False
        t = now()
        gone = [i for i, uid in enumerate(mt["ids"]) if mt["away"].get(uid, t + 1) < t]
        if gone:
            self._end(out, None if len(gone) == 2 else 1 - gone[0], forfeit=True, left=True)
            return True
        if t <= mt["deadline"] + DUEL_SLACK_SECS:     # slack: a last-second pick is still in flight
            return False
        st = mt["state"]
        afk = []
        for side in (mt["need"] if mt["phase"] == "replace" else (0, 1)):
            if side not in mt["choices"]:
                mt["choices"][side] = ({"k": "switch", "to": pb.alive(st, side)[0]} if mt["phase"] == "replace"
                                       else pb.auto_act(st, side))
                mt["auto"][side] += 1
                if mt["auto"][side] >= DUEL_AFK_PICKS:
                    afk.append(side)
        if afk:
            self._end(out, None if len(afk) == 2 else 1 - afk[0], forfeit=True, timeout=True)
            return True
        self._advance(out, rng)
        return True

    def _advance(self, out: Out, rng: random.Random) -> None:
        mt = self.match
        st = mt["state"]
        if mt["phase"] == "replace":
            if any(side not in mt["choices"] for side in mt["need"]):
                out.all("waiting", mid=mt["mid"], turn=st["turn"], waiting=self._waiting())
                return
            events: list[dict] = []
            for side in mt["need"]:
                events += pb.replace(st, side, mt["choices"][side]["to"])
        else:
            if len(mt["choices"]) < 2:
                out.all("waiting", mid=mt["mid"], turn=st["turn"], waiting=self._waiting())
                return
            events = pb.resolve_turn(st, mt["choices"][0], mt["choices"][1], rng.random)
        mt["choices"] = {}
        mt["need"] = pb.needs_replace(st)
        mt["phase"] = "replace" if mt["need"] else "choose"
        mt["deadline"] = now() + (DUEL_REPLACE_SECS if mt["need"] else DUEL_CHOOSE_SECS) + _anim_allowance(events)
        mt["seq"] = mt.get("seq", 0) + 1
        out.all("turn", mid=mt["mid"], seq=mt["seq"], events=events, duel=self.view())
        if st["over"]:
            w = st["winner"]
            self._end(out, w)

    def _end(self, out: Out, winner_side: int | None, **extra: Any) -> None:
        mt = self.match
        winner = None if winner_side is None else (mt["a"] if winner_side == 0 else mt["b"])
        view = self.view()
        out.all("duelend", duel=view, winner=winner, **extra)
        # Whoever is mid-grace (socket down) cannot hear that broadcast: keep it for their rejoin.
        for uid in mt["away"]:
            self.missed[uid] = (now(), {"duel": view, "winner": winner, **extra})
        self.match = None

    def replay_missed(self, user_id: str, ws: Any, out: Out) -> None:
        """A rejoining player whose duel ended while they were away gets that ending, once."""
        got = self.missed.pop(user_id, None)
        t = now()
        self.missed = {u: v for u, v in self.missed.items() if t - v[0] < DUEL_RESULT_SECS}
        if got and t - got[0] < DUEL_RESULT_SECS:
            out.to(ws, "duelend", **got[1])

    def away(self, user_id: str, out: Out) -> bool:
        """The user's last socket dropped. True if they are mid-duel and get a grace period."""
        self.pending.pop(user_id, None)
        mt = self.match
        if not mt or user_id not in mt["ids"]:
            return False
        mt["away"][user_id] = now() + DUEL_GRACE_SECS
        out.all("away", mid=mt["mid"], user=user_id, ms=DUEL_GRACE_SECS * 1000)
        return True

    def back(self, user_id: str, out: Out) -> bool:
        """True if the user was away mid-duel (a reconnect, not a fresh join)."""
        mt = self.match
        if mt and mt["away"].pop(user_id, None) is not None:
            out.all("back", mid=mt["mid"], user=user_id)
            return True
        return False

    def seated(self, user_id: str) -> bool:
        """A duelist of the live match: their lobby seat is reserved while they reconnect."""
        return bool(self.match and user_id in self.match["ids"])

    def drop(self, user_id: str, out: Out) -> None:
        self.pending.pop(user_id, None)
        mt = self.match
        if mt and user_id in mt["ids"]:
            self._end(out, 1 - mt["ids"].index(user_id), forfeit=True)


# ------------------------------------------------------------------- mines --
class Mines:
    def __init__(self) -> None:
        self.run: dict | None = None

    def _floor(self, depth: int, rng: random.Random) -> dict:
        grid = [["rock" if rng.random() < 0.42 else "floor" for _ in range(MINE_COLS)] for _ in range(MINE_ROWS)]
        for x, y in ((0, 0), (1, 0), (0, 1), (MINE_COLS - 1, 0), (MINE_COLS - 2, 0)):
            grid[y][x] = "floor"
        rocks = [(x, y) for y in range(MINE_ROWS) for x in range(MINE_COLS) if grid[y][x] == "rock"]
        ladder = rng.choice(rocks) if rocks else (MINE_COLS - 1, MINE_ROWS - 1)
        slimes = []
        for _ in range(min(1 + depth // 2, 6)):
            sx, sy = rng.randrange(2, MINE_COLS - 2), rng.randrange(2, MINE_ROWS)
            grid[sy][sx] = "floor"
            slimes.append([sx, sy])
        return {"grid": grid, "ladder": list(ladder), "slimes": slimes}

    def view(self) -> dict | None:
        r = self.run
        if not r:
            return None
        grid = [row[:] for row in r["floor"]["grid"]]   # the ladder stays hidden until uncovered
        return {"depth": r["depth"], "grid": grid, "slimes": r["floor"]["slimes"],
                "players": {uid: {k: p[k] for k in ("x", "y", "hp", "name", "out")} for uid, p in r["players"].items()},
                "loot": {uid: p["loot"] for uid, p in r["players"].items()}}

    def start(self, m: Member, out: Out, lobby: Lobby, rng: random.Random) -> None:
        if self.run:
            out.err(m.ws, "a run is already underway")
            return
        spawns = [(0, 0), (MINE_COLS - 1, 0), (1, 0), (MINE_COLS - 2, 0), (0, 1), (1, 1), (MINE_COLS - 1, 1), (MINE_COLS - 2, 1)]
        players = {}
        for i, (uid, p) in enumerate(lobby.members.items()):
            x, y = spawns[i % len(spawns)]
            players[uid] = {"x": x, "y": y, "hp": 5, "loot": {}, "out": False, "name": p.get("displayName") or p.get("handle")}
        self.run = {"depth": 1, "floor": self._floor(1, rng), "players": players, "moves": 0}
        out.all("mines", run=self.view(), by=m.public())

    def _loot(self, depth: int, rng: random.Random) -> str | None:
        x = rng.random()
        if x < 0.45:
            return None
        for need, cut, item in ((8, .985, "ruby"), (6, .97, "emerald"), (4, .95, "amethyst"), (0, .9, "quartz"),
                                (5, .8, "gold"), (2, .65, "iron")):
            if depth >= need and x > cut:
                return item
        return "copper"

    def move(self, m: Member, msg: dict, out: Out, rng: random.Random) -> None:
        r = self.run
        p = r and r["players"].get(m.user_id)
        if not p or p["out"] or p["hp"] <= 0:
            out.err(m.ws, "you are not in this run")
            return
        dx, dy = msg.get("dx"), msg.get("dy")
        if (dx, dy) not in ((1, 0), (-1, 0), (0, 1), (0, -1)):
            out.err(m.ws, "move one tile")
            return
        f = r["floor"]
        nx, ny = p["x"] + dx, p["y"] + dy
        if not (0 <= nx < MINE_COLS and 0 <= ny < MINE_ROWS):
            return
        cell = f["grid"][ny][nx]
        if cell == "rock":
            f["grid"][ny][nx] = "floor"
            if [nx, ny] == f["ladder"]:
                f["grid"][ny][nx] = "ladder"
            else:
                item = self._loot(r["depth"], rng)
                if item:
                    p["loot"][item] = p["loot"].get(item, 0) + 1
        elif cell == "ladder" and r["depth"] < MINE_MAX_DEPTH:
            r["depth"] += 1
            r["floor"] = self._floor(r["depth"], rng)
            for q in r["players"].values():
                if not q["out"] and q["hp"] > 0:
                    q["x"], q["y"] = 0, 0
            out.all("mines", run=self.view(), note=f"{p['name']} found the way down: floor {r['depth']}")
            return
        else:
            p["x"], p["y"] = nx, ny
        r["moves"] += 1
        active = [q for q in r["players"].values() if not q["out"] and q["hp"] > 0]
        for s in f["slimes"]:
            if rng.random() < 0.5 and active:
                tgt = min(active, key=lambda q: abs(q["x"] - s[0]) + abs(q["y"] - s[1]))
                sx = s[0] + (1 if tgt["x"] > s[0] else -1 if tgt["x"] < s[0] else 0)
                sy = s[1] if sx != s[0] else s[1] + (1 if tgt["y"] > s[1] else -1 if tgt["y"] < s[1] else 0)
                if 0 <= sx < MINE_COLS and 0 <= sy < MINE_ROWS and f["grid"][sy][sx] == "floor":
                    s[0], s[1] = sx, sy
            for q in active:
                if [q["x"], q["y"]] == s:
                    q["hp"] -= 1
                    s[0] = min(MINE_COLS - 1, s[0] + 1)
        out.all("mines", run=self.view())
        for uid, q in r["players"].items():
            if q["hp"] <= 0 and not q["out"]:
                q["out"] = True
                half = {k: v // 2 for k, v in q["loot"].items() if v // 2}
                out.user(uid, "loot", items=half, fainted=True)
        self._maybe_end(out)

    def leave(self, m: Member, out: Out) -> None:
        r = self.run
        p = r and r["players"].get(m.user_id)
        if not p or p["out"]:
            return
        p["out"] = True
        out.to(m.ws, "loot", items=p["loot"], fainted=False)
        out.all("mines", run=self.view(), note=f"{p['name']} climbed out")
        self._maybe_end(out)

    def _maybe_end(self, out: Out) -> None:
        if self.run and all(q["out"] for q in self.run["players"].values()):
            out.all("minesend", depth=self.run["depth"])
            self.run = None

    def drop(self, user_id: str, out: Out) -> None:
        r = self.run
        if r and user_id in r["players"]:
            r["players"][user_id]["out"] = True
            self._maybe_end(out)


# -------------------------------------------------------------------- farm --
def _today() -> date:
    return datetime.fromtimestamp(wall(), UTC).date()


async def _farm_load(db, room_id: str) -> RoomFarm:
    row = await db.get(RoomFarm, room_id)
    if row is None:
        row = RoomFarm(room_id=room_id, data={"plots": [None] * FARM_PLOTS, "seeds": {}, "seedDay": "", "gardeners": []})
        db.add(row)
    data = dict(row.data or {})
    plots = list(data.get("plots") or [])[:FARM_PLOTS]
    data["plots"] = plots + [None] * (FARM_PLOTS - len(plots))
    data.setdefault("seeds", {})
    data.setdefault("seedDay", "")
    data.setdefault("gardeners", [])
    row.data = data
    return row


async def _water(db, gardeners: list[str], since: date) -> int:
    """Prompts the room's gardeners published since the plot was planted."""
    if not gardeners:
        return 0
    q = select(func.coalesce(func.sum(DailyStat.prompts), 0)).where(
        DailyStat.user_id.in_(gardeners), DailyStat.stat_date >= since)
    return int((await db.execute(q)).scalar() or 0)


async def _farm_view(db, data: dict) -> dict:
    plots = []
    gardeners = data.get("gardeners", [])
    for p in data["plots"]:
        if not p:
            plots.append(None)
            continue
        hours, need, _ = CROPS[p["crop"]]
        need *= max(1, len(gardeners))
        water = await _water(db, gardeners, date.fromisoformat(p["day"]))
        grown = (wall() - p["at"]) / 3600 >= hours
        plots.append({"crop": p["crop"], "by": p.get("by", ""), "water": min(water, need), "need": need,
                      "hoursLeft": max(0.0, round(hours - (wall() - p["at"]) / 3600, 1)),
                      "ripe": grown and water >= need})
    return {"plots": plots, "seeds": data["seeds"], "seedBoxOpen": data["seedDay"] != _today().isoformat(),
            "gardeners": len(gardeners), "season": season_of(_today())}


async def farm_op(room_id: str, m: Member, msg: dict, out: Out, rng: random.Random) -> None:
    op = msg.get("op")
    async with SessionLocal() as db:
        row = await _farm_load(db, room_id)
        data = dict(row.data)
        if m.user_id not in data["gardeners"]:
            data["gardeners"] = data["gardeners"] + [m.user_id]
        if op == "seeds":
            today = _today().isoformat()
            if data["seedDay"] == today:
                out.err(m.ws, "the seed box is empty until tomorrow")
            else:
                crops = [c for c, v in CROPS.items() if v[2] == season_of(_today())]
                seeds = dict(data["seeds"])
                for _ in range(FARM_SEEDS_PER_DAY):
                    c = rng.choice(crops)
                    seeds[c] = seeds.get(c, 0) + 1
                data["seeds"], data["seedDay"] = seeds, today
                out.all("note", text=f"{m.display_name} opened the seed box")
        elif op == "plant":
            i, crop = msg.get("plot"), msg.get("crop")
            if not isinstance(i, int) or not 0 <= i < FARM_PLOTS or data["plots"][i] or crop not in CROPS:
                out.err(m.ws, "can't plant there")
            elif data["seeds"].get(crop, 0) < 1:
                out.err(m.ws, "no seeds of that kind")
            else:
                seeds = dict(data["seeds"])
                seeds[crop] -= 1
                if not seeds[crop]:
                    del seeds[crop]
                plots = list(data["plots"])
                plots[i] = {"crop": crop, "at": wall(), "day": _today().isoformat(), "by": m.display_name}
                data["seeds"], data["plots"] = seeds, plots
        elif op == "harvest":
            i = msg.get("plot")
            view = await _farm_view(db, data)
            if not isinstance(i, int) or not 0 <= i < FARM_PLOTS or not view["plots"][i] or not view["plots"][i]["ripe"]:
                out.err(m.ws, "not ripe yet")
            else:
                crop = data["plots"][i]["crop"]
                plots = list(data["plots"])
                plots[i] = None
                data["plots"] = plots
                out.to(m.ws, "harvest", crop=crop, n=2)
                out.all("note", text=f"{m.display_name} harvested the {crop}")
        row.data = data
        await db.commit()
        out.all("farm", farm=await _farm_view(db, data))


# --------------------------------------------------------------- dispatch --
def _rng() -> random.Random:
    return random.Random(secrets.randbits(64))     # patched in tests


async def handle(room: Room, member: Member, msg: dict) -> None:
    g, op = msg.get("g"), msg.get("op")
    out = Out(g if isinstance(g, str) else "?")
    if g == "party" and isinstance(op, str):
        partymod.op(room.room_id, member, op, out)
        await _flush(room, out)
        return
    if g not in GAMES or not isinstance(op, str):
        out.err(member.ws, "unknown game")
        await _flush(room, out)
        return
    if g == "hq" and not room.room_id.startswith("hq_"):
        out.err(member.ws, "HQ presence lives in an HQ room")
        await _flush(room, out)
        return
    v = valley_for(room.room_id)
    lobby = v.lobbies[g]
    rng = _rng()

    if op == "join":
        reserved = g == "duel" and v.duel.seated(member.user_id)
        cap = hqmod.cap(room.room_id) if g == "hq" else MAX_LOBBY
        if member.user_id not in lobby.members and len(lobby.members) >= cap and not reserved:
            out.err(member.ws, "this game's lobby is full")
        else:
            fresh = member.user_id not in lobby.members
            returning = g == "duel" and v.duel.back(member.user_id, out)   # a reconnect: no "joined" toast
            blip = lobby.blips.pop(member.user_id, None)
            returning = returning or (blip is not None and now() - blip < HOST_GRACE)
            lobby.blips = {u: t for u, t in lobby.blips.items() if now() - t < HOST_GRACE}
            lobby.members[member.user_id] = member.public()
            ph = lobby.prev_host
            if lobby.host is None or lobby.host not in lobby.members or \
                    (ph is not None and ph[0] == member.user_id and now() - ph[1] < HOST_GRACE):
                lobby.host = member.user_id      # first in, or the host back from a socket blip
            if ph is not None and (ph[0] == member.user_id or now() - ph[1] >= HOST_GRACE):
                lobby.prev_host = None
            out.all("lobby", members=lobby.roster(), joined=member.public() if fresh and not returning else None,
                    name=GAME_NAMES[g])
            if g == "pond":
                v.pond._expire_boss(out)      # a boss nobody touched may have run out
                out.to(member.ws, "pond", pond=v.pond.snapshot())
            elif g == "race" and v.race.running():
                out.to(member.ws, "start", round=v.race.round, secs=max(0, round(v.race.until - now())))
            elif g == "duel":
                v.duel.tick(out, rng)
                v.duel.replay_missed(member.user_id, member.ws, out)
                out.to(member.ws, "duel", duel=v.duel.view())
            elif g == "mines":
                out.to(member.ws, "mines", run=v.mines.view())
            elif g == "farm":
                await farm_op(room.room_id, member, {"op": "view"}, out, rng)
            elif g == "golf":
                if v.golf.restore(member.user_id, member.public(), now()):
                    # back from a dropped socket: everyone sees them return to the round
                    out.lobby(list(lobby.members), "golf", round=v.golf.view(now()), back=member.user_id)
                else:
                    out.to(member.ws, "golf", round=v.golf.view(now()))
            elif g == "kart":
                if v.kart.restore(member.user_id, member.public()):
                    out.lobby(list(lobby.members), "kart", race=v.kart.view(now()), back=member.user_id)
                else:
                    out.to(member.ws, "kart", race=v.kart.view(now()))
            elif g == "plat":
                if v.plat.restore(member.user_id, member.public()):
                    out.lobby(list(lobby.members), "plat", run=v.plat.view(now()), back=member.user_id)
                else:
                    out.to(member.ws, "plat", run=v.plat.view(now()))
            elif g == "type":
                out.to(member.ws, "type", race=v.type.view(now()))
            elif g == "hq":
                v.hq.enter(member.user_id, member.public(), now())
                out.to(member.ws, "snap", ps=v.hq.listing())
                _hq_tick_on(room.room_id)
            elif g == "fps":
                if v.fps.enter(member.user_id, member.public(), now()):
                    # back from a dropped socket, or dropping in mid-match: everyone sees it
                    out.lobby(list(lobby.members), "fps", match=v.fps.view(now()), back=member.user_id)
                else:
                    out.to(member.ws, "fps", match=v.fps.view(now()))
            elif g == "strike":
                if v.strike.enter(member.user_id, member.public(), now()):
                    # back from a dropped socket, or dropping in mid-match: everyone sees it
                    out.lobby(list(lobby.members), "strike", match=v.strike.view(now()), back=member.user_id)
                else:
                    out.to(member.ws, "strike", match=v.strike.view(now()))
    elif op == "leave":
        _leave_lobby(v, g, member.user_id, out)
    elif op == "invite":
        to = msg.get("to")
        if member.user_id not in lobby.members:
            out.err(member.ws, "join the lobby first")
        elif not isinstance(to, str) or not to or to == member.user_id:
            out.err(member.ws, "invite someone else")
        else:
            delivered = await manager.deliver_to_user(to, {"type": "game", "g": g, "ev": "invite", "pv": PROTOCOL[g]["v"],
                                                         "from": member.public(), "room": room.room_id,
                                                         "name": GAME_NAMES[g]})
            out.to(member.ws, "invited", to=to, delivered=delivered)
    elif member.user_id not in lobby.members:
        out.err(member.ws, "join the lobby first")
    elif g == "pond":
        if op in ("cast", "hook", "land", "lose") and v.pond.throttled(member.user_id):
            pass                                  # flooding: drop it (pull has its own limit)
        elif op == "cast":
            v.pond.cast(member, msg, out, rng)
        elif op == "hook":
            v.pond.hook(member, msg, out)
        elif op == "land":
            v.pond.land(member, msg, out, rng)
            v.pond.maybe_boss(lobby, out)
        elif op == "lose":
            v.pond.lose(member, msg, out)
        elif op == "pull":
            v.pond.pull(member, out)
    elif g == "race":
        if op == "start":
            v.race.start(member, out, rng)
        elif op == "guess":
            v.race.guess(member, msg, out)
    elif g == "duel":
        if op == "challenge":
            v.duel.challenge(member, msg, out, lobby)
        elif op == "accept":
            v.duel.accept(member, msg, out, lobby)
        elif op == "act":
            v.duel.act(member, msg, out, rng)
        elif op == "poke":
            v.duel.tick(out, rng)
        elif op == "sync":
            v.duel.tick(out, rng)
            out.to(member.ws, "duel", duel=v.duel.view())
        elif op == "decline":
            v.duel.decline(member, out, lobby)
        elif op == "forfeit":
            v.duel.drop(member.user_id, out)
    elif g == "mines":
        if op == "start":
            v.mines.start(member, out, lobby, rng)
        elif op == "move":
            v.mines.move(member, msg, out, rng)
        elif op == "exit":
            v.mines.leave(member, out)
    elif g == "farm":
        await farm_op(room.room_id, member, msg, out, rng)
    elif g == "hq":
        if op == "pos":
            v.hq.pos(member.user_id, msg, now())
    elif g == "type":
        type_op(room.room_id, v, lobby, member, op, msg, out, rng)
    elif g == "golf":
        golf_op(v.golf, lobby, member, op, msg, out, rng)
    elif g == "kart":
        kart_op(room.room_id, v.kart, lobby, member, op, msg, out)
    elif g == "plat":
        plat_op(room.room_id, v.plat, lobby, member, op, msg, out)
    elif g == "fps":
        fps_op(room.room_id, v.fps, lobby, member, op, msg, out)
    elif g == "strike":
        strike_op(room.room_id, v.strike, lobby, member, op, msg, out)
    await _flush(room, out)


# -------------------------------------------------------------------- plat --
def plat_op(room_id: str, pm: "platmod.Plat", lobby: Lobby, member: Member, op: str, msg: dict, out: Out) -> None:
    """Platformer Rush: position frames feed the room's tick loop; a respawn answers the
    player at once; everything else goes to this game's lobby."""
    t = now()
    uid = member.user_id
    ids = list(lobby.members)
    if op == "pos":
        ok, fix = pm.pos(uid, msg, t)
        if fix is not None:
            out.to(member.ws, "fix", **fix)
    elif op == "respawn":
        sp = pm.respawn(uid, t)
        if sp is not None:
            out.to(member.ws, "spawn", **sp)
    elif op == "view":
        out.to(member.ws, "plat", run=pm.view(t))
    elif op == "char":
        c = pm.char(uid, msg)
        if c is None:
            out.err(member.ws, "pick a character")
        else:
            out.lobby(ids, "char", user=uid, char=c)
    elif op in ("start", "end"):
        if lobby.host != uid:
            out.err(member.ws, "only the host can do that")
            return
        lobby.prev_host = None
        if op == "start":
            err = pm.start(lobby.members, msg.get("level"), msg.get("mode"), t)
            if err is None and not _plat_tick_on(room_id):
                pm.end()
                err = "the Arena is busy right now: try again in a minute"
            if err:
                out.err(member.ws, err)
            else:
                out.lobby(ids, "plat", run=pm.view(t), by=member.public())
        else:
            pm.end()
            realtime.stop("plat:" + room_id)
            out.lobby(ids, "plat", run=pm.view(t))


def _plat_tick_on(room_id: str) -> bool:
    """Start the room's run loop (a no-op if it runs). False: no slot under the budget."""
    key = "plat:" + room_id

    async def step(t: float, send: bool) -> bool:
        v = _rooms.get(room_id)
        room = manager.get(room_id)
        if v is None or room is None or not v.plat.running():
            return False
        lob = v.lobbies["plat"].members
        ids = list(lob) + [u for u in v.plat.players if u not in lob]
        out = Out("plat")
        for ev, data in v.plat.tick(t, send):
            out.lobby(ids, ev, **data)
        if not v.plat.running():
            out.lobby(ids, "plat", run=v.plat.view(t))
        if out.items:
            tk = realtime.get(key)
            if tk is not None:
                n = sum(1 for m in room.members.values() if m.user_id in ids)
                tk.count(sum(len(json.dumps(p, separators=(",", ":"))) for _k, _t, p in out.items) * max(1, n))
            await _flush(room, out)
        return v.plat.running()

    return realtime.start(key, platmod.HZ, step, lambda: now()) is not None


# -------------------------------------------------------------------- type --
def type_op(room_id: str, v: "RoomValley", lobby: Lobby, member: Member, op: str, msg: dict, out: Out,
            rng: random.Random) -> None:
    """Code Typing Race: the host starts; everyone's progress feeds the room's loop."""
    t = now()
    ids = list(lobby.members)
    if op == "prog":
        v.type.prog(member.user_id, msg, t)
    elif op == "view":
        out.to(member.ws, "type", race=v.type.view(t))
    elif op in ("start", "end"):
        if lobby.host != member.user_id:
            out.err(member.ws, "only the host can do that")
            return
        if op == "start":
            err = v.type.start(dict(lobby.members), t, rng)
            if err is None and not _type_tick_on(room_id):
                v.type.end()
                err = "the Arena is busy right now: try again in a minute"
            if err:
                out.err(member.ws, err)
            else:
                out.lobby(ids, "type", race=v.type.view(t), by=member.public())
        else:
            v.type.end()
            realtime.stop("type:" + room_id)
            out.lobby(ids, "type", race=v.type.view(t))


def _type_tick_on(room_id: str) -> bool:
    key = "type:" + room_id

    async def step(t: float, send: bool) -> bool:
        v = _rooms.get(room_id)
        room = manager.get(room_id)
        if v is None or room is None or not v.type.running():
            return False
        ids = list(v.lobbies["type"].members) + [u for u in v.type.players if u not in v.lobbies["type"].members]
        out = Out("type")
        for ev, data in v.type.tick(t):
            out.lobby(ids, ev, **data)
        if out.items:
            await _flush(room, out)
        return v.type.running()

    return realtime.start(key, typemod.HZ, step, lambda: now()) is not None


# ---------------------------------------------------------------------- hq --
def _hq_tick_on(room_id: str) -> bool:
    """Send everyone in an HQ where everyone is, 8 times a second while anyone is there."""
    key = "hq:" + room_id

    async def step(t: float, send: bool) -> bool:
        v = _rooms.get(room_id)
        room = manager.get(room_id)
        if v is None or room is None or not v.hq.people:
            return False
        if send and (v.hq.dirty or t - v.hq.sent_at >= hqmod.KEEPALIVE):
            v.hq.dirty, v.hq.sent_at = False, t
            out = Out("hq")
            out.lobby(list(v.lobbies["hq"].members), "snap", ps=v.hq.listing())
            await _flush(room, out)
        return True

    return realtime.start(key, hqmod.HZ, step, lambda: now()) is not None


# -------------------------------------------------------------------- kart --
def kart_op(room_id: str, km: "kartmod.Kart", lobby: Lobby, member: Member, op: str, msg: dict, out: Out) -> None:
    """Kart Racing: position frames feed the room's tick loop; everything else goes to
    this game's lobby at once."""
    t = now()
    uid = member.user_id
    ids = list(lobby.members)
    if op == "pos":
        ok, fix = km.pos(uid, msg, t)
        if fix is not None:
            out.to(member.ws, "fix", **fix)
    elif op == "view":
        out.to(member.ws, "kart", race=km.view(t))
    elif op == "car":
        c = km.car(uid, msg)
        if c is None:
            out.err(member.ws, "pick a car")
        else:
            out.lobby(ids, "car", user=uid, car=c)
    elif op in ("start", "end"):
        if lobby.host != uid:
            out.err(member.ws, "only the host can do that")
            return
        lobby.prev_host = None
        if op == "start":
            err = km.start(lobby.members, msg.get("track"), msg.get("laps"), t)
            if err is None and not _kart_tick_on(room_id):
                km.end()
                err = "the Arena is busy right now: try again in a minute"
            if err:
                out.err(member.ws, err)
            else:
                out.lobby(ids, "kart", race=km.view(t), by=member.public())
        else:
            km.end()
            realtime.stop("kart:" + room_id)
            out.lobby(ids, "kart", race=km.view(t))


def _kart_tick_on(room_id: str) -> bool:
    """Start the room's race loop (a no-op if it runs). False: no slot under the budget."""
    key = "kart:" + room_id

    async def step(t: float, send: bool) -> bool:
        v = _rooms.get(room_id)
        room = manager.get(room_id)
        if v is None or room is None or not v.kart.running():
            return False
        ids = list(v.lobbies["kart"].members) + [u for u in v.kart.players if u not in v.lobbies["kart"].members]
        out = Out("kart")
        for ev, data in v.kart.tick(t, send):
            out.lobby(ids, ev, **data)
        if not v.kart.running():
            out.lobby(ids, "kart", race=v.kart.view(t))
        if out.items:
            tk = realtime.get(key)
            if tk is not None:
                n = sum(1 for m in room.members.values() if m.user_id in ids)
                tk.count(sum(len(json.dumps(p, separators=(",", ":"))) for _k, _t, p in out.items) * max(1, n))
            await _flush(room, out)
        return v.kart.running()

    return realtime.start(key, kartmod.HZ, step, lambda: now()) is not None


# --------------------------------------------------------------------- fps --
def fps_op(room_id: str, fm: "fpsmod.Fps", lobby: Lobby, member: Member, op: str, msg: dict, out: Out) -> None:
    """Blaster Arena: moves and shots feed the room's tick loop (which batches what
    happened into one snapshot per tick); everything else goes to the lobby at once."""
    t = now()
    uid = member.user_id
    ids = list(lobby.members)
    if op == "pos":
        _ok, fix = fm.pos(uid, msg, t)
        if fix is not None:
            out.to(member.ws, "fix", **fix)
    elif op == "fire":
        _shot, sync = fm.fire(uid, msg, t)
        if sync is not None:
            out.to(member.ws, "ammo", **sync)
    elif op == "reload":
        fm.reload(uid, msg, t)
    elif op == "weapon":
        fm.weapon(uid, msg, t)
    elif op == "view":
        out.to(member.ws, "fps", match=fm.view(t))
    elif op == "char":
        c = fm.char(uid, msg)
        if c is None:
            out.err(member.ws, "pick a character")
        else:
            out.lobby(ids, "char", user=uid, c=c)
    elif op in ("start", "end"):
        if lobby.host != uid:
            out.err(member.ws, "only the host can do that")
            return
        lobby.prev_host = None
        if op == "start":
            err = fm.start(lobby.members, msg.get("minutes"), msg.get("kills"), t)
            if err is None and not _fps_tick_on(room_id):
                fm.end()
                err = "the Arena is busy right now: try again in a minute"
            if err:
                out.err(member.ws, err)
            else:
                out.lobby(ids, "fps", match=fm.view(t), by=member.public())
        else:
            fm.end()
            realtime.stop("fps:" + room_id)
            out.lobby(ids, "fps", match=fm.view(t))


def _fps_tick_on(room_id: str) -> bool:
    """Start the room's match loop (a no-op if it runs). False: no slot under the budget."""
    key = "fps:" + room_id

    async def step(t: float, send: bool) -> bool:
        v = _rooms.get(room_id)
        room = manager.get(room_id)
        if v is None or room is None or not v.fps.running():
            return False
        ids = list(v.lobbies["fps"].members) + [u for u in v.fps.players if u not in v.lobbies["fps"].members]
        out = Out("fps")
        for ev, data in v.fps.tick(t, send):
            out.lobby(ids, ev, **data)
        if not v.fps.running():
            out.lobby(ids, "fps", match=v.fps.view(t))
        if out.items:
            tk = realtime.get(key)
            if tk is not None:
                n = sum(1 for m in room.members.values() if m.user_id in ids)
                tk.count(sum(len(json.dumps(p, separators=(",", ":"))) for _k, _t, p in out.items) * max(1, n))
            await _flush(room, out)
        return v.fps.running()

    return realtime.start(key, fpsmod.HZ, step, lambda: now()) is not None


# ----------------------------------------------------------------- strike --
def strike_op(room_id: str, sm: "strikemod.Strike", lobby: Lobby, member: Member, op: str, msg: dict, out: Out) -> None:
    """Strikezone: moves and shots feed the room's tick loop (which batches what happened
    into one snapshot per tick); everything else goes to the lobby at once."""
    t = now()
    uid = member.user_id
    ids = list(lobby.members)
    if op == "pos":
        _ok, fix = sm.pos(uid, msg, t)
        if fix is not None:
            out.to(member.ws, "fix", **fix)
    elif op == "fire":
        _shot, sync = sm.fire(uid, msg, t)
        if sync is not None:
            out.to(member.ws, "ammo", **sync)
    elif op == "reload":
        sm.reload(uid, msg, t)
    elif op == "weapon":
        sm.weapon(uid, msg, t)
    elif op == "view":
        out.to(member.ws, "strike", match=sm.view(t))
    elif op == "char":
        c = sm.char(uid, msg)
        if c is None:
            out.err(member.ws, "pick a character")
        else:
            out.lobby(ids, "char", user=uid, c=c)
    elif op in ("start", "end"):
        if lobby.host != uid:
            out.err(member.ws, "only the host can do that")
            return
        lobby.prev_host = None
        if op == "start":
            err = sm.start(lobby.members, msg.get("minutes"), msg.get("kills"), t, tsize=msg.get("tsize"))
            if err is None and not _strike_tick_on(room_id):
                sm.end()
                err = "the Arena is busy right now: try again in a minute"
            if err:
                out.err(member.ws, err)
            else:
                out.lobby(ids, "strike", match=sm.view(t), by=member.public())
        else:
            sm.end()
            realtime.stop("strike:" + room_id)
            out.lobby(ids, "strike", match=sm.view(t))


def _strike_tick_on(room_id: str) -> bool:
    """Start the room's match loop (a no-op if it runs). False: no slot under the budget."""
    key = "strike:" + room_id

    async def step(t: float, send: bool) -> bool:
        v = _rooms.get(room_id)
        room = manager.get(room_id)
        if v is None or room is None or not v.strike.running():
            return False
        ids = list(v.lobbies["strike"].members) + [u for u in v.strike.players if u not in v.lobbies["strike"].members]
        out = Out("strike")
        for ev, data in v.strike.tick(t, send):
            out.lobby(ids, ev, **data)
        if not v.strike.running():
            out.lobby(ids, "strike", match=v.strike.view(t))
        if out.items:
            tk = realtime.get(key)
            if tk is not None:
                n = sum(1 for m in room.members.values() if m.user_id in ids)
                tk.count(sum(len(json.dumps(p, separators=(",", ":"))) for _k, _t, p in out.items) * max(1, n))
            await _flush(room, out)
        return v.strike.running()

    return realtime.start(key, strikemod.HZ, step, lambda: now()) is not None


# -------------------------------------------------------------------- golf --
def golf_op(gm: "golfmod.Golf", lobby: Lobby, member: Member, op: str, msg: dict, out: Out,
            rng: random.Random | None = None) -> None:
    """Mini Golf: every event goes to this game's lobby only. rng draws a random round's holes."""
    t = now()
    uid = member.user_id
    ids = list(lobby.members)
    _golf_advance(gm, ids, out, t)       # a dropped player's hold may have run out meanwhile
    if op == "view":
        out.to(member.ws, "golf", round=gm.view(t))
    elif op == "pos":
        rel = gm.pos(uid, msg, t)
        if rel is not None:
            out.lobby(ids, "pos", skip_user=uid, **rel)
    elif op == "char":
        c = gm.char(uid, msg)
        if c is None:
            out.err(member.ws, "pick a character")
        else:
            out.lobby(ids, "char", user=uid, c=c)
    elif op == "shot":
        ev, err = gm.shot(uid, msg, t)
        if err:
            out.err(member.ws, err)
            return
        out.lobby(ids, "shot", **ev)
        _golf_advance(gm, ids, out, t, ev["ticks"])
    elif op == "concede":
        if not gm.pick_up([uid]):
            out.err(member.ws, "nothing to pick up")
            return
        out.lobby(ids, "golf", round=gm.view(t))
        _golf_advance(gm, ids, out, t)
    elif op in ("start", "skip", "end"):
        if lobby.host != uid:
            out.err(member.ws, "only the host can do that")
            return
        lobby.prev_host = None               # the new host acted: a returning old host stays a player
        if op == "start":
            err = gm.start(lobby.members, msg.get("course"), t, msg.get("holes"), rng if rng is not None else _rng())
            if err:
                out.err(member.ws, err)
            else:
                out.lobby(ids, "golf", round=gm.view(t), by=member.public())
        elif op == "skip":
            gm.release()                     # nobody parked holds the hole open any more
            gm.pick_up(list(gm.players))
            out.lobby(ids, "golf", round=gm.view(t))
            _golf_advance(gm, ids, out, t)
        else:
            gm.end()
            out.lobby(ids, "golf", round=gm.view(t))


def _golf_advance(gm: "golfmod.Golf", ids: list[str], out: Out, t: float, ticks: int = 0) -> None:
    was = gm.phase
    step = gm.advance(t, ticks)
    if step is not None:
        out.lobby(ids, step[0], **step[1])
        out.lobby(ids, "golf", round=gm.view(t))
    elif gm.phase != was:                # the round ended: its last (dropped) player never came back
        out.lobby(ids, "golf", round=gm.view(t))


_timers: set = set()                     # strong refs to the recheck tasks in flight


def _golf_recheck_later(room_id: str, delay: float) -> None:
    """Re-run the golf advance once a dropped player's grace runs out, so the hole
    moves on even if nobody else does anything meanwhile."""
    try:
        loop = asyncio.get_running_loop()
    except RuntimeError:
        return

    def fire() -> None:
        task = loop.create_task(golf_recheck(room_id))
        _timers.add(task)
        task.add_done_callback(_timers.discard)
    loop.call_later(max(0.0, delay) + 0.05, fire)


async def golf_recheck(room_id: str) -> None:
    v = _rooms.get(room_id)
    if v is None:
        return
    room = manager.get(room_id)
    t = now()
    out = Out("golf")
    _golf_advance(v.golf, list(v.lobbies["golf"].members), out, t)
    left = v.golf.grace_left(t)
    if left is not None:
        _golf_recheck_later(room_id, left)
    if room is not None and room.members:
        await _flush(room, out)
    elif left is None and _rooms.get(room_id) is v:
        _rooms.pop(room_id, None)        # nobody came back to the room


def _leave_lobby(v: RoomValley, g: str, user_id: str, out: Out, disconnected: bool = False) -> None:
    """Leave one game's lobby. disconnected: the socket dropped rather than an explicit
    leave, so the multi.js shell will most likely rejoin within seconds: the host is
    handed back on a quick return and a golf hole waits for the player."""
    """A dropped live duelist waits DUEL_GRACE_SECS for a reconnect instead of forfeiting."""
    lobby = v.lobbies[g]
    who = lobby.members.pop(user_id, None)
    if who is None:
        return
    if disconnected:
        lobby.blips[user_id] = now()
    if lobby.host == user_id:
        lobby.host = next(iter(lobby.members), None)
        lobby.prev_host = (user_id, now()) if disconnected and lobby.host is not None else None
    away = False
    if g == "pond":
        c = v.pond.drop(user_id)
        if c:                        # their bobber leaves everyone else's water too
            out.all("lost", user=who, id=c["id"], fish=c["fish"])
    elif g == "duel":
        away = disconnected and v.duel.away(user_id, out)
        if not away:
            v.duel.drop(user_id, out)
    elif g == "mines":
        v.mines.drop(user_id, out)
    elif g == "kart" and v.kart.drop(user_id, now(), blip=disconnected):
        out.lobby(list(lobby.members), "kart", race=v.kart.view(now()), left=user_id)
    elif g == "plat" and v.plat.drop(user_id, now(), blip=disconnected):
        out.lobby(list(lobby.members), "plat", run=v.plat.view(now()), left=user_id)
    elif g == "fps" and v.fps.drop(user_id, now(), blip=disconnected):
        out.lobby(list(lobby.members), "fps", match=v.fps.view(now()), left=user_id)
    elif g == "strike" and v.strike.drop(user_id, now(), blip=disconnected):
        out.lobby(list(lobby.members), "strike", match=v.strike.view(now()), left=user_id)
    elif g == "hq":
        v.hq.drop(user_id)
    elif g == "type":
        v.type.drop(user_id)
    elif g == "golf" and v.golf.drop(user_id, now(), blip=disconnected):
        ids = list(lobby.members)
        out.lobby(ids, "golf", round=v.golf.view(now()), left=user_id)
        if not disconnected:
            _golf_advance(v.golf, ids, out, now())
    # A duelist on a grace period has not left: the "away" event says so, no "left" notice.
    out.all("lobby", members=lobby.roster(), left=None if away else who, name=GAME_NAMES[g])


async def on_disconnect(room_id: str, member: Member) -> None:
    """A socket went away: if that was the user's last socket in the room, leave every lobby."""
    v = _rooms.get(room_id)
    room = manager.get(room_id)
    if v is None:
        return
    if room is not None and any(m.user_id == member.user_id and ws is not member.ws
                                for ws, m in room.members.items()):
        return                       # the same person is still here on another socket
    for g in GAMES:
        out = Out(g)
        _leave_lobby(v, g, member.user_id, out, disconnected=True)
        if room is not None:
            await _flush(room, out)
    left = v.golf.grace_left(now())
    if left is not None:
        _golf_recheck_later(room_id, left)   # the recheck also tidies an emptied room
    elif room is None or all(ws is member.ws for ws in room.members):
        _rooms.pop(room_id, None)


async def _send_quiet(ws: Any, payload: dict) -> None:
    try:
        await asyncio.wait_for(ws.send_json(payload), SEND_TIMEOUT)
    except Exception:
        pass


async def _flush(room: Room, out: Out) -> None:
    for kind, target, payload in out.items:
        # A finished game's results are kept (progression, leaderboards, profiles),
        # once per game: from the event the server itself sends the lobby.
        if payload.get("ev") == "done" and payload.get("g") in resultsmod.GAMES and kind in ("lobby", "all"):
            await resultsmod.record(payload["g"], payload)     # once per game, a few ms; never raises
            pm = partymod.on_done(room.room_id, payload["g"], payload,
                                  {m.user_id: m.public() for m in list(room.members.values())})
            if pm is not None:
                out.items.append(("all", None, pm))     # sent after this loop reaches it
        if kind == "all":
            await room.broadcast(payload)
        elif kind == "ws":
            try:
                await target.send_json(payload)
            except Exception:
                pass
        elif kind == "lobby":
            # Fan out concurrently with a per-socket timeout: one slow player never
            # holds up everyone else's position or shot.
            ids, skip = target
            socks = [ws for ws, mem in list(room.members.items()) if mem.user_id in ids and mem.user_id != skip]
            if len(socks) == 1:
                await _send_quiet(socks[0], payload)       # no task needed (wait_for runs it inline)
            elif socks:
                # shielded: if this handler is cancelled mid-send (a closing socket), the
                # other players' copies still go out
                await asyncio.shield(asyncio.gather(*(_send_quiet(ws, payload) for ws in socks)))
        else:
            for ws, mem in list(room.members.items()):
                if mem.user_id == target:
                    try:
                        await ws.send_json(payload)
                    except Exception:
                        pass
