#!/usr/bin/env python3
"""The mutants `cargo mutants` missed, as task files for kill.sh: one group per file, a message a line.

    mutants_tasks.py MUTANTS_OUT OUTDIR [PER_TASK] [--retry-unsettled]

Writes OUTDIR/<slug>.txt (the messages) and OUTDIR/<slug>/ (the diffs the session gets as
`.mutants/`), for every file with a missed mutant, split so that no task holds more than PER_TASK
(default 8). A missed mutant is a lead, not a gap: some change nothing a caller can observe, and
the session is asked to say so rather than to write a test that pins an implementation detail.

A mutant `verdicts.jsonl` already settles is left out - see `ledger.py` - and so is one an
earlier sweep tried and could not settle, unless `--retry-unsettled` asks for those again.
"""

import collections
import json
import os
import re
import shutil
import sys

import ledger

retry = "--retry-unsettled" in sys.argv
args = [arg for arg in sys.argv[1:] if arg != "--retry-unsettled"]
out, dest = args[0], args[1]
per = int(args[2]) if len(args) > 2 else 8
outcomes = json.load(open(os.path.join(out, "outcomes.json")))["outcomes"]
settled = ledger.load()
skipped = collections.Counter()
missed = collections.defaultdict(list)
for o in outcomes:
    if o["summary"] != "MissedMutant":
        continue
    m = o["scenario"]["Mutant"]
    k = ledger.key(m, open(os.path.join(out, o["diff_path"])).read())
    entry = k and settled.get(ledger.ident(k))
    if entry and not (retry and entry["verdict"] == "unsettled"):
        skipped[entry["verdict"]] += 1
        continue
    missed[m["file"]].append((m["name"], o["diff_path"]))
if skipped:
    print("left out, as verdicts.jsonl settles them:", dict(skipped), file=sys.stderr)

os.makedirs(dest, exist_ok=True)
for file, mutants in sorted(missed.items()):
    for n, start in enumerate(range(0, len(mutants), per)):
        chunk = mutants[start : start + per]
        slug = file.replace("/", "_").removesuffix(".rs") + (f"-{n + 1}" if len(mutants) > per else "")
        diffs = os.path.join(dest, slug)
        os.makedirs(diffs, exist_ok=True)
        listed = []
        for name, diff in chunk:
            # cargo-mutants writes a description where the `+++` path goes, which `git apply` cannot
            # read; the file it changed is the path the session will be asked to apply it to
            text = open(os.path.join(out, diff)).read()
            text = re.sub(r"(?m)^\+\+\+ .*$", f"+++ b/{file}", text, count=1)
            text = re.sub(r"(?m)^--- .*$", f"--- a/{file}", text, count=1)
            open(os.path.join(diffs, os.path.basename(diff)), "w").write(text)
            listed.append(f"`.mutants/{os.path.basename(diff)}` - {name}")
        first = (
            f"`cargo mutants` changed the code in {file} in each of the ways below, and every "
            "test in the workspace still passed. Each diff is in `.mutants/` in the working "
            "directory. For each one, decide whether a caller could observe the change. If it "
            "could, write the test that catches it - where the crate's other tests of that code "
            "live, extending one where that is natural - then prove it: the test passes as the "
            "code stands, `git apply .mutants/<diff>` makes it fail, `git apply -R .mutants/<diff>` "
            "takes the mutation back out. If nothing a caller does could tell the difference, "
            "say why and write no test. Report each one on a line of its own in your reply "
            "text, as `KILLED: <diff> by <test name>` or `EQUIVALENT: <diff>: <why>`, as soon as "
            "you know it. The mutants: " + "; ".join(listed) + "."
        )
        with open(os.path.join(dest, slug + ".txt"), "w") as f:
            f.write(first + "\n")
            f.write(
                "Carry on with any mutant you have not settled, and check that `git status` "
                "shows no change outside test code and nothing left of a mutation.\n"
            )
            f.write(
                "Now give the final list: every mutant with KILLED or EQUIVALENT, the test that "
                "kills each killed one, and the command that shows it failing under the mutation.\n"
            )
        print(slug, len(chunk))
