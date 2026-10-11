#!/usr/bin/env python3
"""What a sweep found: its replies and its `context` notes, which is where a hygiene run keeps its
findings. With -r, its reasoning as well.

usage: read.py NAME [-r]

From the snapshot the session wrote, beside its log (see `sweeps.py`): kamchatka rewrites it each
time the session comes to rest, so a sweep still running is read as of the end of its last turn,
and says so. A resumed sweep's snapshot holds everything before the resume as well. Where the
current part has none, from the newest earlier part's in `old/parts/`; and where no part has one,
from the records, which hold every note as the arguments of a `context` call but none of the
replies, so that reading gives the notes alone and says so.
"""

import glob
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sweeps  # noqa: E402


def text(content):
    if content is None:
        return ""
    if isinstance(content, str):
        return content
    if isinstance(content, dict):
        for key in ("text", "Text"):
            if isinstance(content.get(key), str):
                return content[key]
        if "blocks" in content:
            return "\n".join(text(block) for block in content["blocks"])
        return json.dumps(content)[:2000]
    if isinstance(content, list):
        return "\n".join(text(part) for part in content)
    return str(content)


def from_snapshot(path, reasoning):
    items = json.load(open(path))["items"]
    for item in items:
        kind = item["kind"]
        if kind.get("kind") != "assistant_message":
            continue
        said = text(item.get("content"))
        if said.strip():
            print(f"=== item {item['id']} TEXT ===\n{said}\n")
        thought = text(kind.get("reasoning"))
        if reasoning and thought.strip():
            print(f"--- item {item['id']} reasoning ---\n{thought}\n")
    for item in items:
        if item.get("source") == "agent":
            print(f"=== note {item['id']} [{item.get('label', '')}] ({item.get('state', '')}) ===\n"
                  f"{text(item.get('content'))}\n")


def from_records(name):
    print(f"(no snapshot for {name}: the notes from its records, without its replies)\n")
    for line in open(sweeps.log(name), errors="replace"):
        try:
            event = json.loads(line)["event"]
        except (ValueError, KeyError, TypeError):
            continue
        if event.get("event") != "tool.requested" or event.get("tool") != "context":
            continue
        args = event.get("args") or {}
        # the shapes models wrap a call in: `{"call": {...}}`, or the text of one
        if isinstance(args, dict) and isinstance(args.get("_unparsed"), str):
            try:
                args = json.loads(args["_unparsed"])
            except ValueError:
                print(f"=== note (did not parse) ===\n{args['_unparsed']}\n")
                continue
        if isinstance(args, dict) and isinstance(args.get("call"), dict):
            args = args["call"]
        if isinstance(args, dict) and args.get("action") == "note":
            rest = {k: v for k, v in args.items() if k != "action"}
            said = rest.pop("content", None) or rest.pop("text", None) or rest.pop("note", None)
            print(f"=== note {json.dumps(rest)[:200]} ===\n{said}\n")


def earlier_parts(name):
    """The snapshots of the parts before a resume, newest first."""
    found = []
    for kept in glob.glob(os.path.join(sweeps.SWEEPS, "old", "parts", f"{name}.*.session")):
        n = int(re.search(r"\.(\d+)\.session$", kept).group(1))
        snapshot = os.path.join(sweeps.LOGS, open(kept).read().strip() + ".json")
        if os.path.exists(snapshot):
            found.append((n, snapshot))
    return [snapshot for _, snapshot in sorted(found, reverse=True)]


def main():
    args = sys.argv[1:]
    if not args or args[0].startswith("-"):
        sys.exit(__doc__)
    name = args[0]
    path = sweeps.snapshot(name)
    if path and sweeps.state(name)["status"] == "running":
        print("(still running: as of the end of its last turn)\n")
    if not path:
        path = next(iter(earlier_parts(name)), None)
        if path:
            print(f"(no snapshot of the current part: read from the part before it, {path})\n")
    if path:
        from_snapshot(path, "-r" in args)
    else:
        from_records(name)


if __name__ == "__main__":
    main()
