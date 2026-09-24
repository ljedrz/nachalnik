#!/usr/bin/env python3
"""Per sweep: how many requests it made, and the mean and largest input it sent.

usage: stats.py NAME...   (reads $SWEEPS/NAME.jsonl)
"""
import json, os, sys

sweeps = os.environ.get("SWEEPS", os.path.join(os.environ.get("TMPDIR", "/tmp"), "sweeps"))
for name in sys.argv[1:]:
    sizes = []
    for line in open(os.path.join(sweeps, name + ".jsonl")):
        try:
            event = json.loads(line)["event"]
        except (ValueError, KeyError):
            continue  # a record cut off by a sweep that was killed
        if event.get("event") == "model.finished":
            used = (event.get("usage") or {}).get("input_tokens")
            if used:
                sizes.append(used)
    mean = sum(sizes) // max(len(sizes), 1)
    print(f"{name}: {len(sizes)} requests, mean {mean:,}, max {max(sizes, default=0):,}")
