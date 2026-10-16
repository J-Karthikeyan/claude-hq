#!/usr/bin/env python3
"""Ground-truth vectors for the notch's creature logic, taken from HQ itself:
species / stage / shiny from dashboard.py's creature_for() + shiny_for_species(), and the
drawn form (dex) from ui/app/06-trainer-packs.js's pokeDexFor() run under node.

    python3 notch/tools/parity_vectors.py > vectors.json
    swift run -c release --package-path notch hqnotch-selftest vectors.json
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, REPO)

import dashboard  # noqa: E402

SALT = "0123456789abcdef"
PROMPTS = [0, 2, 3, 11, 12, 29, 30, 69, 70, 400]

JS = r"""
const fs=require("fs"),vm=require("vm");
const ctx={console,localStorage:{getItem:()=>null,setItem(){}},window:{},
  document:{addEventListener(){},querySelector:()=>null,getElementById:()=>null}};
vm.createContext(ctx);
for(const f of ["ui/app/01-creatures.js","ui/app/06-trainer-packs.js"])
  vm.runInContext(fs.readFileSync(f,"utf8"),ctx,{filename:f});
vm.runInContext('cfg=function(){return {creaturePack:"pokemon"}};speciesHighWater=function(){return 0};',ctx);
const inp=JSON.parse(fs.readFileSync(0,"utf8"));
ctx.__in=inp;
const out=vm.runInContext('__in.map(function(c){return {dex:pokeDexFor({species:c.species,stage:c.stage,_sid:c.sid}),hash:hashStr(c.sid)};})',ctx);
process.stdout.write(JSON.stringify(out));
"""


def session_ids():
    ids = ["", "a2207bcc-c7ba-4031-abe0-84cf99ea9153", "héllo-wörld-🦊"]
    ids += ["%08x-0000-4000-8000-%012x" % (i * 2654435761 % (1 << 32), i) for i in range(160)]
    return ids


def main():
    rows = []
    for i, sid in enumerate(session_ids()):
        pc = PROMPTS[i % len(PROMPTS)]
        cr = dashboard.creature_for(sid, pc)
        rows.append({"sid": sid, "prompts": pc, "species": cr["species"], "stage": cr["stage"],
                     "shiny": dashboard.shiny_for_species(cr["species"], SALT)})
    js = subprocess.run(["node", "-e", JS], input=json.dumps(rows), capture_output=True, text=True,
                        cwd=REPO, check=True)
    for r, j in zip(rows, json.loads(js.stdout)):
        r["dex"] = j["dex"]
        r["hashStr"] = j["hash"]
    json.dump({"salt": SALT, "rows": rows}, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
