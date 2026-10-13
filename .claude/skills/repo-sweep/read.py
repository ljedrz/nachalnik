#!/usr/bin/env python3
"""What a sweep found: its replies, which hold its `FINDING:` lines, and any `context` notes it
kept. With -r, its reasoning as well; with -f, its `FINDING:` lines alone, each once.

usage: read.py NAME [-r | -f]

From the snapshot the session wrote, beside its log (see `sweeps.py`): kamchatka rewrites it each
time the session comes to rest, so a sweep still running is read as of the end of its last turn,
and says so. A resumed sweep's snapshot holds everything before the resume as well. Where the
current part has none, from the newest earlier part's in `old/parts/`; and where no part has one,
from the records, which hold every note as the arguments of a `context` call but none of the
replies, so that reading gives the notes alone and says so. -f reads what no snapshot holds from
the prose instead, NAME.err and the parts' before it, which has every reply as it was written.
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
    """The replies, their reasoning where asked for, then the notes: `(heading, text)` each."""
    items = json.load(open(path))["items"]
    for item in items:
        kind = item["kind"]
        if kind.get("kind") != "assistant_message":
            continue
        said = text(item.get("content"))
        if said.strip():
            yield f"item {item['id']} TEXT", said
        thought = text(kind.get("reasoning"))
        if reasoning and thought.strip():
            yield f"item {item['id']} reasoning", thought
    for item in items:
        if item.get("source") == "agent":
            yield (f"note {item['id']} [{item.get('label', '')}] ({item.get('state', '')})",
                   text(item.get("content")))


def from_records(name):
    """The notes, as the arguments of the `context` calls that made them."""
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
                yield "note (did not parse)", args["_unparsed"]
                continue
        if isinstance(args, dict) and isinstance(args.get("call"), dict):
            args = args["call"]
        if isinstance(args, dict) and args.get("action") == "note":
            rest = {k: v for k, v in args.items() if k != "action"}
            said = rest.pop("content", None) or rest.pop("text", None) or rest.pop("note", None)
            yield f"note {json.dumps(rest)[:200]}", said


def earlier_parts(name):
    """The snapshots of the parts before a resume, newest first."""
    found = []
    for kept in glob.glob(os.path.join(sweeps.SWEEPS, "old", "parts", f"{name}.*.session")):
        n = int(re.search(r"\.(\d+)\.session$", kept).group(1))
        snapshot = os.path.join(sweeps.LOGS, open(kept).read().strip() + ".json")
        if os.path.exists(snapshot):
            found.append((n, snapshot))
    return [snapshot for _, snapshot in sorted(found, reverse=True)]


def entries(name, reasoning=False):
    """What a sweep found, as `(heading, text)`, after a line saying where it was read from if
    that is not the current part's snapshot: from it, an earlier part's, or the records."""
    path = sweeps.snapshot(name)
    if path and sweeps.state(name)["status"] == "running":
        yield None, "(still running: as of the end of its last turn)"
    if not path:
        path = next(iter(earlier_parts(name)), None)
        if path:
            yield None, f"(no snapshot of the current part: read from the part before it, {path})"
    if path:
        yield from from_snapshot(path, reasoning)
    else:
        yield None, f"(no snapshot for {name}: the notes from its records, without its replies)"
        yield from from_records(name)


FINDING = re.compile(r"FINDING:\s*(.*\S)")


def findings(name):
    """The sweep's `FINDING:` lines, each once, in the order first written: from its snapshot, or
    from its prose where no part has one.

    note: each once because a sweep's last message asks for every line again, and a model that
    writes them as it goes and then in its answer writes each twice more."""
    if sweeps.snapshot(name) or earlier_parts(name):
        texts = (said for heading, said in entries(name) if heading and said)
    else:
        parts = sorted(glob.glob(os.path.join(sweeps.SWEEPS, "old", "parts", f"{name}.*.err")),
                       key=lambda p: int(re.search(r"\.(\d+)\.err$", p).group(1)))
        texts = (open(p, errors="replace").read()
                 for p in parts + [sweeps.path(name, ".err")] if os.path.exists(p))
    seen = {}
    for said in texts:
        for line in said.splitlines():
            m = FINDING.search(line)
            if m:
                seen.setdefault(" ".join(m.group(1).split()), None)
    return list(seen)


def main():
    args = sys.argv[1:]
    if not args or args[0].startswith("-"):
        sys.exit(__doc__)
    if "-f" in args:
        print("\n".join(findings(args[0])))
        return
    for heading, said in entries(args[0], "-r" in args):
        if heading is None:
            print(f"{said}\n")
        elif heading.startswith("item ") and heading.endswith(" reasoning"):
            print(f"--- {heading} ---\n{said}\n")
        else:
            print(f"=== {heading} ===\n{said}\n")


if __name__ == "__main__":
    main()
