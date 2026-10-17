#!/usr/bin/env python3
"""screen.py OUTDIR... [--flaky TEST]...

Finds the caught mutants a run should not trust: the ones caught only by a test that was failing
for a reason of its own, such as a worktree an earlier mutant left a directory shut in.

Without --flaky, prints on stderr the tests that failed in the most catches of each OUTDIR, with
how many source files those mutants were in. A test of the mutated code fails for mutants of one
file or two; one failing for mutants all over the crate is the suspect. With --flaky, prints
`OUTDIR<TAB>NAME` for every caught mutant whose log fails nothing but the named tests. Such a catch
was made by the worktree, not by a test of the change: take the name out of the shard's
caught.txt (keep a copy) so the next --iterate run tests it again.
"""
import collections
import json
import os
import re
import sys

args = sys.argv[1:]
flaky = set()
while "--flaky" in args:
    i = args.index("--flaky")
    flaky.add(args[i + 1])
    del args[i:i + 2]
if not args:
    sys.exit(__doc__)
FAILED = re.compile(r"^test (\S+) \.\.\. FAILED$", re.M)


def catches(out):
    """(mutant, file, failed tests) for every caught mutant in OUTDIR."""
    path = os.path.join(out, "mutants.out", "outcomes.json")
    if not os.path.exists(path):
        return  # a shard that drew no mutants writes none
    for o in json.load(open(path))["outcomes"]:
        if o["summary"] != "CaughtMutant":
            continue
        mutant = o["scenario"]["Mutant"]
        log = os.path.join(out, "mutants.out", o["log_path"])
        failed = {t.split("::")[-1] for t in FAILED.findall(open(log, errors="replace").read())}
        yield mutant["name"], mutant["file"], failed


for out in args:
    found = list(catches(out))
    if not flaky:
        counts, files = collections.Counter(), collections.defaultdict(set)
        for _, file, failed in found:
            for t in failed:
                counts[t] += 1
                files[t].add(file)
        print(f"{out}: {len(found)} caught; the tests that failed in the most of them", file=sys.stderr)
        for t, n in counts.most_common(5):
            print(f"  {n:5} catches, {len(files[t]):3} files  {t}", file=sys.stderr)
        continue
    suspect = [name for name, _, failed in found if failed and failed <= flaky]
    for name in suspect:
        print(out, name, sep="\t")
    print(f"{out}: {len(suspect)} suspect of {len(found)} caught", file=sys.stderr)
