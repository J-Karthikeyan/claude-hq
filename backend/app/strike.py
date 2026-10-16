"""
Strikezone for the Valley (g = "strike"): a team FPS for up to 8, two sides of 2-4
players each, with server-driven bots filling the side that is short. Its referee.

Each human runs and aims in the browser (games/strike.js) and streams where they are;
the server runs a 20 Hz tick (app/realtime.py) that checks every move, judges every
shot, keeps the score and sends the lobby one batched snapshot per tick. Movement is
checked the way Blaster Arena checks a runner (no faster than a runner can go, measured
on the sender's own clock; not inside a wall; inside the arena); shots are decided here
with the same lag compensation (see app/fps.py for the full description).

What is new here, on top of the Blaster referee:

  Teams.  Players are split amber (0) / cyan (1). The host picks the side size (2-4)
  at start; both sides are filled with bots until each has that many, and a bot leaves
  first when a human takes its place. A team scores when any of its members makes a
  kill; the round ends at the score limit, at the time, or when one side has no active
  members left.

  Bots.  A bot is a player in the match vector like any other, flagged b=1; the client
  draws it exactly like a human. Every tick the referee thinks for each bot: pick a
  goal (a living enemy when one is in sight, else a waypoint of the patrol graph),
  walk there with the same step() physics the clients use, and fire through the same
  adjudication a human's shot goes through (no rewind is needed - the bot is on the
  server). Bots never send network frames, never have clocks of their own, and are
  marked bots in the results.

The arena lives in strike_map.json (a byte copy of games/strike/map.json): axis-aligned
solid boxes (metres; collision for the players and occlusion for the shots), spawn
points (the first half amber side, the rest cyan side), pickups and the practice
range's drone paths. The shared geometry below mirrors the STRIKE-SHARED block of
games/strike.js (tests/test_strike_sync.py runs both).

Units on the wire: x, y, z in centimetres, yaw and pitch in hundredths of a degree
(yaw 0 looks north, -z; 90 east, +x; pitch up is positive), the sender's clock in
centiseconds (q). Nothing transcript-derived ever travels.
"""
import json
import math
import os
from collections import deque
from typing import Any

# --- movement (shared with games/strike.js) ---
R = 0.35                 # half the width of a player's collision box (m)
H = 1.8                  # its height; positions are the feet
EYE = 1.6                # eye height above the feet
STEP_H = 0.55            # a ledge this high is stepped onto while walking
RUN = 6.0                # m/s
SPRINT = 1.35            # hold Shift (v3): RUN x SPRINT while a movement key is down
# sprint stamina (client-side; the referee only watches raw speed)
STAMINA_MAX = 100
STAMINA_DRAIN = 12.5
STAMINA_REGEN = 14
SPRINT_SLOW = 0.015
# v4: parachute drops — a spawn may lift you and you glide down (mirrors the JS strike)
DROP_HEIGHT = 11.0
DROP_TIME = 3.0
DROP_SINK = 2.8
DROP_DRIFT = 4.5
ACCEL = 12.0             # how fast the run speed follows the stick on the ground (1/s)
AIR_ACCEL = 3.0          # ... and in the air
GRAVITY = 18.0           # m/s^2
JUMP_V = 6.5             # take-off speed (a ~1.17 m jump)
FALL_MAX = 25.0          # terminal fall speed
LEAP_MULT = 3.0          # the leap totem: Space-bar jumps this much taller
SPEED_MULT = 2.0         # the speed totem: plain WASD at this multiple of sprint pace

# --- weapons (shared with games/strike.js) ---
WEAPONS = (
    {"id": "rapid", "name": "Rapid smg", "interval": 0.1, "damage": 12, "head": 18, "mag": 30,
     "reserve": 90, "reload": 1.6, "spread": 1.2, "range": 60.0, "auto": True, "pack": 60},
    {"id": "marksman", "name": "Marksman rifle", "interval": 0.8, "damage": 75, "head": 112, "mag": 5,
     "reserve": 15, "reload": 2.2, "spread": 0.3, "range": 120.0, "auto": False, "pack": 10},
    {"id": "revolver", "name": "Revolver", "interval": 0.5, "damage": 50, "head": 75, "mag": 6,
     "reserve": 24, "reload": 2.2, "spread": 0.5, "range": 70.0, "auto": False, "pack": 12},
    {"id": "launcher", "name": "Launcher", "interval": 1.2, "damage": 120, "head": 120, "mag": 1,
     "reserve": 5, "reload": 2.5, "spread": 0.0, "range": 60.0, "auto": False, "pack": 2},
)
# The launcher's rocket: a real missile that flies and blasts. The Rust Arena (backend-rs,
# the live referee) flies it in its 20 Hz tick; this Python mirror carries the same numbers
# for tests/test_strike_sync.py and shares the constants.
LAUNCH = WEAPONS[3]
MISSILE_V = 22.0         # m/s launch speed
MISSILE_G = 6.0          # gravity on the rocket (gentler than a body)
MISSILE_TTL = 3.5        # s before it fizzles mid-air
BLAST_R = 4.0            # m: splash radius, falloff 75 -> 25
DIRECT_R = 0.8           # m: inside this, a direct hit (120)


def blast_damage(dist: float, direct: bool) -> int:
    """Splash damage by distance from the explosion (mirrors FS.blastDamage)."""
    if direct:
        return 120
    if dist >= BLAST_R:
        return 0
    return round(75 - (75 - 25) * (dist / BLAST_R))


SWITCH = 0.25            # s after a weapon switch before it fires
HP = 100
HEAL = 50                # a health pack
BODY_R = 0.4             # hit boxes around a player's feet position: body ...
BODY_H = 1.4
HEAD_R = 0.25            # ... and head
HEAD_TOP = 1.9

# --- referee ---
HZ = 20                  # server ticks per second while a match is on
MAX_SPEED = 7.5          # m/s across the ground; the browser runs at 6
SLACK = 1.0              # m allowed on top of speed x dt (bunched frames)
SPRINT_MAX_SPEED = 10.5  # v3 clients that send a sprint flag get the higher cap. This frozen
                         # referee stays v1: it never grants it — the constant lives here only
                         # so all three impls keep the same numbers in step.
SHRINK = 0.05            # a position may touch a wall by this much (float noise)
MAX_AIR = 2.0            # s off the ground before a position is not believed
CLOCK_LEAD = 0.6         # s a sender's clock may run ahead of ours, summed over the match
POS_RATE = 25.0          # position frames per second per player (clients send 20)
POS_BURST = 10
FIRE_RATE = 15.0         # fire ops per second per player (the rapid smg fires 10)
FIRE_BURST = 6
OP_RATE = 6.0            # reload / weapon ops per second
OP_BURST = 4
FIX_GAP = 0.5            # at most one correction per this long
ORIGIN_SLACK = 1.0       # m a shot may start from the shooter's last known eye
FIRE_LATE = 0.5          # s: a shot's own clock may be at most this far behind its arrival
IP_CAP = 0.35            # s of interpolation delay a client may claim (100 ms + jitter buffer)
RTT_CAP = 0.4            # s of measured round trip that is compensated
RTT_WINDOW = 3.0         # s of acknowledgements the round trip is the minimum of
HISTORY = 1.2            # s of position history per player (>= the largest rewind)
COUNTDOWN = 4.0          # s from "start" to "go"
RESPAWN = 3.0            # s dead before respawning
PROTECT = 1.5            # s of spawn protection (ends early when you fire)
PICK_R = 1.1             # m to walk over a pickup
PICKUP_BACK = 15.0       # s before a taken pickup is back
GRACE = 15.0             # s a dropped player's score waits on the board for a rejoin

# --- totems (the "hack" pickups): one drops every TOTEM_GAP during a round; the mole only
# appears when a side fields a bot (it can't turn a human). Durations mirror games/strike.js
# TOTEMS, which is the client's word for what each gem does.
TOTEM_GAP = 10.0         # s between totem drops
TOTEM_FIRST = 8.0        # s into the round before the first drop
TOTEM_KINDS = ("mole", "freeze", "decoy", "cloak", "shield", "wings", "leap", "speed", "teleport")
# --- the drop rings: past the map's own packs (the first len(m["pickups"]) slots) sit a
# fixed pool of totem slots then pack slots, reused round-robin so slot indices stay
# stable while the drops themselves keep coming — fresh kind, fresh spot, repeats allowed.
# Loose packs (health/ammo) are their own clock, independent of the totems.
DROP_SLOTS = 3           # recycled totem slots
PACK_SLOTS = 2           # recycled loose-pack slots
PACK_GAP = 8.0           # s between loose-pack drops
PACK_FIRST = 6.0         # s into the round before the first loose pack
CHARM = 8.0              # s a mole-turned bot shoots its own side
FREEZE = 5.0             # s a paralysed bot can't move or fire
SHIELD_TOTEM = 10.0      # s the shield totem's bubble holds (they still shoot)
DECOY = 10.0             # s a decoy ghost stands and eats shots
CLOAK = 12.0             # s enemy bots can't see the cloaked player
WINGS = 10.0             # s the wings totem lets the taker fly (auto rise, glide, auto land)
WINGS_LAND = 5.0         # s beyond WINGS the descent is still judged as wings flight (9 m at WING_SINK 2.0, rounded up)
LEAP = 10.0              # s the leap totem's boosted Space-bar jumps
SPEED = 6.0              # s the speed totem's double-sprint dash on plain WASD
MINUTES = (3, 5, 10)      # menu presets; the host's own 1..60 is accepted too
KILLS = (10, 20, 30, 50)  # menu presets; the host's own 1..500 is accepted too
TEAM_SIZES = (1, 2, 3, 4, 5)   # the host's X in "X vs X"
MAX_PLAYERS = 10
CHARS = 6                # the Mini Characters looks

