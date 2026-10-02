#!/usr/bin/env python3
"""merge.py DEST SRC...
merge.py --unify SRC...

Merges the outputs of the shards of one run (shards.sh's OUT-1 .. OUT-N) into DEST/mutants.out:
the outcomes, the diffs they name, and the union of each list `--iterate` skips by. DEST is then
what mutants_tasks.py reads, and the SEED of a later sharded `--iterate` run: every shard of that
run has to skip the same mutants, or the shards divide different lists and some mutants are
tested twice and others never.

A mutant in more than one SRC keeps its outcome from the last: list a shard's copies from before
each retry first and its final output last, since `--iterate` rewrites `outcomes.json` with only
the mutants it tested again.

--unify writes the union of the skip lists back into every SRC, for a sharded `--iterate` run
over the shards themselves (a retry of tainted outcomes): `tainted.py --strip` first, then this.
"""
import json
import os
import shutil
import sys

SKIPS = ("caught.txt", "unviable.txt", "previously_caught.txt")

if sys.argv[1] == "--unify":
    srcs = sys.argv[2:]
    union = {}
    for src in srcs:
        for f in SKIPS:
            p = os.path.join(src, "mutants.out", f)
            if os.path.exists(p):
                union.setdefault(f, {}).update(dict.fromkeys(open(p).read().splitlines()))
    for src in srcs:
        for f, lines in union.items():
            open(os.path.join(src, "mutants.out", f), "w").write("".join(l + "\n" for l in lines))
    print(f"unified {len(srcs)}: " + ", ".join(f"{f} {len(l)}" for f, l in union.items()), file=sys.stderr)
    sys.exit(0)

dest, srcs = sys.argv[1], sys.argv[2:]
d = os.path.join(dest, "mutants.out")
os.makedirs(d, exist_ok=True)
outcomes, every, seen = {}, [], set()
lists = {}
for src in srcs:
    s = os.path.join(src, "mutants.out")
    if not os.path.exists(os.path.join(s, "outcomes.json")):
        continue  # a shard that drew no mutants
    for o in json.load(open(os.path.join(s, "outcomes.json")))["outcomes"]:
        if o["scenario"] == "Baseline":
            continue
        outcomes[o["scenario"]["Mutant"]["name"]] = o
        for key in ("diff_path", "log_path"):
            if o.get(key):
                os.makedirs(os.path.join(d, os.path.dirname(o[key])), exist_ok=True)
                shutil.copy(os.path.join(s, o[key]), os.path.join(d, o[key]))
    for m in json.load(open(os.path.join(s, "mutants.json"))):
        if m["name"] not in seen:
            seen.add(m["name"])
            every.append(m)
    for f in ("caught.txt", "unviable.txt", "previously_caught.txt", "missed.txt", "timeout.txt"):
        p = os.path.join(s, f)
        if os.path.exists(p):
            lists.setdefault(f, []).extend(open(p).read().splitlines())
outcomes = list(outcomes.values())
json.dump({"outcomes": outcomes}, open(os.path.join(d, "outcomes.json"), "w"))
json.dump(every, open(os.path.join(d, "mutants.json"), "w"))
for f, lines in lists.items():
    open(os.path.join(d, f), "w").write("".join(l + "\n" for l in dict.fromkeys(lines)))
counts = {}
for o in outcomes:
    counts[o["summary"]] = counts.get(o["summary"], 0) + 1
print(f"{dest}: {len(outcomes)} outcomes {counts}", file=sys.stderr)
