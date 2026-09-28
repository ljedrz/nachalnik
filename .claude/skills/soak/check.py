#!/usr/bin/env python3
"""Holds the records of a soak - one session carried across several processes by `kamchatka -r` -
to the invariants that span them.

usage: check.py [--kamchatka BIN] STEM...

Each STEM names one segment's pair, `STEM.jsonl` (the log) and `STEM.json` (the snapshot it left),
in the order the segments ran; segment k+1 must have been resumed from segment k's snapshot. The
segment's log is its own records, not the stdout of a later process.

`kamchatka --check` is run on each pair first: it knows one record. What this adds is what only the
chain can say - a resume that picked up where the snapshot says, identifiers never issued twice
across it, states carried over and never jumped, a pin never compacted, no item gone, no content
changed without the event that says so, and no call run without a decision to run it.

Exit status 1 when anything is a VIOLATION. A NOTE is a fact worth reading that is not one: a
resume from a snapshot taken before the end of its log is what a killed process leaves.
"""

import hashlib
import json
import os
import subprocess
import sys

SHOWN = {"active", "pinned", "elided"}


class Report:
    def __init__(self):
        self.violations = []
        self.notes = []

    def violation(self, segment, text):
        self.violations.append(f"[{segment}] {text}")

    def note(self, segment, text):
        self.notes.append(f"[{segment}] {text}")


def read_log(path, report, name):
    records = []
    with open(path) as f:
        for number, line in enumerate(f, 1):
            if not line.strip():
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                # the last line of a killed run is the likely one, and `--check` names it
                report.note(name, f"line {number} of the log is not JSON")
    return records


def digest(content):
    return hashlib.sha256(json.dumps(content, sort_keys=True).encode()).hexdigest()[:16]