# --- bots ---
BOT_THINK = 0.25         # s between a bot's decisions
BOT_TURN = 9.0           # how fast a bot swings its aim (radians/s)
BOT_AIM = 0.04           # radians of aim error a bot lets itself (they miss)
BOT_RANGE = 45.0         # m at which a bot will start shooting
BOT_REACT = 0.35         # s a bot takes to open fire once a target is in sight
BOT_SKILL = 0.75         # chance a bot's burst is aimed at the head box
BOT_NAMES = ("Vex", "Rook", "Piston", "Mako", "Juno", "Crow", "Dart", "Onyx")

# In MP the host picks one skill for every bot on BOTH sides (bot-filled teams are how the
# game stays fair with uneven humans, so both teams' fillers must be the same difficulty).
# Each level scales the same BOT_* constants up (aim=1 is the shipped normal feel):
# aim — how wide the deliberate miss cone is, higher = dumber;
# react — how long before a bot opens fire once a target is in sight, higher = slower;
# turn — how fast a bot swings its aim, lower = clunkier; rate — how long between their
# own shots, higher = more breathing room between their bullets.
BOT_DIFFS = {
    "easy":   {"aim": 3.0, "react": 2.0, "turn": 0.5,  "rate": 1.6},
    "normal": {"aim": 1.0, "react": 1.0, "turn": 1.0,  "rate": 1.0},
    "hard":   {"aim": 0.5, "react": 0.35, "turn": 1.6, "rate": 0.8},
    "sweaty": {"aim": 0.22, "react": 0.22, "turn": 2.2, "rate": 0.7},
}


def bot_diff_for(d: str | None) -> dict:
    """The bot-skill table for the HOST-picked MP level; the in-payload id is glossed over
    if it isn't one we know, so a trap wire id can never select a custom super-bot."""
    return BOT_DIFFS.get(d) or BOT_DIFFS["normal"]
BOT_COLORS = ((0.86, 0.55, 0.20), (0.25, 0.72, 0.80))   # per team

# patrol waypoints: (x, z) walked on the ground plane, plus the catwalk ring
WAYPOINTS = (
    (0.0, -9.0), (0.0, 0.0), (0.0, 9.0), (-7.0, 0.0), (7.0, 0.0),
    (-7.0, -9.0), (7.0, -9.0), (-7.0, 9.0), (7.0, 9.0),
    (-14.2, 0.0), (14.2, 0.0), (-11.0, -6.0), (11.0, 6.0),
    (-9.0, -12.8), (9.0, -12.8), (-9.0, 12.8), (9.0, 12.8),
    (-5.5, -5.2), (5.5, 5.2), (-5.5, 5.2), (5.5, -5.2),
    (0.0, -6.0), (0.0, 6.0),
)

_HERE = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(_HERE, "strike_map.json"), encoding="utf-8") as _f:
    DATA = json.load(_f)


def compile_map(d: dict) -> dict:
    boxes = [tuple(float(v) for v in b[:6]) for b in d["boxes"]]
    for b in boxes:
        if not (b[0] < b[3] and b[1] < b[4] and b[2] < b[5]):
            raise ValueError("box with no volume")
    return {"id": d["id"], "name": d["name"], "bounds": tuple(float(v) for v in d["bounds"]), "boxes": boxes,
            "spawns": [tuple(float(v) for v in s) for s in d["spawns"]],
            "pickups": [{"id": p["id"], "kind": p["kind"], "at": tuple(float(v) for v in p["at"])} for p in d["pickups"]],
            "targets": d.get("targets", [])}


MAP = compile_map(DATA)


# ------------------------------------------------------- shared geometry --
def overlaps(m: dict, x: float, y: float, z: float, shrink: float = 0.0) -> int:
    """Index of the first solid box a player standing at (x, y, z) is inside, else -1.
    Touching is not inside."""
    x0, x1 = x - R + shrink, x + R - shrink
    y0, y1 = y + shrink, y + H - shrink
    z0, z1 = z - R + shrink, z + R - shrink
    for i, b in enumerate(m["boxes"]):
        if x1 > b[0] and x0 < b[3] and y1 > b[1] and y0 < b[4] and z1 > b[2] and z0 < b[5]:
            return i
    return -1


def top_under(m: dict, x: float, y: float, z: float) -> float | None:
    """The highest box top under a player at (x, y, z) among the boxes it overlaps."""
    best = None
    x0, x1, z0, z1 = x - R, x + R, z - R, z + R
    for b in m["boxes"]:
        if x1 > b[0] and x0 < b[3] and y + H > b[1] and y < b[4] and z1 > b[2] and z0 < b[5]:
            if best is None or b[4] > best:
                best = b[4]
    return best


def move(m: dict, s: dict, wx: float, wz: float, jump: bool, dt: float, sprint: bool = False) -> dict:
    """One step of the runner: s = {x, y, z, vx, vy, vz, g (on the ground)}; (wx, wz) the
    wished direction in world space (length <= 1). Mutates and returns s. Axis by axis
    against the solid boxes; a low ledge is stepped onto while on the ground. sprint scales
    the speed the runner accelerates toward (False everywhere but a v3 client's hold)."""
    top = RUN * (SPRINT if sprint else 1)
    k = min(1.0, dt * (ACCEL if s["g"] else AIR_ACCEL))
    s["vx"] += (wx * top - s["vx"]) * k
    s["vz"] += (wz * top - s["vz"]) * k
    if jump and s["g"]:
        s["vy"] = JUMP_V
        s["g"] = False
    s["vy"] = max(-FALL_MAX, s["vy"] - GRAVITY * dt)
    for ax in ("x", "z"):
        old = s[ax]
        s[ax] = old + s["v" + ax] * dt
        i = overlaps(m, s["x"], s["y"], s["z"])
        if i >= 0:
            top = m["boxes"][i][4]
            if s["g"] and 0.0 < top - s["y"] <= STEP_H and overlaps(m, s["x"], top, s["z"]) < 0:
                s["y"] = top
            else:
                s[ax] = old
                s["v" + ax] = 0.0
    ny = s["y"] + s["vy"] * dt
    if overlaps(m, s["x"], ny, s["z"]) < 0:
        s["y"] = ny
        s["g"] = False
    elif s["vy"] <= 0.0:
        top = top_under(m, s["x"], ny, s["z"])
        if top is not None and top <= s["y"] + 1e-9:
            s["y"] = top
        s["vy"] = 0.0
        s["g"] = True
    else:
        s["vy"] = 0.0
    return s


def dir_of(yaw: float, pitch: float) -> tuple[float, float, float]:
    """Unit view direction for yaw/pitch in degrees (yaw 0 = -z, 90 = +x; pitch up +)."""
    y, p = math.radians(yaw), math.radians(pitch)
    cp = math.cos(p)
    return math.sin(y) * cp, math.sin(p), -math.cos(y) * cp


def yaw_of(dx: float, dz: float) -> float:
    """The yaw (degrees) that looks along (dx, dz) - the inverse of dir_of on the plane."""
    return math.degrees(math.atan2(dx, -dz))


def mb32(a: int) -> float:
    """mulberry32, one draw: the same 32-bit integer steps as the browser's."""
    a = (a + 0x6D2B79F5) & 0xFFFFFFFF
    t = _imul(a ^ (a >> 15), 1 | a)
    t = ((t + _imul(t ^ (t >> 7), 61 | t)) & 0xFFFFFFFF) ^ t
    return ((t ^ (t >> 14)) & 0xFFFFFFFF) / 4294967296


def _imul(a: int, b: int) -> int:
    return (a * b) & 0xFFFFFFFF


def hash_match(seed: int, salt: int) -> float:
    """The totem drop rng (games/strike.js hashMatch): one salted draw from a seed."""
    x = (seed ^ _imul(salt, 0x9E3779B9)) & 0xFFFFFFFF
    return x / 4294967295


def spread_of(n: int, w: int) -> tuple[float, float]:
    """The deterministic spread (degrees of yaw, pitch) of a player's n-th shot."""
    s = WEAPONS[w]["spread"]
    seed = (n * 2654435761 + w * 40503) & 0xFFFFFFFF
    return (mb32(seed) * 2 - 1) * s, (mb32(seed ^ 0x5BD1E995) * 2 - 1) * s


def ray_box(o: tuple, d: tuple, b: tuple) -> float | None:
    """Distance along the ray o + t d (t >= 0) to the box (x0, y0, z0, x1, y1, z1); 0 if
    o is inside it; None if it misses."""
    tmin, tmax = 0.0, math.inf
    for a in range(3):
        if abs(d[a]) < 1e-12:
            if o[a] < b[a] or o[a] > b[a + 3]:
                return None
        else:
            inv = 1.0 / d[a]
            t1 = (b[a] - o[a]) * inv
            t2 = (b[a + 3] - o[a]) * inv
            if t1 > t2:
                t1, t2 = t2, t1
            if t1 > tmin:
                tmin = t1
            if t2 < tmax:
                tmax = t2
            if tmin > tmax:
                return None
    return tmin


def ray_map(m: dict, o: tuple, d: tuple, maxd: float) -> float:
    """Distance to the first solid box along the ray, or maxd."""
    best = maxd
    for b in m["boxes"]:
        t = ray_box(o, d, b)
        if t is not None and t < best:
            best = t
    return best


def hit_boxes(x: float, y: float, z: float) -> tuple[tuple, tuple]:
    return ((x - BODY_R, y, z - BODY_R, x + BODY_R, y + BODY_H, z + BODY_R),
            (x - HEAD_R, y + BODY_H, z - HEAD_R, x + HEAD_R, y + HEAD_TOP, z + HEAD_R))


def ray_player(o: tuple, d: tuple, x: float, y: float, z: float, maxd: float) -> tuple[float, bool] | None:
    """(distance, head?) where the ray first meets a player standing at (x, y, z), if
    nearer than maxd."""
    body, head = hit_boxes(x, y, z)
    tb, th = ray_box(o, d, body), ray_box(o, d, head)
    best = None
    if tb is not None and tb < maxd:
        best = (tb, False)
    if th is not None and th < maxd and (best is None or th < best[0]):
        best = (th, True)
    return best


