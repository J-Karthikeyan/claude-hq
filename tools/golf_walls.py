#!/usr/bin/env python3
"""Generate Mini Golf collision data from the vendored Kenney GLB tiles (stdlib only).

Reads games/golf/<piece>.glb, keeps the vertical triangles that span the ball's
height band, projects them onto the ground plane (x, z) as integer segments
(1 tile = 10000 units, origin = tile centre) and writes them into the "pieces"
part of games/golf/courses.json, then mirrors the file byte-for-byte to
backend/data/golf_courses.json (the Arena embeds it at build time).

    python3 tools/golf_walls.py            # regenerate + mirror
    python3 tools/golf_walls.py --check    # exit 1 if either file is stale
    python3 tools/golf_walls.py --show straight hole-round

Faces on the tile border are dropped when they lie wholly in the wall band (|other
coordinate| >= 4000): those are the end caps where one tile's wall meets the next
one's, and keeping them would bump a ball rolling along a wall at every seam. Border
faces that reach into the lane (the nose of a split divider) are kept. Floor holes ("voids") and the cup /
tee positions are not in the meshes' walls, so they come from the EXTRA table below,
measured from the floor triangles (see --floor).
"""
import json
import math
import os
import struct
import sys

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GOLF = os.path.join(HERE, "games", "golf")
COURSES = os.path.join(GOLF, "courses.json")
MIRROR = os.path.join(HERE, "backend", "data", "golf_courses.json")

FLOOR = 0.063          # floor top (tile units)
BAND_LO = FLOOR + 0.005  # a wall must reach down to here ...
BAND_HI = FLOOR + 0.06   # ... and up to here to touch a rolling ball
UNIT = 10000

# The course pieces vendored in games/golf/. Cups sit at the tile centre of every
# hole-* piece.
PIECES = ["start", "straight", "corner", "end", "side", "round-corner-a", "split", "split-t",
          "split-start", "walls-to-open", "gap", "obstacle-block", "obstacle-diamond",
          "obstacle-triangle", "narrow-block", "narrow-round", "castle", "windmill", "tunnel-wide",
          "tunnel-narrow", "hole-round", "hole-square", "hole-open", "open", "inner-corner", "hill-round", "bump",
          "tunnel-double"]
# start: the back half (z > 0) is a ramp down to the grass, so it is a void and the
# tee sits on the flat front half. gap: a square hole, about 0.4 x 0.4 tiles.
EXTRA = {
    "start": {"tee": [0, -2500], "voids": [[-5000, 0, 5000, 5000]]},
    "hole-round": {"cup": [0, 0]}, "hole-square": {"cup": [0, 0]}, "hole-open": {"cup": [0, 0]},
    "gap": {"voids": [[-2000, -2000, 2000, 2000]]},
    # windmill: while a blade sweeps past the bottom it closes the far doorway (the blades
    # hang in front of the z = -4000 face, hub at z = -0.446)
    "windmill": {"blades": [-1625, -4458, 1625, -4458]},
    # hill-round: a ridge across the lane (crest at z = 0, 0.084 above the floor); gravity
    # pulls a slow ball back down either side
    "hill-round": {"slopes": [[-4000, -3000, 4000, 0, 0, -400], [-4000, 0, 4000, 3000, 0, 400]]},
    # bump: a low round mound in the middle of an open tile; it nudges the ball outward
    "bump": {"slopes": [[-2500, -2500, 0, 0, -110, -110], [0, -2500, 2500, 0, 110, -110],
                        [-2500, 0, 0, 2500, -110, 110], [0, 0, 2500, 2500, 110, 110]]},
}


