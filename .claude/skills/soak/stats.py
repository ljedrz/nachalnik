#!/usr/bin/env python3
"""What a soak's segments did, and where the context is heading.

usage: stats.py STEM...     (the same stems check.py takes, in order)

Per segment: requests, calls and their errors, refusals, provider failures, compactions. Then the
floor - what each compaction left behind - which is the number a long session lives or dies by:
a pass takes neither the turn in progress nor what is pinned, so those accumulate, and a floor that
climbs towards the limit is a session that will stop being able to send. Last, what the final snapshot
holds, by kind and state, which says what the floor is made of.
"""

import json
import sys
from collections import Counter


def records(stem):
    with open(stem + ".jsonl") as f:
        for line in f:
            if line.strip():
                try:
                    yield json.loads(line)
                except json.JSONDecodeError:
                    pass


def main():
    stems = [s.removesuffix(".jsonl").removesuffix(".json") for s in sys.argv[1:]]
    if not stems:
        sys.exit(__doc__)
    floor = []
    limit = None
    for n, stem in enumerate(stems):
        kinds = Counter()
        errors = denied = 0
        first = last = None
        for r in records(stem):
            e = r["event"]
            kinds[e["event"]] += 1
            first = first or r["at"]
            last = r["at"]
            if e["event"] == "tool.finished" and e["is_error"]:
                errors += 1
            elif e["event"] == "permission.decided" and e["grant"] == "deny":
                denied += 1
            elif e["event"] == "context.compacted":
                floor.append((n, e["report"]["tokens_before"], e["report"]["tokens_after"]))
            elif e["event"] == "model.requested":
                limit = e["model"].get("context_limit") or limit
        minutes = (last - first) / 60000 if first else 0
        print(
            f"{n}: {minutes:.0f} min, {kinds['model.requested']} requests, "
            f"{kinds['tool.finished']} calls ({errors} errors, {denied} refused), "
            f"{kinds['model.failed'] + kinds['step.failed']} failures, "
            f"{kinds['context.compacted']} compactions, "
            f"{kinds['context.undone']} undo, {kinds['context.redone']} redo, "
            f"ended {'finished' if kinds['session.finished'] else 'without session.finished'}"
        )
    if floor:
        print(f"\nthe floor after each compaction (limit {limit}):")
        print("  " + " ".join(f"{n}:{after}" for n, _, after in floor))
        gains = [before - after for _, before, after in floor]
        print(f"  recovered per pass: first {gains[0]}, last {gains[-1]}, median {sorted(gains)[len(gains)//2]}")
    with open(stems[-1] + ".json") as f:
        snapshot = json.load(f)
    held = Counter()
    for item in snapshot["items"]:
        if item["state"] in ("active", "pinned"):
            held[(item["kind"]["kind"], item["state"])] += item["tokens"]
    print("\nwhat the last snapshot sends in full, by kind and state (tokens):")
    for (kind, state), tokens in held.most_common():
        print(f"  {tokens:>7}  {kind} ({state})")


if __name__ == "__main__":
    main()