def target_at(tg: dict, t: float) -> tuple[float, float, float]:
    """A practice drone's centre at time t: back and forth between a and b at s m/s."""
    a, b, s = tg["a"], tg["b"], float(tg["s"])
    ln = math.dist(a, b) or 1.0
    ph = (t * s / ln) % 2.0
    u = ph if ph <= 1.0 else 2.0 - ph
    return tuple(a[i] + (b[i] - a[i]) * u for i in range(3))


def as_num(v: Any, lo: float, hi: float) -> float | None:
    if isinstance(v, bool) or not isinstance(v, (int, float)) or v != v or v in (float("inf"), float("-inf")):
        return None
    return min(hi, max(lo, float(v)))


def los(m: dict, ax: float, ay: float, az: float, bx: float, by: float, bz: float) -> bool:
    """Line of sight between two eye points: True when no solid box is in the way."""
    dx, dy, dz = bx - ax, by - ay, bz - az
    d = math.sqrt(dx * dx + dy * dy + dz * dz)
    if d < 1e-9:
        return True
    return ray_map(m, (ax, ay, az), (dx / d, dy / d, dz / d), d) >= d - 1e-6


# ----------------------------------------------------------------- referee --
class Strike:
    """One room's match. idle -> warmup (countdown) -> round -> done; the host starts the
    next one from done or idle. Players who join the lobby mid-round drop in. Humans are
    split amber/cyan as evenly as they arrive; bots top each side up to the host's X."""

    def __init__(self) -> None:
        self.m = MAP
        self.phase = "idle"
        self.minutes = 5
        self.limit = 30
        self.tsize = 2              # the host's X vs X
        self.bot_diff = "normal"    # the host-picked skill every bot on both sides plays at
        self.bot_aim = 1.0          # aim-error multiplier (higher = dumber)
        self.bot_react = 1.0        # reaction multiplier (higher = slower)
        self.bot_turn = 1.0         # aim-swing speed multiplier (higher = snappier)
        self.bot_rate = 1.0         # refire gap multiplier (higher = fewer shots)
        self.scores = [0, 0]        # team kill totals
        self.go_at = 0.0
        self.ends_at = 0.0
        self.players: dict[str, dict] = {}
        self.chars: dict[str, int] = {}             # user_id -> look, kept between matches
        self.results: list[dict] | None = None
        self.items: list[float | None] = []         # per pickup: None = there, else when it returns
        # totem slots appended to the map's pickups this round (kind/spot per slot; "" = a
        # map pack). Mirrors the client: base slots are health/ammo, totems append and reset.
        self.kinds: list[str] = []
        self.spots: list[tuple[float, float, float]] = []
        self.totem_at: float | None = None          # when the next totem drops
        self.totem_k = 0                            # drop counter: the rng seed
        self.pack_at: float | None = None           # when the next loose pack drops
        self.pack_k = 0                             # loose-pack drop counter
        self.decoys: list[dict] = []                # ghosts in flight
        self.dirty: set[str] = set()
        self.shots: list[list] = []                 # judged since the last snapshot
        self.pending: list[tuple[str, dict]] = []   # kills, pickups: out with the next tick
        self.k = 0                                  # tick number
        self.sent: deque = deque()                  # (tick, time) of the snapshots sent

    # -- views --
    def ms_left(self, t: float) -> int:
        if self.phase == "round":
            return max(0, int((self.ends_at - t) * 1000))
        return int(self.minutes * 60000) if self.phase == "warmup" else 0

    def _pub(self, uid: str, p: dict) -> dict:
        return {"user": p["user"], "slot": p["slot"], "team": p["team"], "bot": 1 if p["bot"] else 0,
                "char": p["char"], "kills": p["kills"], "deaths": p["deaths"],
                "hp": p["hp"], "dead": p["dead"], "away": p["away"] is not None, "gone": p["gone"], "w": p["w"],
                "x": round(p["x"] * 100), "y": round(p["y"] * 100), "z": round(p["z"] * 100), "r": p["r"],
                "e": p["life"]}

    def view(self, t: float) -> dict:
        return {"map": self.m["id"], "phase": self.phase, "minutes": self.minutes, "limit": self.limit,
                "tsize": self.tsize, "scores": list(self.scores),
                "goInMs": max(0, int((self.go_at - t) * 1000)) if self.phase == "warmup" else 0,
                "msLeft": self.ms_left(t), "players": [self._pub(u, p) for u, p in self.players.items()],
                "items": [1 if a is None else 0 for a in self.items], "results": self.results,
                "kinds": list(self.kinds), "chars": dict(self.chars)}

    def order(self) -> list[str]:
        return [u for u, _ in sorted(self.players.items(),
                                     key=lambda it: (it[1]["team"], -it[1]["kills"], it[1]["deaths"], it[1]["slot"]))]

    def team_of(self, uid: str) -> int:
        p = self.players.get(uid)
        return p["team"] if p else 0

    def mates(self, uid: str) -> list[str]:
        t = self.team_of(uid)
        return [u for u, p in self.players.items() if p["team"] == t]

    # -- setup --
    def char(self, uid: str, msg: dict) -> int | None:
        c = as_num(msg.get("c"), -1, CHARS)
        if c is None or c != int(c) or not 0 <= c < CHARS:
            return None
        self.chars[uid] = int(c)
        if uid in self.players:
            self.players[uid]["char"] = int(c)
        return int(c)

    def _new(self, pub: dict, slot: int, uid: str, t: float, team: int, bot: bool = False) -> dict:
        b = None
        if bot:
            b = {"goal": None, "wpi": (slot * 7) % len(WAYPOINTS), "think_at": t + BOT_THINK,
                 "target": None, "seen_at": None, "aim_y": 0.0, "aim_p": 0.0, "err_y": 0.0, "err_p": 0.0,
                 "err_at": 0.0, "fire_at": None, "jump": False}
        return {"user": pub, "slot": slot, "team": team, "bot": b, "char": self.chars.get(uid, slot % CHARS),
                "x": 0.0, "y": 0.0, "z": 0.0, "r": 0, "pt": 0, "hp": HP, "w": 0,
                "mag": [w["mag"] for w in WEAPONS], "res": [w["reserve"] for w in WEAPONS],
                "last": [deque(), deque()], "ready": -(1 << 30), "reload": None, "n": 0,
                "kills": 0, "deaths": 0, "dead": False, "respawn_at": 0.0, "life": 0, "spawn_t": t,
                "protect": 0.0, "away": None, "gone": False,
                "q": None, "q0": 0, "t0": t, "off": None, "qmax": None, "at": t, "air_at": t,
                "pb": [float(POS_BURST), t], "fb": [float(FIRE_BURST), t], "ob": [float(OP_BURST), t],
                "fix_at": -1e9, "sync_at": -1e9, "bad": 0, "hist": deque(), "rtts": deque(), "rtt": 0.0,
                # totem windows (server clock): charm (mole), freeze (paralyser), cloak,
                # the shield totem's bubble (they still shoot), and the three movement
                # totems (wings flight, leap jumps, speed dash) — all pop on death
                "charm_until": 0.0, "freeze_until": 0.0, "cloak_until": 0.0, "shield_totem_until": 0.0,
                # wings_fly is the cruise boundary (rise+hover — the shown window); the
                # judged window (wings_until) runs past it while the flyer glides back down
                "wings_until": 0.0, "wings_fly": 0.0, "leap_until": 0.0, "speed_until": 0.0}

    def _side_spawns(self, team: int) -> list[int]:
        """The spawn indices of one side: the first half of the list is amber's, the rest
        cyan's. Spawns 0-6 amber, 7-13 cyan."""
        n = len(self.m["spawns"])
        half = n // 2
        return list(range(0, half)) if team == 0 else list(range(half, n))

    def _place(self, p: dict, i: int, t: float) -> None:
        sx, sy, sz, yaw = self.m["spawns"][i]
        p.update(x=sx, y=sy, z=sz, r=int(yaw * 100) % 36000, pt=0, hp=HP, dead=False, w=p["w"],
                 mag=[w["mag"] for w in WEAPONS], res=[w["reserve"] for w in WEAPONS], reload=None,
                 spawn_t=t, protect=t + PROTECT, air_at=t, n=0)     # the spread sequence restarts each life
        p["charm_until"] = 0.0
        p["freeze_until"] = 0.0
        p["shield_totem_until"] = 0.0   # a death pops the shield totem's bubble too
        p["wings_until"] = 0.0
        p["wings_fly"] = 0.0
        p["leap_until"] = 0.0
        p["speed_until"] = 0.0
        p["life"] += 1
        p["hist"] = deque([(t, sx, sy, sz)])
        if p["bot"]:
            p["bot"]["goal"] = None
            p["bot"]["target"] = None
            p["bot"]["fire_at"] = None
            b = p["bot"]
            b["aim_y"], b["aim_p"] = yaw, 0.0
            s = self.m["spawns"][i]
            # start the patrol from the waypoint nearest this spawn
            b["wpi"] = min(range(len(WAYPOINTS)),
                           key=lambda j: (WAYPOINTS[j][0] - s[0]) ** 2 + (WAYPOINTS[j][1] - s[2]) ** 2)

    def _fill_bots(self, t: float) -> None:
        """Top each side up to the host's X with bots; retire bots when humans abound."""
        for team in (0, 1):
            side = [u for u, p in self.players.items() if p["team"] == team and not p["gone"]]
            humans = [u for u in side if not self.players[u]["bot"]]
            want = self.tsize
            # retire extra bots (humans arrived): the most recently added first
            while len(side) > want and any(self.players[u]["bot"] for u in side):
                bot = max((u for u in side if self.players[u]["bot"]),
                          key=lambda u: self.players[u]["slot"])
                self.players[bot]["gone"] = True
                side.remove(bot)
            # add bots on the short side
            k = 0
            while len(side) < want:
                uid = "bot-%d-%d" % (team, k)
                while uid in self.players:
                    k += 1
                    uid = "bot-%d-%d" % (team, k)
                slot = 1 + max((q["slot"] for q in self.players.values()), default=-1)
                name = BOT_NAMES[(team * 4 + k) % len(BOT_NAMES)]
                col = BOT_COLORS[team % 2]
                pub = {"id": uid, "name": name, "color": col, "bot": 1}
                p = self.players[uid] = self._new(pub, slot, uid, t, team, bot=True)
                spawns = self._side_spawns(team)
                self._place(p, spawns[k % len(spawns)], t)
                side.append(uid)
                k += 1
                self.dirty.add(uid)

    def _human_side(self, uid: str) -> int:
        """The side a joining human takes: the short one (bots there get retired)."""
        counts = [0, 0]
        for u, p in self.players.items():
            if not p["bot"] and not p["gone"]:
                counts[p["team"]] += 1
        if uid in self.players:
            return self.players[uid]["team"]
        return 0 if counts[0] <= counts[1] else 1

    def set_bot_diff(self, d: str | None) -> None:
        """Apply one MP bot-skill level to every bot the match will field (aim is the miss
        cone, react the delay before firing on a sighting, turn the aim-swing speed, rate
        the gap between a bot's own shots). An unknown id — including one a crafted wire
        message could carry — quietly falls back to normal."""
        x = bot_diff_for(d)
        self.bot_diff = d if d in BOT_DIFFS else "normal"
        self.bot_aim, self.bot_react, self.bot_turn, self.bot_rate = \
            x["aim"], x["react"], x["turn"], x["rate"]

    def start(self, members: dict[str, dict], minutes: Any, kills: Any, t: float, tsize: Any = None,
              diff: Any = None) -> str | None:
        if self.phase in ("warmup", "round"):
            return "a match is already on: the host can end it first"
        # the host's own scoreline: any length in 1..60 minutes and target in 1..500 kills
        # (MINUTES/KILLS above are the menu's quick presets, not a gate)
        mi, ki = as_num(minutes, 1, 60), as_num(kills, 1, 500)
        self.minutes = int(mi) if mi is not None else 5
        self.limit = int(ki) if ki is not None else 30
        ti = as_num(tsize, 0, 1000)
        self.tsize = int(ti) if ti is not None and int(ti) in TEAM_SIZES else 2
        # one skill for every bot on BOTH sides — bot-filled teams are how uneven humans
        # meet a fair lobby, so no team may get sharper fillers than the other
        self.set_bot_diff(diff if isinstance(diff, str) else None)
        self.phase = "warmup"
        self.go_at = t + COUNTDOWN
        self.results = None
        self.players = {}
        self.scores = [0, 0]
        # nothing is on the map at the start: the fixed packs stay dark (1e9 = past any
        # round, so the refill timer never lights them) and every item arrives mid-match
        # from a drop clock — totems every TOTEM_GAP, packs every PACK_GAP, each running
        # regardless of whether the previous drop was taken
        self.items = [1e9] * len(self.m["pickups"])
        self.kinds = [""] * len(self.m["pickups"])
        self.spots = [(0.0, 0.0, 0.0)] * len(self.m["pickups"])
        self._ensure_drop_slots()   # the recycled drop slots exist (empty) for the whole match
        self.totem_at = None
        self.totem_k = 0
        self.pack_at = None
        self.pack_k = 0
        self.decoys = []
        self.shots, self.pending = [], []
        self.sent = deque()
        # humans first, split evenly across the two sides
        for k, (uid, pub) in enumerate(list(members.items())[:MAX_PLAYERS]):
            team = k % 2
            p = self.players[uid] = self._new(pub, k, uid, t, team)
            spawns = self._side_spawns(team)
            self._place(p, spawns[(k // 2) % len(spawns)], t)
        self._fill_bots(t)
        self.dirty = set(self.players)
        return None

    def end(self) -> None:
        self.phase = "idle"
        self.players = {}
        self.results = None
        self.shots, self.pending = [], []

    def running(self) -> bool:
        return self.phase in ("warmup", "round")

    def active(self) -> list[str]:
        return [u for u, p in self.players.items() if not p["gone"]]

    # -- comings and goings --
    def enter(self, uid: str, pub: dict, t: float) -> bool:
        """A lobby member (re)joined while a match is on: a dropped player gets their score
        back; someone new drops in (and spawns on the next tick). True if the match changed."""
        if not self.running():
            return False
        p = self.players.get(uid)
        if p is not None:
            was_gone = p["gone"]
            p["away"], p["gone"], p["user"] = None, False, pub
            p["q"], p["off"], p["qmax"] = None, None, None      # a reloaded page starts a new clock
            p["rtts"] = deque()
            p["pb"], p["fb"], p["ob"] = [float(POS_BURST), t], [float(FIRE_BURST), t], [float(OP_BURST), t]
            if was_gone and self.phase == "round" and not p["dead"]:
                p["dead"], p["respawn_at"] = True, t           # back from the bench: a fresh spawn
            self.dirty.add(uid)
            self._fill_bots(t)
            return True
        humans = [u for u, q in self.players.items() if not q["bot"] and not q["gone"]]
        if len(humans) >= MAX_PLAYERS:
            return False
        team = self._human_side(uid)
        slot = 1 + max((q["slot"] for q in self.players.values()), default=-1)
        p = self.players[uid] = self._new(pub, slot, uid, t, team)
        if self.phase == "warmup":
            spawns = self._side_spawns(team)
            self._place(p, spawns[(slot * 5) % len(spawns)], t)
        else:
            p["dead"], p["respawn_at"] = True, t
        self.dirty.add(uid)
        self._fill_bots(t)
        return True

    def drop(self, uid: str, t: float, blip: bool = False) -> bool:
        """A player left. A dropped socket (blip) keeps their place for GRACE seconds; an
        explicit leave benches them at once. Their score stays on the board either way."""
        p = self.players.get(uid)
        if p is None or not self.running() or p["gone"]:
            return False
        if blip:
            p["away"] = t
        else:
            p["gone"] = True
        self.dirty.add(uid)
        self._fill_bots(t)
        return True

    # -- clocks --
    def _bucket(self, b: list, rate: float, burst: float, t: float) -> bool:
        b[0] = min(float(burst), b[0] + (t - b[1]) * rate)
        b[1] = t
        if b[0] < 1.0:
            return False
        b[0] -= 1.0
        return True

    def _lead_ok(self, p: dict, q: int, t: float) -> bool:
        """The sender's clock may never get more than CLOCK_LEAD ahead of ours since its
        first frame: a fast clock buys no speed and no rate."""
        if p["qmax"] is None:
            p["q0"], p["t0"], p["qmax"] = q, t, q
            return True
        return (q - p["q0"]) / 100.0 <= (t - p["t0"]) + CLOCK_LEAD

    def _seen(self, p: dict, q: int, t: float) -> None:
        sample = t - q / 100.0
        if p["off"] is None or sample < p["off"]:
            p["off"] = sample
        if p["qmax"] is None or q > p["qmax"]:
            p["qmax"] = q

    def _ack(self, p: dict, msg: dict, q: int | None, t: float) -> None:
        """A frame acknowledging snapshot tick k, which reached the sender at its clock
        kq: one round-trip sample (our send -> their arrival, mapped onto our clock, which
        adds the sender's fastest trip up). Without kq the frame's own time stands in."""
        kk = as_num(msg.get("k"), 0, 1 << 30)
        if kk is None or q is None or p["off"] is None:
            return
        kk = int(kk)
        kq = as_num(msg.get("kq"), 0, (1 << 30) - 1)
        arrived = int(kq) if kq is not None and q - 100 <= kq <= q else q
        mapped = arrived / 100.0 + p["off"]
        for tick, at in reversed(self.sent):
            if tick == kk:
                p["rtts"].append((t, max(0.0, mapped - at)))
                break
            if tick < kk:
                break
        while p["rtts"] and t - p["rtts"][0][0] > RTT_WINDOW:
            p["rtts"].popleft()
        if p["rtts"]:
            p["rtt"] = min(s for _, s in p["rtts"])

    # -- movement --
    def pos(self, uid: str, msg: dict, t: float) -> tuple[bool, dict | None]:
        """A position frame. Returns (accepted, correction to send back). Ignored before
        the round, while dead, and from an earlier life (frames still in flight)."""
        p = self.players.get(uid)
        if p is None or p["bot"] or self.phase != "round" or p["dead"] or p["gone"]:
            return False, None
        if not self._bucket(p["pb"], POS_RATE, POS_BURST, t):
            return False, None
        x, y, z = (as_num(msg.get(c), -1e6, 1e6) for c in ("x", "y", "z"))
        r, pt = as_num(msg.get("r"), -1e6, 1e6), as_num(msg.get("p"), -9000, 9000)
        if x is None or y is None or z is None or r is None:
            return False, None
        e = as_num(msg.get("e"), 0, 1 << 30)
        if e is not None and int(e) != p["life"]:
            return False, None
        q = as_num(msg.get("q"), 0, (1 << 30) - 1)
        q = int(q) if q is not None else None
        if q is not None and p["q"] is not None:
            if q <= p["q"]:
                return False, None                   # older than what we have: stale, not cheating
            dt = (q - p["q"]) / 100.0 if self._lead_ok(p, q, t) else -1.0
        else:
            if q is not None:
                self._lead_ok(p, q, t)
            dt = max(0.0, t - p["at"])
        xm, ym, zm = x / 100.0, y / 100.0, z / 100.0
        b = self.m["bounds"]
        ok = dt >= 0 and b[0] <= xm <= b[3] and b[1] <= ym <= b[4] and b[2] <= zm <= b[5]
        if ok:
            across = math.hypot(xm - p["x"], zm - p["z"])
            dy = ym - p["y"]
            # the movement totems buy room while their windows run: the speed totem's dash
            # earns the raised cap, the leap totem's jumps earn the taller dy, and the
            # wings totem's flight parks the air clock (a flyer is off the ground by design).
            # (This frozen v1 referee never grants the sp sprint flag; only the speed
            # totem — which the server itself judged — raises the cap here.)
            cap = MAX_SPEED
            if p.get("speed_until", 0.0) > t:
                cap = SPRINT_MAX_SPEED * SPEED_MULT
            up = JUMP_V * (LEAP_MULT if p.get("leap_until", 0.0) > t else 1.0)
            ok = (across <= cap * dt + SLACK and dy <= up * dt + STEP_H + SLACK
                  and -dy <= FALL_MAX * dt + SLACK and overlaps(self.m, xm, ym, zm, SHRINK) < 0)
        # the air clock runs on server time (the sender's clock mapped onto ours)
        clock = q / 100.0 + p["off"] if q is not None and p["off"] is not None else t
        grounded = False
        if ok:
            grounded = overlaps(self.m, xm, ym - 0.1, zm) >= 0
            if not grounded and clock - p["air_at"] > MAX_AIR and p.get("wings_until", 0.0) <= t:
                ok = False
        if not ok:
            p["bad"] += 1
            if t - p["fix_at"] < FIX_GAP:
                return False, None
            p["fix_at"] = t
            return False, {"x": round(p["x"] * 100), "y": round(p["y"] * 100), "z": round(p["z"] * 100), "e": p["life"]}
        if q is not None:
            self._seen(p, q, t)
            p["q"] = q
            mapped = q / 100.0 + p["off"]
        else:
            mapped = t
        if grounded:
            p["air_at"] = mapped
        p["x"], p["y"], p["z"] = xm, ym, zm
        p["r"] = int(r) % 36000
        p["pt"] = int(pt) if pt is not None else 0
        p["at"] = t
        h = p["hist"]
        h.append((mapped, xm, ym, zm))
        while len(h) > 2 and mapped - h[0][0] > HISTORY:
            h.popleft()
        self._ack(p, msg, q, t)
        self.dirty.add(uid)
        self._pickups(uid, p, t)
        return True, None

    def _take_totem(self, uid: str, p: dict, kind: str, t: float) -> bool:
        """Apply one totem to the taker (server-judged in MP, exactly what the solo
        client's totemApply() does locally). False: nothing eligible, the slot stays."""
        if kind == "mole":
            foes = [(u, v) for u, v in self.players.items()
                    if u != uid and v["bot"] and not v["dead"] and not v["gone"] and v["team"] != p["team"]]
            if not foes:
                return False
            vid, _ = min(foes, key=lambda kv: kv[1]["slot"])
            self.players[vid]["charm_until"] = t + CHARM
            self.dirty.add(vid)
            return True
        if kind == "freeze":
            foes = [(u, v) for u, v in self.players.items()
                    if u != uid and v["bot"] and not v["dead"] and not v["gone"] and v["team"] != p["team"]]
            if not foes:
                return False
            vid, v = min(foes, key=lambda kv: math.hypot(kv[1]["x"] - p["x"], kv[1]["z"] - p["z"]))
            v["freeze_until"] = t + FREEZE
            self.dirty.add(vid)
            return True
        if kind == "decoy":
            yaw = p["r"] / 100.0
            vx, vz = math.sin(math.radians(yaw)), -math.cos(math.radians(yaw))
            x, z = p["x"] + vx * 3.0, p["z"] + vz * 3.0
            for k in range(20):
                if overlaps(self.m, x, 0.1, z, 0.2) < 0:
                    break
                x, z = p["x"] + vx * (3.0 + k * 0.3), p["z"] + vz * (3.0 + k * 0.3)
            self.decoys.append({"id": self.totem_k, "x": x, "y": 0.0, "z": z, "yaw": yaw,
                                "owner": uid, "until": t + DECOY})
            return True
        if kind == "cloak":
            p["cloak_until"] = t + CLOAK
            return True
        if kind == "shield":
            # the drop shield, bottled: nothing hurts the taker while they keep shooting
            p["shield_totem_until"] = t + SHIELD_TOTEM
            return True
        if kind == "wings":
            # the window the referee judges covers the whole flight: rise, cruise, then the
            # automatic glide back to the ground — the client flies all three without input.
            # rise+cruise is the shown window; the judged window also covers the automatic
            # glide back down so the descent isn't read as a teleport
            p["wings_until"] = t + WINGS + WINGS_LAND
            p["wings_fly"] = t + WINGS
            return True
        if kind == "leap":
            p["leap_until"] = t + LEAP
            return True
        if kind == "speed":
            p["speed_until"] = t + SPEED
            return True
        if kind == "teleport":
            # instant: a random open ground spot, seeded from the take counter so every
            # hop lands somewhere fresh. The referee plants the body there itself (hist
            # rebuilt, air clock reset) so the next pos frame starts from the destination
            # instead of earning a correction for "teleporting".
            self.totem_k += 1
            tx, tz = self._open_spot(self.totem_k * 2654435761 & 0xFFFFFFFF)
            p.update(x=tx, y=0.0, z=tz)
            p["hist"] = deque([(t, tx, 0.0, tz)])
            p["air_at"] = t
            p["wings_until"] = 0.0
            p["wings_fly"] = 0.0
            return True
        return False

    def _drop_rings(self) -> tuple[int, int, int]:
        """The drop-slot layout: [first totem slot, first pack slot, end). Past the map's
        own packs — fixed all match, so a drop's index means the same thing everywhere."""
        n_map = len(self.m["pickups"])
        return n_map, n_map + DROP_SLOTS, n_map + DROP_SLOTS + PACK_SLOTS

    def _ensure_drop_slots(self) -> None:
        """The drop-slot entries exist (kind "" = empty) for the whole round; drops
        rewrite them round-robin. Called at start and before any drop lands."""
        a, b, c = self._drop_rings()
        while len(self.kinds) < c:
            self.items.append(None)
            self.kinds.append("")
            self.spots.append((0.0, 0.0, 0.0))

    def _open_spot(self, seed: int) -> tuple[float, float]:
        """A random open spot for a drop: hashed from the seed, nudged off any box."""
        x = -17.0 + hash_match(seed, 2) * 34.0
        z = -10.0 + hash_match(seed, 3) * 20.0
        for k in range(60):
            if overlaps(self.m, x, 0.1, z, 0.3) < 0:
                break
            x = -17.0 + hash_match(seed ^ k, 2) * 34.0
            z = -10.0 + hash_match(seed ^ k, 3) * 20.0
        return x, z

    def _spawn_totem(self, can_mole: bool) -> None:
        """Drop one totem: a fresh kind and spot derived from the drop counter, landing in
        the OLDEST totem slot of the ring (replacing whatever sat there, taken or not),
        announced to the room by the "totem" event tick() emits. (The mole only drops
        with a bot — it can't turn a human. Repetitions allowed: kinds repeat by chance.)"""
        self._ensure_drop_slots()
        a, _, _ = self._drop_rings()
        self.totem_k += 1
        seed = _imul(self.totem_k, 0x9E3779B9) ^ (self.k & 0xFFFFFFFF)
        kinds = TOTEM_KINDS if can_mole else TOTEM_KINDS[1:]
        kind = kinds[int(hash_match(seed, 1) * len(kinds)) % len(kinds)]
        x, z = self._open_spot(seed)
        i = a + (self.totem_k - 1) % DROP_SLOTS
        self.items[i] = None
        self.kinds[i] = kind
        self.spots[i] = (x, 0.0, z)
        self.pending.append(("totem", {"i": i, "kind": kind, "x": round(x * 100), "z": round(z * 100)}))

    def _spawn_pack(self) -> None:
        """Drop one loose pack (health or ammo, even odds): its own clock, independent of
        the totems — same recycled-slot ring, same fresh random spot each time."""
        self._ensure_drop_slots()
        _, b, _ = self._drop_rings()
        self.pack_k += 1
        seed = _imul(self.pack_k, 0x9E3779B9) ^ (self.k & 0xFFFFFFFF) ^ 0x5F356495
        kind = "ammo" if hash_match(seed, 4) >= 0.5 else "health"
        x, z = self._open_spot(seed)
        i = b + (self.pack_k - 1) % PACK_SLOTS
        self.items[i] = None
        self.kinds[i] = kind
        self.spots[i] = (x, 0.0, z)
        self.pending.append(("pack", {"i": i, "kind": kind, "x": round(x * 100), "z": round(z * 100)}))

    def _pickups(self, uid: str, p: dict, t: float) -> None:
        n_map = len(self.m["pickups"])
        for i in range(len(self.items)):
            if self.items[i] is not None:
                continue
            if i >= n_map:
                # a drop slot: the spot came with the "totem"/"pack" event. Loose packs
                # refill like the map's own; totems are judged here, exactly like the
                # solo client's local take
                kind = self.kinds[i]
                if not kind:
                    continue
                ax, ay, az = self.spots[i]
                if math.hypot(p["x"] - ax, p["z"] - az) > PICK_R or abs(p["y"] - ay) > 1.2:
                    continue
                if kind in ("health", "ammo"):
                    if kind == "health":
                        if p["hp"] >= HP:
                            continue
                        p["hp"] = min(HP, p["hp"] + HEAL)
                    else:
                        full = all(p["res"][w] >= WEAPONS[w]["reserve"] * 2 for w in range(len(WEAPONS)))
                        if full:
                            continue
                        p["res"] = [min(WEAPONS[w]["reserve"] * 2, p["res"][w] + WEAPONS[w]["pack"]) for w in range(len(WEAPONS))]
                    self.items[i] = t + 1e9
                    self.pending.append(("pick", {"i": i, "user": uid, "hp": p["hp"], "res": list(p["res"]),
                                                  "kind": kind}))
                    continue
                if not self._take_totem(uid, p, kind, t):
                    continue
                # a taken ring slot does NOT respawn in place: the drop clock replaces it
                # with a fresh drop (new kind, new spot) within one gap. Mark it gone for
                # good by pushing its return past any round length.
                self.items[i] = t + 1e9
                ev = {"i": i, "user": uid, "hp": p["hp"], "res": list(p["res"]), "kind": kind}
                if kind == "teleport":
                    ev["tx"] = round(p["x"] * 100)   # the destination, so the taker's client snaps
                    ev["tz"] = round(p["z"] * 100)
                self.pending.append(("pick", ev))
                continue
            pk = self.m["pickups"][i]
            ax, ay, az = pk["at"]
            if math.hypot(p["x"] - ax, p["z"] - az) > PICK_R or abs(p["y"] - ay) > 1.2:
                continue
            if pk["kind"] == "health":
                if p["hp"] >= HP:
                    continue
                p["hp"] = min(HP, p["hp"] + HEAL)
            else:
                full = all(p["res"][w] >= WEAPONS[w]["reserve"] * 2 for w in range(len(WEAPONS)))
                if full:
                    continue
                p["res"] = [min(WEAPONS[w]["reserve"] * 2, p["res"][w] + WEAPONS[w]["pack"]) for w in range(len(WEAPONS))]
            self.items[i] = t + PICKUP_BACK
            self.pending.append(("pick", {"i": i, "user": uid, "hp": p["hp"], "res": list(p["res"])}))

    def where(self, p: dict, at: float) -> tuple[float, float, float]:
        """Where a player was at server time `at`, from their history (held at the ends)."""
        h = p["hist"]
        if not h:
            return p["x"], p["y"], p["z"]
        if at <= h[0][0]:
            return h[0][1:]
        if at >= h[-1][0]:
            return h[-1][1:]
        lo, hi = 0, len(h) - 1
        while hi - lo > 1:
            mid = (lo + hi) // 2
            if h[mid][0] <= at:
                lo = mid
            else:
                hi = mid
        a, b = h[lo], h[hi]
        u = (at - a[0]) / ((b[0] - a[0]) or 1.0)
        return a[1] + (b[1] - a[1]) * u, a[2] + (b[2] - a[2]) * u, a[3] + (b[3] - a[3]) * u

    # -- shooting --
    def _sync(self, p: dict, t: float) -> dict | None:
        """The shooter's ammo as the server sees it, at most once per FIX_GAP."""
        if t - p["sync_at"] < FIX_GAP:
            return None
        p["sync_at"] = t
        return {"w": p["w"], "mag": list(p["mag"]), "res": list(p["res"]), "reloading": p["reload"] is not None}

    def _this_life(self, p: dict, msg: dict) -> bool:
        """Ops carry the life they were made in (e); one from before the last respawn is
        still in flight and just dropped."""
        e = as_num(msg.get("e"), 0, 1 << 30)
        return e is None or int(e) == p["life"]

    def _clock(self, p: dict, msg: dict, t: float) -> int | None:
        q = as_num(msg.get("q"), 0, (1 << 30) - 1)
        if q is None:
            return None
        q = int(q)
        if not self._lead_ok(p, q, t):
            p["bad"] += 1
            return None
        if p["qmax"] is not None and q < p["qmax"] - int(FIRE_LATE * 100):
            return None
        self._seen(p, q, t)
        return q

    def _finish_reload(self, p: dict, q: int) -> None:
        if p["reload"] is not None and q >= p["reload"][2]:
            w = p["reload"][0]
            take = min(WEAPONS[w]["mag"] - p["mag"][w], p["res"][w])
            p["mag"][w] += take
            p["res"][w] -= take
            p["reload"] = None

    def fire(self, uid: str, msg: dict, t: float) -> tuple[list | None, dict | None]:
        """A shot. Returns (the judged shot for the snapshot, or None if refused; the
        shooter's ammo to send back when it was refused for ammo, or None)."""
        p = self.players.get(uid)
        if p is None or self.phase != "round" or p["dead"] or p["gone"] or p["bot"]:
            return None, None
        if not self._bucket(p["fb"], FIRE_RATE, FIRE_BURST, t):
            return None, None
        w = as_num(msg.get("w"), -1, 99)
        ox, oy, oz = (as_num(msg.get(c), -1e6, 1e6) for c in ("ox", "oy", "oz"))
        yaw, pitch = as_num(msg.get("r"), -1e6, 1e6), as_num(msg.get("p"), -9000, 9000)
        if w is None or int(w) != w or not 0 <= w < len(WEAPONS) or None in (ox, oy, oz, yaw, pitch):
            return None, None
        w = int(w)
        if not self._this_life(p, msg):
            return None, None
        q = self._clock(p, msg, t)
        if q is None:
            return None, None
        self._ack(p, msg, q, t)
        wp = WEAPONS[w]
        self._finish_reload(p, q)
        rl = p["reload"]
        if w != p["w"] or q < p["ready"] or (rl is not None and rl[1] <= q) or p["mag"][w] <= 0:
            return None, self._sync(p, t)
        # The fire rate, on the shooter's clock: every two shots of a weapon at least its
        # interval apart (1 cs of rounding), whatever order the network delivered them in.
        gap = int(round(wp["interval"] * 100)) - 1
        last = p["last"][w]
        if any(abs(q - lq) < gap for lq in last):
            return None, None
        o = (ox / 100.0, oy / 100.0, oz / 100.0)
        lag = abs(q - p["q"]) / 100.0 if p["q"] is not None else 0.0
        # SPRINT_MAX_SPEED: a shooter can sprint inside their lag window, and the charge
        # only means anything beyond even a sprinter's drift (v1: nobody does, no change)
        if math.dist(o, (p["x"], p["y"] + EYE, p["z"])) > ORIGIN_SLACK + SPRINT_MAX_SPEED * min(lag, FIRE_LATE):
            p["bad"] += 1
            return None, None
        p["mag"][w] -= 1
        last.append(q)
        while len(last) > 8:
            last.popleft()
        p["n"] += 1
        if p["protect"] > t:
            p["protect"] = t
            self.dirty.add(uid)
        dy, dp = spread_of(p["n"], w)
        d = dir_of(yaw / 100.0 + dy, max(-89.0, min(89.0, pitch / 100.0 + dp)))
        # lag compensation: where was everyone when the shooter pulled the trigger, as
        # the shooter saw them?
        t_fire = min(t, max(t - FIRE_LATE, q / 100.0 + p["off"]))
        ip = as_num(msg.get("ip"), 0, 1e6)
        t_view = t_fire - min(p["rtt"], RTT_CAP) - min((ip or 0.0) / 1000.0, IP_CAP)
        t_view = max(t_view, t - HISTORY)
        reach = ray_map(self.m, o, d, wp["range"])
        best = None
        for vid, v in self.players.items():
            if vid == uid or v["dead"] or v["gone"] or v["team"] == p["team"] or v["spawn_t"] > t_view:
                continue
            vx, vy, vz = self.where(v, t_view)
            h = ray_player(o, d, vx, vy, vz, reach)
            if h is not None and (best is None or h[0] < best[1][0]):
                best = (vid, h)
        end = reach if best is None else best[1][0]
        shot = [p["slot"], w, round((o[0] + d[0] * end) * 100), round((o[1] + d[1] * end) * 100),
                round((o[2] + d[2] * end) * 100), -1, 0]
        if best is not None:
            vid, (_, head) = best
            v = self.players[vid]
            shot[5], shot[6] = v["slot"], 1 if head else 0
            dmg = 0 if v["protect"] > t or v["shield_totem_until"] > t else (wp["head"] if head else wp["damage"])
            v["hp"] = max(0, v["hp"] - dmg)
            self.dirty.add(vid)
            if dmg and v["hp"] <= 0:
                self._kill(uid, vid, w, head, t)
        self.shots.append(shot)
        return shot, None

    def _kill(self, uid: str, vid: str, w: int, head: bool, t: float) -> None:
        p, v = self.players[uid], self.players[vid]
        v["dead"], v["deaths"], v["respawn_at"] = True, v["deaths"] + 1, t + RESPAWN
        v["reload"] = None
        p["kills"] += 1
        # the mole's twist: a charmed killer's point lands on the OTHER side (the charmer's)
        self.scores[1 - p["team"] if p.get("charm_until", 0.0) > t else p["team"]] += 1
        self.pending.append(("kill", {"k": uid, "v": vid, "w": w, "hs": 1 if head else 0,
                                      "kills": p["kills"], "deaths": v["deaths"],
                                      "team": p["team"], "scores": list(self.scores)}))

    def reload(self, uid: str, msg: dict, t: float) -> bool:
        p = self.players.get(uid)
        if p is None or p["bot"] or self.phase != "round" or p["dead"] or p["gone"]:
            return False
        if not self._bucket(p["ob"], OP_RATE, OP_BURST, t) or not self._this_life(p, msg):
            return False
        q = self._clock(p, msg, t)
        if q is None:
            return False
        self._finish_reload(p, q)
        w = p["w"]
        if p["reload"] is not None or p["mag"][w] >= WEAPONS[w]["mag"] or p["res"][w] <= 0:
            return False
        p["reload"] = (w, q, q + int(round(WEAPONS[w]["reload"] * 100)))
        return True

    def weapon(self, uid: str, msg: dict, t: float) -> bool:
        p = self.players.get(uid)
        if p is None or p["bot"] or self.phase not in ("warmup", "round") or p["gone"]:
            return False
        if not self._bucket(p["ob"], OP_RATE, OP_BURST, t):
            return False
        w = as_num(msg.get("w"), -1, 99)
        if w is None or int(w) != w or not 0 <= w < len(WEAPONS):
            return False
        q = self._clock(p, msg, t) if msg.get("q") is not None else None
        if int(w) == p["w"]:
            return False
        p["w"] = int(w)
        p["reload"] = None
        p["ready"] = (q if q is not None else (p["qmax"] or 0)) + int(SWITCH * 100)
        self.dirty.add(uid)
        return True

    # -- bots --
    def _bot_step(self, uid: str, p: dict, dt: float, t: float) -> None:
        """Think and move for one bot this tick. Movement runs through the same step()
        physics the clients run; shooting runs through the same fire() adjudication a
        human's shot goes through."""
        b = p["bot"]
        if p["dead"]:
            return
        # the Paralyser: a frozen bot holds still and holds its fire
        if p.get("freeze_until", 0.0) > t:
            return
        m = self.m
        # -- decide a goal (at most every BOT_THINK) --
        if t >= b["think_at"]:
            b["think_at"] = t + BOT_THINK
            # the mole: a charmed bot sees its own side as the enemy and its old enemy as home
            charmed = p.get("charm_until", 0.0) > t
            # nearest living enemy with line of sight
            pick, pick_d = None, None
            for vid, v in self.players.items():
                foe = v["team"] != p["team"]
                if charmed:
                    foe = not foe
                if not foe or v["dead"] or v["gone"] or v.get("cloak_until", 0.0) > t or v.get("shield_totem_until", 0.0) > t:
                    continue
                d = math.hypot(v["x"] - p["x"], v["z"] - p["z"])
                if d > BOT_RANGE or not los(m, p["x"], p["y"] + EYE, p["z"],
                                            v["x"], v["y"] + EYE, v["z"]):
                    continue
                if pick_d is None or d < pick_d:
                    pick, pick_d = vid, d
            # the decoy: a standing ghost is just as tempting as the player it copies
            b["decoy"] = None
            for dj, dc in enumerate(self.decoys):
                if dc["until"] <= t or dc["owner"] == uid:
                    continue
                d = math.hypot(dc["x"] - p["x"], dc["z"] - p["z"])
                if d <= BOT_RANGE and (pick_d is None or d < pick_d) \
                        and los(m, p["x"], p["y"] + EYE, p["z"], dc["x"], 1.0 + EYE, dc["z"]):
                    pick, pick_d, b["decoy"] = None, d, dj
            if pick is not None:
                b["target"] = pick
                if b["seen_at"] is None:
                    b["seen_at"] = t
            else:
                b["target"] = None
                b["seen_at"] = None
            if b["decoy"] is not None and b["seen_at"] is None:
                b["seen_at"] = t
            if b["decoy"] is not None:
                dc = self.decoys[b["decoy"]]
                b["goal"] = (dc["x"], dc["z"]) if math.hypot(dc["x"] - p["x"], dc["z"] - p["z"]) > 18.0 else None
            elif b["target"] is None:
                # patrol goal: the next waypoint when the current one is reached (or stuck)
                wx, wz = WAYPOINTS[b["wpi"] % len(WAYPOINTS)]
                if math.hypot(p["x"] - wx, p["z"] - wz) < 1.5:
                    b["wpi"] += 1
                b["goal"] = (wx, wz)
            else:
                # fight from where we stand: close a little on a far target
                v = self.players[b["target"]]
                d = math.hypot(v["x"] - p["x"], v["z"] - p["z"])
                if d > 18.0:
                    b["goal"] = (v["x"], v["z"])
                else:
                    b["goal"] = None
        # -- turn and aim --
        aim_yaw, aim_pitch = b["aim_y"], b["aim_p"]
        bait = b.get("decoy")
        if bait is not None and (bait >= len(self.decoys) or self.decoys[bait]["until"] <= t):
            bait = None
        if bait is not None:
            # aim at the ghost: a still target at chest height, same turn/error rules
            dc = self.decoys[bait]
            tx, tz, ty = dc["x"], dc["z"], 1.0
            want_yaw = yaw_of(tx - p["x"], tz - p["z"])
            flat = math.hypot(tx - p["x"], tz - p["z"])
            want_pitch = math.degrees(math.atan2(ty - (p["y"] + EYE), max(0.1, flat)))
            if t - b["err_at"] >= BOT_THINK:
                b["err_at"] = t
                b["err_y"] = (mb32(int(t * 1000) & 0xFFFF) * 2 - 1) * math.degrees(BOT_AIM * self.bot_aim)
                b["err_p"] = (mb32((int(t * 1000) + 7919) & 0xFFFF) * 2 - 1) * math.degrees(BOT_AIM * self.bot_aim)
            want_yaw += b["err_y"]
            want_pitch = max(-89.0, min(89.0, want_pitch + b["err_p"]))
            dy = (want_yaw - aim_yaw + 540.0) % 360.0 - 180.0
            step = math.degrees(BOT_TURN * self.bot_turn * dt)
            aim_yaw += max(-step, min(step, dy))
            dp = want_pitch - aim_pitch
            aim_pitch += max(-step, min(step, dp))
            b["aim_y"], b["aim_p"] = aim_yaw, aim_pitch
        tgt = self.players.get(b["target"]) if b["target"] else None
        if bait is None and tgt is not None and not tgt["dead"] and not tgt["gone"]:
            # where will they be? lead a little by their speed
            lead = min(0.25, math.hypot(tgt["x"] - p["x"], tgt["z"] - p["z"]) / WEAPONS[p["w"]]["range"])
            tx = tgt["x"] + (tgt.get("vx", 0.0) or 0.0) * lead
            ty = tgt["y"] + (1.35 if (p["n"] % 3 == 0) else 1.0)
            tz = tgt["z"] + (tgt.get("vz", 0.0) or 0.0) * lead
            want_yaw = yaw_of(tx - p["x"], tz - p["z"])
            flat = math.hypot(tx - p["x"], tz - p["z"])
            want_pitch = math.degrees(math.atan2(ty - (p["y"] + EYE), max(0.1, flat)))
            # a fresh error every BOT_THINK (they are not aimbots)
            if t - b["err_at"] >= BOT_THINK:
                b["err_at"] = t
                b["err_y"] = (mb32(int(t * 1000) & 0xFFFF) * 2 - 1) * math.degrees(BOT_AIM * self.bot_aim)
                b["err_p"] = (mb32((int(t * 1000) + 7919) & 0xFFFF) * 2 - 1) * math.degrees(BOT_AIM * self.bot_aim)
            want_yaw += b["err_y"]
            want_pitch = max(-89.0, min(89.0, want_pitch + b["err_p"]))
            # swing the aim at BOT_TURN rad/s
            dy = (want_yaw - aim_yaw + 540.0) % 360.0 - 180.0
            step = math.degrees(BOT_TURN * self.bot_turn * dt)
            aim_yaw += max(-step, min(step, dy))
            dp = want_pitch - aim_pitch
            aim_pitch += max(-step, min(step, dp))
            b["aim_y"], b["aim_p"] = aim_yaw, aim_pitch
        else:
            # walk the patrol: face the way we are going
            if b["goal"]:
                want_yaw = yaw_of(b["goal"][0] - p["x"], b["goal"][1] - p["z"])
                dy = (want_yaw - aim_yaw + 540.0) % 360.0 - 180.0
                step = math.degrees(BOT_TURN * self.bot_turn * dt)
                aim_yaw += max(-step, min(step, dy))
                b["aim_y"] = aim_yaw
        # -- move (same physics as the clients) --
        wx = wz = 0.0
        if b["goal"]:
            gx, gz = b["goal"]
            dx, dz = gx - p["x"], gz - p["z"]
            ln = math.hypot(dx, dz)
            if ln > 0.3:
                wx, wz = dx / ln, dz / ln
        if p.get("st") is None:
            p["st"] = {"x": p["x"], "y": p["y"], "z": p["z"], "vx": 0.0, "vy": 0.0, "vz": 0.0, "g": True}
        st = p["st"]
        st["x"], st["y"], st["z"] = p["x"], p["y"], p["z"]
        move(m, st, wx, wz, False, dt)
        p["x"], p["y"], p["z"] = st["x"], st["y"], st["z"]
        p["vx"], p["vz"] = st["vx"], st["vz"]
        p["r"] = int(b["aim_y"] * 100) % 36000
        p["pt"] = int(b["aim_p"] * 100)
        p["at"] = t
        h = p["hist"]
        h.append((t, p["x"], p["y"], p["z"]))
        while len(h) > 2 and t - h[0][0] > HISTORY:
            h.popleft()
        # -- shoot (same adjudication as a human's fire op) --
        if (bait is not None or tgt is not None) and b["seen_at"] is not None and t - b["seen_at"] >= BOT_REACT * self.bot_react:
            d = math.hypot(self.decoys[bait]["x"] - p["x"], self.decoys[bait]["z"] - p["z"]) if bait is not None \
                else math.hypot(tgt["x"] - p["x"], tgt["z"] - p["z"])
            if d <= BOT_RANGE:
                # bots have no clock: adjudicate on ours, with no rewind
                self._bot_fire(uid, None, t)
        # bot reload (its own clock is ours)
        if p["reload"] is not None and t * 100 >= p["reload"][2]:
            w = p["reload"][0]
            take = min(WEAPONS[w]["mag"] - p["mag"][w], p["res"][w])
            p["mag"][w] += take
            p["res"][w] -= take
            p["reload"] = None
        if p["mag"][p["w"]] <= 0 and p["reload"] is None and p["res"][p["w"]] > 0:
            wp = WEAPONS[p["w"]]
            p["reload"] = (p["w"], int(t * 100), int((t + wp["reload"]) * 100))
        self.dirty.add(uid)

    def _bot_fire(self, uid: str, msg: dict, t: float) -> tuple[list | None, dict | None]:
        """A bot's shot: the same rules as fire() but with no sender clock, no lag
        compensation (the bot is on the server, so what it sees is what we see) and no
        fire-rate history on the wire - just the interval between its last two shots."""
        p = self.players.get(uid)
        if p is None or self.phase != "round" or p["dead"] or p["gone"]:
            return None, None
        b = p["bot"]
        w = p["w"]
        wp = WEAPONS[w]
        if t < (b.get("fire_at") or -1e9) + wp["interval"] * self.bot_rate - 0.01:
            return None, None
        if p["mag"][w] <= 0 or p["reload"] is not None:
            return None, None
        o = (p["x"], p["y"] + EYE, p["z"])
        p["n"] += 1
        dy, dp = spread_of(p["n"], w)
        d = dir_of(b["aim_y"] + dy, max(-89.0, min(89.0, b["aim_p"] + dp)))
        if p["protect"] > t:
            p["protect"] = t
        reach = ray_map(self.m, o, d, wp["range"])
        # the decoy: a ghost standing in the line of fire swallows the shot
        decoy_hit, decoy_d = None, reach
        for dj, dc in enumerate(self.decoys):
            if dc["until"] <= t:
                continue
            h = ray_player(o, d, dc["x"], dc["y"], dc["z"], decoy_d)
            if h is not None and h[0] < decoy_d:
                decoy_hit, decoy_d = dj, h[0]
        charmed = p.get("charm_until", 0.0) > t
        best = None
        for vid, v in self.players.items():
            if vid == uid or v["dead"] or v["gone"]:
                continue
            foe = v["team"] != p["team"]
            if charmed:
                foe = not foe
            if not foe or v.get("cloak_until", 0.0) > t or v.get("shield_totem_until", 0.0) > t:
                continue
            h = ray_player(o, d, v["x"], v["y"], v["z"], reach)
            if h is not None and (best is None or h[0] < best[1][0]):
                best = (vid, h)
        # the ghost wins only when it stands nearer than any body it masks
        if decoy_hit is not None and (best is None or best[1][0] > decoy_d):
            self.decoys[decoy_hit]["until"] = t   # one hit retires the ghost
            shot = [p["slot"], w, round((o[0] + d[0] * decoy_d) * 100), round((o[1] + d[1] * decoy_d) * 100),
                    round((o[2] + d[2] * decoy_d) * 100), -1, 0]
            b["fire_at"] = t
            p["mag"][w] -= 1
            self.shots.append(shot)
            return shot, None
        end = reach if best is None else best[1][0]
        shot = [p["slot"], w, round((o[0] + d[0] * end) * 100), round((o[1] + d[1] * end) * 100),
                round((o[2] + d[2] * end) * 100), -1, 0]
        b["fire_at"] = t
        p["mag"][w] -= 1
        if best is not None:
            vid, (_, head) = best
            v = self.players[vid]
            shot[5], shot[6] = v["slot"], 1 if head else 0
            dmg = 0 if v["protect"] > t or v["shield_totem_until"] > t else (wp["head"] if head else wp["damage"])
            v["hp"] = max(0, v["hp"] - dmg)
            self.dirty.add(vid)
            if dmg and v["hp"] <= 0:
                self._kill(uid, vid, w, head, t)
        self.shots.append(shot)
        return shot, None

    # -- the clock --
    def spawn_for(self, uid: str) -> int:
        """The spawn point of this player's side farthest from every living opponent."""
        p = self.players[uid]
        foes = [(v["x"], v["y"], v["z"]) for u, v in self.players.items()
                if u != uid and v["team"] != p["team"] and not v["dead"] and not v["gone"]]
        spawns = self._side_spawns(p["team"])
        start = p["slot"] * 5
        best, best_d = spawns[0], -1.0
        for j in range(len(spawns)):
            i = spawns[(start + j) % len(spawns)]
            s = self.m["spawns"][i]
            dmin = min((math.dist(s[:3], f) for f in foes), default=1e9)
            if dmin > best_d + 1e-9:
                best, best_d = i, dmin
        return best

    def tick(self, t: float, send: bool) -> list[tuple[str, dict]]:
        """Advance the match clock; returns the events for the lobby, in order."""
        evs: list[tuple[str, dict]] = []
        self.k += 1
        dt = 1.0 / HZ
        if self.phase == "warmup" and t >= self.go_at:
            self.phase = "round"
            self.ends_at = self.go_at + self.minutes * 60
            # the round's own drop clocks: the first totem lands TOTEM_FIRST in, the
            # first loose pack PACK_FIRST in — two independent clocks
            self.totem_at = t + TOTEM_FIRST
            self.pack_at = t + PACK_FIRST
            for p in self.players.values():
                p["at"], p["air_at"] = t, t
                p["protect"] = t + PROTECT
                p["pb"], p["fb"], p["ob"] = [float(POS_BURST), t], [float(FIRE_BURST), t], [float(OP_BURST), t]
            self.dirty = set(self.players)
            evs.append(("go", {"msLeft": self.ms_left(t)}))
        if self.phase != "round":
            return evs
        for uid, p in self.players.items():
            if p["gone"]:
                continue
            if p["away"] is not None and t - p["away"] >= GRACE:
                p["gone"] = True
                self.dirty.add(uid)
                evs.append(("gone", {"user": uid}))
                continue
            if p["bot"]:
                if not p["dead"]:
                    self._bot_step(uid, p, dt, t)
                continue
            if p["dead"] and t >= p["respawn_at"]:
                self._place(p, self.spawn_for(uid), t)
                self.dirty.add(uid)
                evs.append(("spawn", {"user": uid, "x": round(p["x"] * 100), "y": round(p["y"] * 100),
                                      "z": round(p["z"] * 100), "r": p["r"], "e": p["life"]}))
            elif p["protect"] and t >= p["protect"]:
                p["protect"] = 0.0
                self.dirty.add(uid)
        # bots respawn too
        for uid, p in self.players.items():
            if p["bot"] and p["dead"] and not p["gone"] and t >= p["respawn_at"]:
                self._place(p, self.spawn_for(uid), t)
                self.dirty.add(uid)
        # the two drop clocks, independent of each other: a fresh random totem every
        # TOTEM_GAP (the mole only when a side fields a bot — it can't turn a human) and
        # a fresh random health/ammo pack every PACK_GAP. Each replaces the oldest slot
        # of its ring, so drops keep coming whether or not anyone took the last one.
        if self.totem_at is not None and t >= self.totem_at:
            self.totem_at = t + TOTEM_GAP
            can_mole = any(v["bot"] and not v["dead"] and not v["gone"] for v in self.players.values())
            self._spawn_totem(can_mole)
        if self.pack_at is not None and t >= self.pack_at:
            self.pack_at = t + PACK_GAP
            self._spawn_pack()
        # decoys expire silently (the client's render pass drops the mesh when its window ends)
        self.decoys = [dc for dc in self.decoys if dc["until"] > t]
        for i, back in enumerate(self.items):
            if back is not None and t >= back:
                self.items[i] = None
                evs.append(("item", {"i": i, "on": 1}))
        evs.extend(self.pending)
        self.pending = []
        # same fill as drop(): a blips-past-GRACE human is replaced too
        self._fill_bots(t)
        # over at the score limit, at the time, or when a side is empty of active players
        empty = [all(p["gone"] or (p["away"] is not None and t - p["away"] >= GRACE)
                     for p in self.players.values() if p["team"] == tm) for tm in (0, 1)]
        if self.scores[0] >= self.limit or self.scores[1] >= self.limit or t >= self.ends_at or any(empty):
            self.phase = "done"
            self.results = []
            prev, place = None, 0
            for i, u in enumerate(self.order()):
                p = self.players[u]
                key = (p["team"], p["kills"], p["deaths"])
                if key != prev:
                    place, prev = i + 1, key
                self.results.append({"user": p["user"], "place": place, "team": p["team"], "bot": 1 if p["bot"] else 0,
                                     "kills": p["kills"], "deaths": p["deaths"], "left": p["gone"]})
            self.results.append({"teams": list(self.scores)})
            if self.shots:
                evs.append(("snap", self._snap(t)))
            evs.append(("done", {"results": self.results}))
            return evs
        if send and (self.dirty or self.shots):
            evs.append(("snap", self._snap(t)))
        return evs

    def _ms(self, at: float) -> int:
        """A server time on the wire: ms since the round's start (the match clock)."""
        return int(round((at - self.go_at) * 1000))

    def _snap(self, t: float) -> dict:
        """One snapshot: each changed player as [slot, x, y, z, yaw, pitch, hp, weapon,
        flags, when] (when = the server time that position is from, on the match clock;
        for bots, the tick time), the shots judged since the last one, and ts = when this
        snapshot left."""
        ents = []
        for uid in self.dirty:
            p = self.players.get(uid)
            if p is None:
                continue
            f = (1 if p["dead"] else 0) | (2 if p["protect"] > t else 0) | (4 if p["away"] is not None else 0) | \
                (8 if p["gone"] else 0) | (16 if p["bot"] else 0) | \
                (32 if p.get("shield_totem_until", 0.0) > t else 0) | (64 if p.get("charm_until", 0.0) > t else 0) | \
                (128 if p.get("freeze_until", 0.0) > t else 0) | (256 if p.get("wings_fly", 0.0) > t else 0) | \
                (512 if p.get("leap_until", 0.0) > t else 0) | (1024 if p.get("speed_until", 0.0) > t else 0)
            ents.append([p["slot"], round(p["x"] * 100), round(p["y"] * 100), round(p["z"] * 100), p["r"], p["pt"],
                         p["hp"], p["w"], f, self._ms(p["hist"][-1][0]) if p["hist"] else -1])
        decoys = [[dc["id"], round(dc["x"] * 100), round(dc["y"] * 100), round(dc["z"] * 100),
                   int((dc["until"] - t) * 1000), int(dc["yaw"] * 100)] for dc in self.decoys]
        snap = {"k": self.k, "ts": self._ms(t), "ms": self.ms_left(t), "sc": list(self.scores), "p": ents,
                "s": self.shots, "d": decoys}
        self.dirty = set()
        self.shots = []
        self.sent.append((self.k, t))
        while self.sent and t - self.sent[0][1] > RTT_WINDOW + 1.0:
            self.sent.popleft()
        return snap