def glb_triangles(path, skip=("blades",)):
    b = open(path, "rb").read()
    if b[:4] != b"glTF":
        raise ValueError("not a GLB: " + path)
    n = struct.unpack("<I", b[12:16])[0]
    j = json.loads(b[20:20 + n])
    off = 20 + n
    bl = struct.unpack("<I", b[off:off + 4])[0]
    bin_ = b[off + 8:off + 8 + bl]
    tris = []
    for nd in j["nodes"]:
        if "mesh" not in nd or nd.get("name") in skip:
            continue
        if any(k in nd for k in ("rotation", "scale", "matrix")):
            raise ValueError("unsupported node transform in " + path)
        t = nd.get("translation", [0, 0, 0])
        for pr in j["meshes"][nd["mesh"]]["primitives"]:
            a = j["accessors"][pr["attributes"]["POSITION"]]
            bv = j["bufferViews"][a["bufferView"]]
            st = bv.get("byteStride", 12)
            o = bv.get("byteOffset", 0) + a.get("byteOffset", 0)
            verts = [tuple(c + d for c, d in zip(struct.unpack("<3f", bin_[o + i * st:o + i * st + 12]), t))
                     for i in range(a["count"])]
            ia = j["accessors"][pr["indices"]]
            ib = j["bufferViews"][ia["bufferView"]]
            io = ib.get("byteOffset", 0) + ia.get("byteOffset", 0)
            fmt = {5121: "B", 5123: "H", 5125: "I"}[ia["componentType"]]
            sz = struct.calcsize(fmt)
            idx = [struct.unpack("<" + fmt, bin_[io + k * sz:io + k * sz + sz])[0] for k in range(ia["count"])]
            tris += [(verts[idx[k]], verts[idx[k + 1]], verts[idx[k + 2]]) for k in range(0, len(idx), 3)]
    return tris


def _snap(v):
    v = round(v * UNIT)
    if abs(v) >= 4990:
        v = 5000 if v > 0 else -5000
    return v


def _merge(segs):
    """Join colinear segments that touch or overlap (exact integer tests)."""
    segs = [tuple(s) for s in segs]
    changed = True
    while changed:
        changed = False
        for i in range(len(segs)):
            for k in range(i + 1, len(segs)):
                a, b = segs[i], segs[k]
                dx, dz = a[2] - a[0], a[3] - a[1]
                if dx * (b[1] - a[1]) - dz * (b[0] - a[0]) or dx * (b[3] - a[1]) - dz * (b[2] - a[0]):
                    continue                      # not on the same line
                L = dx * dx + dz * dz
                pts = [(0, a[0], a[1]), (L, a[2], a[3]),
                       (dx * (b[0] - a[0]) + dz * (b[1] - a[1]), b[0], b[1]),
                       (dx * (b[2] - a[0]) + dz * (b[3] - a[1]), b[2], b[3])]
                if max(pts[2][0], pts[3][0]) < 0 or min(pts[2][0], pts[3][0]) > L:
                    continue                      # same line, but a gap between them
                pts.sort()
                segs[i] = (pts[0][1], pts[0][2], pts[-1][1], pts[-1][2])
                del segs[k]
                changed = True
                break
            if changed:
                break
    return segs


def _seam_cap(a, b):
    for i in (0, 1):
        if abs(a[i]) == 5000 and a[i] == b[i]:
            o = 1 - i
            lo, hi = min(a[o], b[o]), max(a[o], b[o])
            if hi <= -4000 or lo >= 4000:
                return True
    return False


def piece_segments(path):
    out = set()
    for t in glb_triangles(path):
        ys = [v[1] for v in t]
        if min(ys) > BAND_LO or max(ys) < BAND_HI:
            continue
        ux, uy, uz = (t[1][i] - t[0][i] for i in range(3))
        vx, vy, vz = (t[2][i] - t[0][i] for i in range(3))
        nx, ny, nz = uy * vz - uz * vy, uz * vx - ux * vz, ux * vy - uy * vx
        ln = math.sqrt(nx * nx + ny * ny + nz * nz) or 1
        if abs(ny / ln) > 0.05:
            continue
        pts = sorted({(_snap(v[0]), _snap(v[2])) for v in t})
        a, b = pts[0], pts[-1]
        if a == b or _seam_cap(a, b):
            continue
        out.add((a[0], a[1], b[0], b[1]) if a <= b else (b[0], b[1], a[0], a[1]))
    segs = sorted(_merge(sorted(out)))
    for s in segs:
        if (s[2] - s[0]) ** 2 + (s[3] - s[1]) ** 2 > 15000 ** 2:
            raise ValueError("segment longer than 1.5 tiles in " + path)
    return [list(s) for s in segs]


