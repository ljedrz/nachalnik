#!/usr/bin/env python3
"""tainted.py OUTDIR [--strip]

Lists the mutants in OUTDIR/mutants.out whose outcome cannot be trusted: the ones whose log shows
a failed fork (EAGAIN, os error 11) and the ones that never got an outcome at all. A mutant whose
build could not fork is recorded as Unviable, and `--iterate` skips an Unviable one forever, so a
run that hit the process limit has to be cleaned before it is iterated on.

--strip removes the tainted names from caught.txt, unviable.txt and previously_caught.txt, the
three files `--iterate` skips by, keeping a .bak of each, so the next `--iterate` run over OUTDIR
tests them again. The listing reads the logs, not those files, so it does not shrink after a strip.
Check the "Found N mutants to test" line of the next run against the count printed here plus the
missed and timeout counts.
"""
import json
import os
import shutil
import sys

out = sys.argv[1]
strip = "--strip" in sys.argv[2:]
d = os.path.join(out, "mutants.out")
outcomes = json.load(open(os.path.join(d, "outcomes.json")))["outcomes"]
every = [m["name"] for m in json.load(open(os.path.join(d, "mutants.json")))]

seen, tainted = set(), []
for o in outcomes:
    if o["scenario"] == "Baseline":
        continue
    name = o["scenario"]["Mutant"]["name"]
    seen.add(name)
    log = open(os.path.join(d, o["log_path"]), errors="replace").read()
    if "os error 11" in log or "Resource temporarily unavailable" in log:
        tainted.append((o["summary"], name))
untested = [n for n in every if n not in seen]

for summary, name in tainted:
    print(f"tainted  {summary:13} {name}")
for name in untested:
    print(f"untested {'':13} {name}")
print(f"{len(tainted)} tainted, {len(untested)} untested, of {len(every)}", file=sys.stderr)

if strip:
    drop = {n for _, n in tainted}
    for f in ("caught.txt", "unviable.txt", "previously_caught.txt"):
        p = os.path.join(d, f)
        if not os.path.exists(p):
            continue
        shutil.copy(p, p + ".bak")
        lines = open(p).read().splitlines()
        kept = [l for l in lines if l not in drop]
        open(p, "w").write("".join(l + "\n" for l in kept))
        print(f"{f}: removed {len(lines) - len(kept)}", file=sys.stderr)
