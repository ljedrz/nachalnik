#!/usr/bin/env python3
"""Proves check.py on a real chain: copies it, breaks one invariant at a time, and requires the
checker to name each break. A clean chain that stays clean and a break nobody reports are both
what this is for. The chain's own violations are printed first and are the soak's findings; a case
passes only on a violation the break added.

usage: selftest.py [--kamchatka BIN] STEM...     (the same stems check.py was given, in order)

Each case edits the copy the way a fault in the runtime would leave it. A case that finds nothing
to break in this chain (no compaction, no second segment) says so and is skipped.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))


def load(stem):
    with open(stem + ".jsonl") as f:
        log = [json.loads(line) for line in f if line.strip()]
    snapshot = None
    if os.path.exists(stem + ".json"):
        with open(stem + ".json") as f:
            snapshot = json.load(f)
    return log, snapshot


def save(stem, log, snapshot):
    with open(stem + ".jsonl", "w") as f:
        for record in log:
            f.write(json.dumps(record) + "\n")
    if snapshot is not None:
        with open(stem + ".json", "w") as f:
            json.dump(snapshot, f)


def of(log, kind):
    return [r for r in log if r["event"]["event"] == kind]


# each case: (name, the text a VIOLATION must contain, fn(chain) -> bool whether it applied)
# chain is a list of [log, snapshot], edited in place


def reused_item(chain):
    log, snap = chain[0]
    added = [r for r in of(log, "context.added") if r["seq"] <= snap["last_seq"]]
    if len(added) < 2:
        return False
    added[-1]["event"]["id"] = added[0]["event"]["id"]
    return True


def reused_call(chain):
    log, _ = chain[0]
    asked = of(log, "tool.requested")
    if len(asked) < 2:
        return False
    asked[-1]["event"]["call"] = asked[0]["event"]["call"]
    return True


def pin_compacted(chain):
    for log, _ in chain:
        for at, r in enumerate(log):
            if r["event"]["event"] != "context.compacted":
                continue
            taken = r["event"]["report"]["elided"] + r["event"]["report"]["removed"]
            if not taken:
                continue
            victim = taken[0]["id"]
            # the item was pinned all along: its move into the pass says so, and so does the
            # pin that put it there, which is added just before the pass
            for change in reversed(log[:at]):
                e = change["event"]
                if e["event"] == "context.changed" and e["id"] == victim:
                    was = e["from"]
                    e["from"] = "pinned"
                    pin = {"seq": change["seq"], "at": change["at"], "event": {
                        "event": "context.changed", "id": victim, "from": was,
                        "to": "pinned", "note": None}}
                    log.insert(log.index(change), pin)
                    return True
    return False


def item_lost(chain):
    log, snap = chain[-1]
    if snap is None or not snap["items"]:
        return False
    snap["items"].pop(0)
    return True


def run_undecided(chain):
    for log, _ in chain:
        decided = [r for r in of(log, "permission.decided") if r["event"]["grant"] == "allow"]
        started = {r["event"]["call"] for r in of(log, "tool.started")}
        for r in decided:
            if r["event"]["call"] in started:
                log.remove(r)
                return True
    return False


def machine_jumped(chain):
    log, _ = chain[0]
    changes = of(log, "state.changed")
    if len(changes) < 3:
        return False
    log.remove(changes[1])
    return True


def excluded_sent(chain):
    for log, _ in chain:
        excluded = set()
        for r in log:
            e = r["event"]
            if e["event"] == "context.changed" and e["to"] == "excluded":
                excluded.add(e["id"])
            elif e["event"] == "context.changed":
                excluded.discard(e["id"])
            elif e["event"] == "model.requested" and excluded:
                e["items"].append(min(excluded))
                return True
    return False


def content_changed(chain):
    if len(chain) < 2:
        return False
    first, last = chain[0][1], chain[-1][1]
    held = {item["id"] for item in first["items"]}
    for item in last["items"]:
        if item["id"] in held and "text" in item["content"]:
            item["content"]["text"] += " (changed)"
            return True
    return False


def resumed_elsewhere(chain):
    if len(chain) < 2:
        return False
    chain[1][0][0]["seq"] += 5
    return True


def resumed_short(chain):
    if len(chain) < 2:
        return False
    chain[1][0][0]["event"]["items"] -= 1
    return True


def reused_permission(chain):
    log, _ = chain[0]
    decided = of(log, "permission.decided")
    if len(decided) < 2:
        return False
    decided[-1]["event"]["id"] = decided[0]["event"]["id"]
    return True


CASES = [
    ("an item identifier issued twice", "an identifier already issued", reused_item),
    ("a call identifier issued twice", "already issued", reused_call),
    ("a permission identifier issued twice", "a second time", reused_permission),
    ("a pinned item compacted", "compaction took pinned item", pin_compacted),
    ("an item missing from the last snapshot", "the snapshot does not hold them", item_lost),
    ("a call run with no decision", "with no decision", run_undecided),
    ("a state transition missing", "where the machine was", machine_jumped),
    ("an excluded item sent", "which is excluded", excluded_sent),
    ("content changed across a resume", "content changed with no event", content_changed),
    ("a resume at the wrong record", "was taken at", resumed_elsewhere),
    ("a resume with the wrong count", "the snapshot it was resumed from holds", resumed_short),
]


def untagged(line):
    """A violation without the segment tag, which names the file and differs between copies."""
    return line.split("] ", 1)[-1]


def check(binary, stems):
    out = subprocess.run(
        [sys.executable, os.path.join(HERE, "check.py"), "--kamchatka", binary, *stems],
        capture_output=True, text=True,
    )
    return [line for line in out.stdout.splitlines() if line.startswith("VIOLATION")]


def main():
    args = sys.argv[1:]
    binary = "kamchatka"
    if args[:1] == ["--kamchatka"]:
        binary, args = args[1], args[2:]
    if not args:
        sys.exit(__doc__)
    stems = [s.removesuffix(".jsonl").removesuffix(".json") for s in args]
    failed = 0

    # what the chain already breaks is the soak's finding, not the checker's failure; a case only
    # counts if its own violation is among what is reported after the break
    baseline = check(binary, stems)
    print(f"--     the chain as it is: {len(baseline)} violation(s)")
    for line in baseline:
        print(f"         {line}")

    for title, expected, case in CASES:
        with tempfile.TemporaryDirectory() as scratch:
            copies = []
            for n, stem in enumerate(stems):
                copy = os.path.join(scratch, f"{n}-{os.path.basename(stem)}")
                for suffix in (".jsonl", ".json"):
                    if os.path.exists(stem + suffix):
                        shutil.copy(stem + suffix, copy + suffix)
                copies.append(copy)
            chain = [list(load(c)) for c in copies]
            if not case(chain):
                print(f"skip   {title}: nothing in this chain to break")
                continue
            for copy, (log, snapshot) in zip(copies, chain):
                save(copy, log, snapshot)
            found = check(binary, copies)
            already = {untagged(line) for line in baseline}
            hit = [line for line in found if expected in line and untagged(line) not in already]
            print(f"{'ok' if hit else 'FAIL'}   {title}" + (f": {hit[0]}" if hit else f": got {found[:3]}"))
            failed += not hit
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