def build_pieces():
    pieces = {}
    for name in PIECES:
        p = {"segs": piece_segments(os.path.join(GOLF, name + ".glb"))}
        p.update(EXTRA.get(name, {}))
        pieces[name] = p
    return pieces


def render(data):
    """Stable, compact JSON: one piece / one hole per line."""
    lines = ['{"v": %d, "tile": %d,' % (data["v"], data["tile"]), '"pieces": {']
    names = list(data["pieces"])
    for i, k in enumerate(names):
        lines.append(json.dumps(k) + ": " + json.dumps(data["pieces"][k], separators=(",", ":"))
                     + ("," if i < len(names) - 1 else ""))
    lines.append("},")
    lines.append('"courses": [')
    for ci, c in enumerate(data["courses"]):
        head = '{"id": %s, "name": %s, ' % (json.dumps(c["id"]), json.dumps(c["name"]))
        if "theme" in c:
            head += '"theme": %s,\n' % json.dumps(c["theme"], separators=(", ", ": "))
        lines.append(head + '"holes": [')
        for hi, h in enumerate(c["holes"]):
            lines.append("  " + json.dumps(h, separators=(", ", ": "))
                         + ("," if hi < len(c["holes"]) - 1 else ""))
        lines.append("]}" + ("," if ci < len(data["courses"]) - 1 else ""))
    lines.append("]}")
    return "\n".join(lines) + "\n"


def floor_map(path, n=21):
    """ASCII map of a piece's floor at the ball's height (# = floor), for checking voids."""
    fl = [t for t in glb_triangles(path) if all(abs(v[1] - FLOOR) < 0.002 for v in t)]

    def inside(x, z, t):
        (ax, az), (bx, bz), (cx, cz) = [(v[0], v[2]) for v in t]
        d1 = (x - bx) * (az - bz) - (ax - bx) * (z - bz)
        d2 = (x - cx) * (bz - cz) - (bx - cx) * (z - cz)
        d3 = (x - ax) * (cz - az) - (cx - ax) * (z - az)
        return not ((d1 < 0 or d2 < 0 or d3 < 0) and (d1 > 0 or d2 > 0 or d3 > 0))
    step = 1.0 / (n - 1)
    return "\n".join("".join("#" if any(inside(-0.5 + i * step, -0.5 + k * step, t) for t in fl) else "."
                             for i in range(n)) for k in range(n))


def main(argv):
    if argv[:1] == ["--floor"]:
        for name in argv[1:]:
            print(name)
            print(floor_map(os.path.join(GOLF, name + ".glb")))
        return 0
    if argv[:1] == ["--show"]:
        for name in argv[1:]:
            print(name, piece_segments(os.path.join(GOLF, name + ".glb")))
        return 0
    with open(COURSES, encoding="utf-8") as f:
        data = json.load(f)
    data["pieces"] = build_pieces()
    text = render(data)
    if argv[:1] == ["--check"]:
        stale = [p for p in (COURSES, MIRROR) if not os.path.isfile(p) or open(p, encoding="utf-8").read() != text]
        for p in stale:
            print("stale:", os.path.relpath(p, HERE))
        return 1 if stale else 0
    for p in (COURSES, MIRROR):
        with open(p, "w", encoding="utf-8") as f:
            f.write(text)
    print("wrote", os.path.relpath(COURSES, HERE), "and", os.path.relpath(MIRROR, HERE))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
