"""Arena City skatepark: the weekly skate board, local proxy routes (stdlib only).

GET  /api/arena/skate         -> the Arena's /v1/skate (this UTC week's top 20 and your place)
POST /api/arena/skate/combo   -> /v1/skate {total, tricks}

Privacy: the only thing that leaves this machine is a combo's score: its total and how
many tricks it chained, two whole numbers checked here before they go. Nothing about
what you did, where, or from which session.
"""
import arena

MAX_TOTAL = 200000
MAX_TRICKS = 10


def _whole(v, lo, hi):
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return None
    if isinstance(v, float) and (v != v or v != int(v)):
        return None
    v = int(v)
    return v if lo <= v <= hi else None


def get_board(arg):
    token, base = arena._authed()
    if not token:
        return 400, {"error": "not paired"}
    return arena._request("GET", base + "/v1/skate", token=token)


def post_combo(body):
    body = body if isinstance(body, dict) else {}
    total = _whole(body.get("total"), 1, MAX_TOTAL)
    tricks = _whole(body.get("tricks"), 1, MAX_TRICKS)
    if total is None or tricks is None:
        return 400, {"error": "a combo is a total (1-200000) and a trick count (1-10)"}
    token, base = arena._authed()
    if not token:
        return 400, {"error": "not paired"}
    return arena._request("POST", base + "/v1/skate", token=token, body={"total": total, "tricks": tricks})


GET = {"/api/arena/skate": get_board}
POST = {"/api/arena/skate/combo": post_combo}