def run_check(binary, stem, report, name):
    try:
        out = subprocess.run(
            [binary, "--check", stem], capture_output=True, text=True, timeout=120
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        report.violation(name, f"`kamchatka --check` could not run: {e}")
        return
    lines = [line for line in (out.stdout + out.stderr).splitlines() if line.strip()]
    if out.returncode != 0:
        for line in lines:
            # a call a killed or interrupted run left open is what --check is told to say, and
            # which segments ended that way is the soak's business, not a fault in the record
            kind = report.note if "never finished" in line else report.violation
            kind(name, f"--check: {line}")


class Lineage:
    """What the chain has issued and what state it has left each item in."""

    def __init__(self):
        self.items = {}  # id -> state, for every item the context holds
        self.content = {}  # id -> digest of content, from the last snapshot
        self.added = set()  # every context id ever issued in the lineage
        self.calls = set()
        self.permissions = set()
        self.decided = set()
        self.last_seq = 0
        # what the last segment issued past its snapshot, which a resume cannot know was issued
        self.stale = set()

    def seed(self, snapshot):
        self.items = {item["id"]: item["state"] for item in snapshot["items"]}
        self.content = {item["id"]: digest(item["content"]) for item in snapshot["items"]}
        self.added |= set(self.items)
        self.calls |= set(snapshot.get("used_calls", []))


def segment(name, records, snapshot, previous, lineage, report):
    """Walks one segment's log against what the lineage carried into it."""
    if not records:
        report.violation(name, "the log is empty")
        return
    first = records[0]["event"]
    if previous is None:
        if first["event"] != "session.started":
            report.violation(name, f"the first segment begins with `{first['event']}`")
    else:
        if first["event"] != "session.resumed":
            report.violation(name, f"a resumed segment begins with `{first['event']}`")
        elif first["items"] != len(previous["items"]):
            report.violation(
                name,
                f"resumed with {first['items']} items, and the snapshot it was resumed from "
                f"holds {len(previous['items'])}",
            )
        expected = previous["last_seq"] + 1
        if records[0]["seq"] != expected:
            report.violation(
                name,
                f"its first record is {records[0]['seq']}, and the snapshot it was resumed from "
                f"was taken at {previous['last_seq']}",
            )

    replaced = set()  # items whose content an event said changed
    removed_by_undo = set()
    decided = {}  # call -> grant
    state = {"state": "idle"}
    passing = {}  # id -> the state a run of context.changed took it from
    frozen = None
    for record in records:
        seq, event = record["seq"], record["event"]
        kind = event["event"]
        if kind not in ("context.changed", "context.added", "context.compacted"):
            passing = {}
        if seq <= lineage.last_seq and kind not in ("session.started", "session.resumed"):
            report.violation(name, f"record {seq} is not past {lineage.last_seq}")
        lineage.last_seq = max(lineage.last_seq, seq)

        if kind == "state.changed":
            if event["from"] != state:
                report.violation(
                    name,
                    f"record {seq} moves from {event['from']['state']}, where the machine was "
                    f"{state['state']}",
                )
            state = event["to"]
        elif kind == "context.added":
            cid = event["id"]
            if ("item", cid) in lineage.stale:
                report.note(name, f"record {seq} adds item {cid} again: the last segment issued it past its snapshot")
            elif cid in lineage.added:
                report.violation(name, f"record {seq} adds item {cid}, an identifier already issued")
            if lineage.added and cid <= max(lineage.added):
                report.violation(name, f"record {seq} adds item {cid}, not past {max(lineage.added)}")
            lineage.added.add(cid)
            lineage.items[cid] = "active"
            replaced.add(cid)  # new content, not changed content
        elif kind == "context.changed":
            cid = event["id"]
            if cid not in lineage.items:
                report.violation(name, f"record {seq} changes item {cid}, which the lineage does not hold")
            elif lineage.items[cid] not in (None, event["from"]):
                report.violation(
                    name,
                    f"record {seq} changes item {cid} from {event['from']}, where it was "
                    f"{lineage.items[cid]}",
                )
            lineage.items[cid] = event["to"]
            # a compaction pass announces its moves just before its report
            passing[cid] = event["from"]
        elif kind == "context.compacted":
            r = event["report"]
            for entry in r["removed"] + r["elided"]:
                if passing.get(entry["id"]) == "pinned":
                    report.violation(name, f"record {seq}: compaction took pinned item {entry['id']}")
                if lineage.items.get(entry["id"]) not in ("excluded", "elided"):
                    report.violation(
                        name,
                        f"record {seq}: compaction reports item {entry['id']} taken, and it is "
                        f"{lineage.items.get(entry['id'])}",
                    )
            for entry in r.get("refused", []):
                report.note(name, f"record {seq}: compaction was refused item {entry['id']}")
        elif kind == "context.replaced":
            replaced.add(event["id"])
        elif kind == "context.undone":
            for cid in event.get("removed", []):
                lineage.items.pop(cid, None)
                removed_by_undo.add(cid)
            for cid in event.get("changed", []):
                lineage.items[cid] = None  # the log does not say what state it went back to
                replaced.add(cid)
        elif kind == "context.redone":
            for cid in event.get("restored", []) + event.get("changed", []):
                lineage.items[cid] = None
                removed_by_undo.discard(cid)
                replaced.add(cid)
        elif kind == "model.requested":
            for cid in event["items"]:
                held = lineage.items.get(cid, "absent")
                if held is not None and held not in SHOWN:  # None: an undo left it unknown
                    report.violation(name, f"record {seq} sends item {cid}, which is {held}")
        elif kind == "permission.requested":
            pid = event["request"]["id"]
            if ("permission", pid) in lineage.stale:
                report.note(name, f"record {seq} asks permission {pid} again: the last segment issued it past its snapshot")
            elif pid in lineage.permissions:
                report.violation(name, f"record {seq} asks permission {pid} a second time")
            lineage.permissions.add(pid)
        elif kind == "permission.decided":
            decided[event["call"]] = event["grant"]
            # a question the policy answered is numbered too, with no `permission.requested`
            pid = event["id"]
            if ("permission", pid) in lineage.stale and pid not in lineage.decided:
                report.note(name, f"record {seq} decides permission {pid} again: the last segment issued it past its snapshot")
            elif pid in lineage.decided:
                report.violation(name, f"record {seq} decides permission {pid} a second time")
            lineage.decided.add(pid)
        elif kind == "tool.requested":
            call = event["call"]
            if ("call", call) in lineage.stale:
                report.note(name, f"record {seq} asks for call `{call}` again: the last segment issued it past its snapshot")
            elif call in lineage.calls:
                report.violation(name, f"record {seq} asks for call `{call}`, already issued")
            lineage.calls.add(call)
        elif kind == "tool.started":
            grant = decided.get(event["call"])
            if grant != "allow":
                report.violation(
                    name,
                    f"record {seq} starts call `{event['call']}` to `{event['tool']}` with "
                    f"{'no decision' if grant is None else 'a ' + grant}",
                )
        elif kind == "tool.finished":
            for cid in filter(None, (event["item"], event.get("whole"))):
                if cid not in lineage.added:
                    report.violation(name, f"record {seq} records call output as item {cid}, never added")

        if snapshot is not None and seq == snapshot["last_seq"]:
            # what the snapshot is compared with: the walk where it was taken, not where it ended
            frozen = (dict(lineage.items), set(lineage.added), set(replaced), set(removed_by_undo))

    if snapshot is None:
        report.note(name, "no snapshot; the lineage cannot be carried past this segment")
        return

    # what was issued past the snapshot is forgotten by a resume from it
    stale = set()
    for r in records:
        if r["seq"] <= snapshot["last_seq"]:
            continue
        e = r["event"]
        if e["event"] == "context.added":
            stale.add(("item", e["id"]))
        elif e["event"] == "tool.requested":
            stale.add(("call", e["call"]))
        elif e["event"] == "permission.requested":
            stale.add(("permission", e["request"]["id"]))
        elif e["event"] == "permission.decided":
            stale.add(("permission", e["id"]))
    lineage.stale = stale

    # the snapshot against what the walk left
    tail = records[-1]["seq"]
    if snapshot["last_seq"] < tail:
        lost = [r for r in records if r["seq"] > snapshot["last_seq"]]
        lost_items = sorted(r["event"]["id"] for r in lost if r["event"]["event"] == "context.added")
        lost_calls = sorted(r["event"]["call"] for r in lost if r["event"]["event"] == "tool.requested")
        report.note(
            name,
            f"the snapshot was taken at record {snapshot['last_seq']} and the log runs to {tail}: "
            f"{len(lost)} records past it, adding items {lost_items or 'none'} and calls "
            f"{lost_calls or 'none'}, are not in what a resume carries on from",
        )
    if frozen is None:
        report.violation(name, f"the snapshot was taken at record {snapshot['last_seq']}, which the log does not have")
        return
    held_items, added, replaced, removed_by_undo = frozen
    ids = {item["id"] for item in snapshot["items"]}
    gone = sorted(added - ids - removed_by_undo)
    if gone:
        report.violation(name, f"items {gone} were issued and the snapshot does not hold them")
    if ids and snapshot["next_item"] <= max(ids):
        report.violation(name, f"the snapshot's next_item {snapshot['next_item']} is not past {max(ids)}")
    missing_calls = sorted(
        c
        for c in lineage.calls
        - set(snapshot.get("used_calls", []))
        if not any(
            r["seq"] > snapshot["last_seq"] and r["event"].get("call") == c for r in records
        )
    )
    if missing_calls:
        report.violation(name, f"calls {missing_calls[:5]} were issued and the snapshot's used_calls lacks them")
    if lineage.permissions and snapshot.get("next_permission", 0) <= max(lineage.permissions):
        # only those issued before the snapshot count
        before = {
            r["event"]["request"]["id"]
            for r in records
            if r["seq"] <= snapshot["last_seq"] and r["event"]["event"] == "permission.requested"
        }
        if before and snapshot["next_permission"] <= max(before):
            report.violation(name, f"next_permission {snapshot['next_permission']} is not past {max(before)}")
    for item in snapshot["items"]:
        held = held_items.get(item["id"], "absent")
        if held == "absent":
            report.violation(name, f"the snapshot has item {item['id']}, which the log had not issued by then")
        elif held is not None and held != item["state"]:
            report.violation(
                name, f"the snapshot has item {item['id']} {item['state']}, the log leaves it {held}"
            )
        was = lineage.content.get(item["id"])
        if was is not None and was != digest(item["content"]) and item["id"] not in replaced:
            report.violation(name, f"item {item['id']}'s content changed with no event saying so")


def main():
    args = sys.argv[1:]
    binary = "kamchatka"
    if args[:1] == ["--kamchatka"]:
        binary, args = args[1], args[2:]
    if not args:
        sys.exit(__doc__)
    report = Report()
    lineage = Lineage()
    previous = None
    last_tail = 0  # the last record the previous segment's log holds
    for number, stem in enumerate(args):
        stem = stem.removesuffix(".jsonl").removesuffix(".json")
        name = f"{number}:{os.path.basename(stem)}"
        run_check(binary, stem, report, name)
        stale_tail = last_tail
        records = read_log(stem + ".jsonl", report, name)
        snapshot = None
        if os.path.exists(stem + ".json"):
            with open(stem + ".json") as f:
                snapshot = json.load(f)
        if previous is not None:
            # what a resume carries on from is the snapshot, not the whole of the last log
            lineage.items, lineage.content = {}, {}
            lineage.last_seq = previous["last_seq"]
            lineage.added -= {v for k, v in lineage.stale if k == "item"}
            lineage.calls -= {v for k, v in lineage.stale if k == "call"}
            lineage.permissions -= {v for k, v in lineage.stale if k == "permission"}
            lineage.decided -= {v for k, v in lineage.stale if k == "permission"}
            reissued = [r["seq"] for r in records if r["seq"] <= stale_tail]
            if reissued:
                report.note(name, f"records {reissued[0]} to {reissued[-1]} are numbered again: "
                            "the last segment's log had already used those numbers past its snapshot")
            lineage.seed(previous)
        segment(name, records, snapshot, previous, lineage, report)
        kinds = {}
        for r in records:
            kinds[r["event"]["event"]] = kinds.get(r["event"]["event"], 0) + 1
        interesting = ("context.compacted", "context.undone", "context.redone", "context.replaced",
                       "tool.finished", "model.requested", "permission.decided", "context.changed")
        print(f"{name}: {len(records)} records; " + ", ".join(f"{k} {kinds.get(k, 0)}" for k in interesting))
        previous = snapshot
        last_tail = records[-1]["seq"] if records else 0
    for text in report.notes:
        print("NOTE", text)
    for text in report.violations:
        print("VIOLATION", text)
    print(f"{len(report.violations)} violation(s), {len(report.notes)} note(s)")
    sys.exit(1 if report.violations else 0)


if __name__ == "__main__":
    main()
