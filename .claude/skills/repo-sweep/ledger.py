"""The mutants a sweep has already settled without a test, so that the next one does not ask again.

    ledger.py add MUTANTS_OUT VERDICT WHY NAME...    records mutants by their cargo-mutants names

    verdicts.jsonl, beside this file: one JSON object a line -
    {"key": {...}, "verdict": "...", "why": "...", "name": "...", "commit": "...", "date": "..."}

`verdict` is one of
- `equivalent`: no caller can tell the mutant from the code
- `not-pinned`: observable only at the exact edge of a private threshold, or in a tuning value
- `no-caller`: the code is dead; a POSTPONED entry rather than a test
- `unsettled`: sessions tried and found no test; `mutants_tasks.py --retry-unsettled` asks again

A mutant is keyed by what survives the code around it moving: the file, the enclosing function,
the kind of mutation and its replacement, and the text of the line it starts on with where in that
line it starts. Not by line number, which every edit above it changes - and not by anything that
outlives an edit to the line itself: a verdict about code that has since changed is a verdict
about other code, so the key stops matching and the mutant is a task again.
"""

import json
import os
import re

LEDGER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "verdicts.jsonl")
VERDICTS = ("equivalent", "not-pinned", "no-caller", "unsettled")


def line_at(diff, number):
    """The text line `number` had before the mutation, read off the diff's hunks, or None."""
    old = None
    for line in diff.splitlines():
        header = re.match(r"@@ -(\d+)(?:,\d+)? \+\d+(?:,\d+)? @@", line)
        if header:
            old = int(header.group(1))
            continue
        if old is None or line.startswith(("---", "+++")):
            continue
        if line.startswith("+"):
            continue
        if line.startswith((" ", "-")) or line == "":
            if old == number:
                return line[1:]
            old += 1
    return None


def key(mutant, diff):
    """The key of one mutant, as `outcomes.json` describes it, given the diff cargo-mutants wrote."""
    start = mutant["span"]["start"]
    line = line_at(diff, start["line"])
    if line is None:
        return None
    indent = len(line) - len(line.lstrip())
    return {
        "file": mutant["file"],
        "function": (mutant.get("function") or {}).get("function_name"),
        "genre": mutant["genre"],
        "replacement": mutant["replacement"],
        "line": line.strip(),
        "at": start["column"] - 1 - indent,
    }


def ident(k):
    return json.dumps(k, sort_keys=True, ensure_ascii=False)


def load(path=LEDGER):
    """Every entry, by key; a later line for the same key replaces an earlier one."""
    entries = {}
    if os.path.exists(path):
        for line in open(path, encoding="utf-8"):
            if line.strip():
                entry = json.loads(line)
                entries[ident(entry["key"])] = entry
    return entries


def append(entries, path=LEDGER):
    with open(path, "a", encoding="utf-8") as out:
        for entry in entries:
            assert entry["verdict"] in VERDICTS, entry
            out.write(json.dumps(entry, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    import datetime
    import subprocess
    import sys

    if len(sys.argv) < 6 or sys.argv[1] != "add" or sys.argv[3] not in VERDICTS:
        sys.exit(__doc__)
    out, verdict, why, names = sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5:]
    outcomes = {
        o["scenario"]["Mutant"]["name"]: o
        for o in json.load(open(os.path.join(out, "outcomes.json")))["outcomes"]
        if isinstance(o["scenario"], dict) and "Mutant" in o["scenario"]
    }
    commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True).stdout.strip()
    entries = []
    for name in names:
        o = outcomes.get(name) or sys.exit(f"no mutant named {name!r} in {out}")
        k = key(o["scenario"]["Mutant"], open(os.path.join(out, o["diff_path"])).read())
        k or sys.exit(f"{name}: its line is not in its diff")
        entries.append({"key": k, "verdict": verdict, "why": why, "name": name, "commit": commit,
                        "date": datetime.date.today().isoformat()})
    append(entries)
    print(f"recorded {len(entries)} as {verdict}")
