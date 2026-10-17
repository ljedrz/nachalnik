#!/usr/bin/env python3
"""tainted.py OUTDIR [--strip]

Lists the mutants in OUTDIR/mutants.out whose outcome cannot be trusted: the ones whose log shows
a failed fork (EAGAIN, os error 11) and the ones that never got an outcome at all. A mutant whose
build could not fork is recorded as Unviable, and `--iterate` skips an Unviable one forever, so a
run that hit the process limit has to be cleaned before it is iterated on.

EAGAIN is also what a read from a socket with a read timeout gives, and the two print the same:
`Os { code: 11, kind: WouldBlock, ... }` from a failed `Command::spawn` and from a served
session's `stream.read` alike. So a line with EAGAIN is a fork's when it or the few lines above it
say so (`fork`, `spawn`, `could not execute`), and a socket's only when it is a panic whose site,
read from this repository, is a read; anything else is taken for a fork. A socket's is listed as
`wouldblock` and left alone.

--strip removes the tainted names from caught.txt, unviable.txt and previously_caught.txt, the
three files `--iterate` skips by, keeping a .bak of each, so the next `--iterate` run over OUTDIR
tests them again. The listing reads the logs, not those files, so it does not shrink after a strip.
Check the "Found N mutants to test" line of the next run against the count printed here plus the
missed and timeout counts.
"""
import json
import os
import re
import shutil
import sys

out = sys.argv[1]
strip = "--strip" in sys.argv[2:]
d = os.path.join(out, "mutants.out")
# a shard of a sharded run that drew no mutants writes no outcomes at all
if not os.path.exists(os.path.join(d, "outcomes.json")) and not json.load(open(os.path.join(d, "mutants.json"))):
    print("0 tainted, 0 untested, of 0", file=sys.stderr)
    sys.exit(0)
outcomes = json.load(open(os.path.join(d, "outcomes.json")))["outcomes"]
every = [m["name"] for m in json.load(open(os.path.join(d, "mutants.json")))]

REPO = os.path.realpath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
# EAGAIN exactly: "os error 111" is a refused connection, which tests make on purpose
EAGAIN = re.compile(r"os error 11\b|code: 11\b|Resource temporarily unavailable")
FORK = re.compile(r"\bfork\b|spawn|could not execute|never executed")
PANIC = re.compile(r"panicked at (\S+?):(\d+):\d+:")
READ = re.compile(r"\.read(_to_end|_to_string|_line|_exact)?\(|\.recv\(|\.accept\(|\.lines\(\)")


def socket_read(lines, i):
    """Whether the EAGAIN on line i is a read timing out rather than a fork failing."""
    above = lines[max(0, i - 6):i + 1]
    if any(FORK.search(l) for l in above):
        return False
    for l in reversed(above):
        m = PANIC.search(l)
        if m:
            try:
                source = open(os.path.join(REPO, m.group(1)), errors="replace").read().splitlines()
            except OSError:
                return False
            line = int(m.group(2))
            # the call an `.expect` takes is often on the line above it, as rustfmt breaks it
            return any(READ.search(s) for s in source[max(0, line - 4):line])
    return False


seen, tainted, blocked = set(), [], []
for o in outcomes:
    if o["scenario"] == "Baseline":
        continue
    name = o["scenario"]["Mutant"]["name"]
    seen.add(name)
    lines = open(os.path.join(d, o["log_path"]), errors="replace").read().splitlines()
    hits = [i for i, l in enumerate(lines) if EAGAIN.search(l)]
    if any(not socket_read(lines, i) for i in hits):
        tainted.append((o["summary"], name))
    elif hits:
        blocked.append((o["summary"], name))
untested = [n for n in every if n not in seen]

for summary, name in tainted:
    print(f"tainted  {summary:13} {name}")
for summary, name in blocked:
    print(f"wouldblock {summary:11} {name}")
for name in untested:
    print(f"untested {'':13} {name}")
# last, as `shards.sh` shows only the last line, and a piped stdout is otherwise written at exit
sys.stdout.flush()
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
