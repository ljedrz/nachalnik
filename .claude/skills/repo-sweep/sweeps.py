"""What a sweep's files say about it, for `run.py` and `status.py`.

A sweep is complete when the process ended on its own (`exit 0`), every message in `NAME.in`
reached the model, every turn got answers from it, and the last answer ended as an answer rather
than being cut off. Anything else that has ended is incomplete, with the reason, and `run.py` runs
it again.

note: a turn that failed after the model had answered in it did its work up to there, and the
session goes on with the next message, which says to continue where it left off; an endpoint that
refuses one request in a hundred - one larger than it will take, say, which kamchatka answers by
compacting - is no reason to throw a sweep's hours away. A turn that failed before any answer is
one the endpoint never served: that sweep is missing a part, and is run again.

note: read from the records and not from what the model says, because models differ in every
other respect - one writes FINDINGS, one writes DONE, one writes nothing - and the records are the
same for all of them.
"""

import json
import os
import re
import time

SWEEPS = os.environ.get("SWEEPS") or os.path.join(os.environ.get("TMPDIR", "/tmp"), "sweeps")

# what kamchatka prints while it waits on the endpoint: `MODEL answered 429; trying again in 4s`,
# and the same for a timeout or a refused connection
RETRYING = re.compile(r"; trying again in \d+s")
REFUSED = "not permitted"
# the stop reason of a last answer that is an answer: `tool_use` there is a turn that stopped
# partway, and `max_tokens` one cut off
ANSWERED = {"end_turn"}


def path(name, suffix):
    return os.path.join(SWEEPS, name + suffix)


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def expected(name):
    """How many messages the session was given: the lines of NAME.in that are not commands."""
    try:
        with open(path(name, ".in")) as f:
            return sum(1 for line in f if line.strip() and not line.startswith("/"))
    except FileNotFoundError:
        return None


class Records:
    """Read incrementally, since a sweep's records grow to megabytes and are read every minute."""

    def __init__(self):
        self.offset = 0
        self.inode = None
        self.sent = 0
        self.requests = 0
        self.inputs = []
        self.failed = []
        self.empty = 0  # turns that failed with nothing answered in them
        self.answered = 0  # answers in the turn under way
        self.stop = None

    def read(self, name):
        p = path(name, ".jsonl")
        try:
            st = os.stat(p)
        except FileNotFoundError:
            return
        if st.st_ino != self.inode or st.st_size < self.offset:
            self.__init__()
            self.inode = st.st_ino
        with open(p, "rb") as f:
            f.seek(self.offset)
            for raw in f:
                if not raw.endswith(b"\n"):
                    break  # a record still being written
                self.offset += len(raw)
                try:
                    event = json.loads(raw)["event"]
                except (ValueError, KeyError, TypeError):
                    continue
                kind = event.get("event")
                if kind == "context.added" and event.get("kind") == "user_message" \
                        and event.get("source") == "user":
                    self.sent += 1
                    self.answered = 0
                elif kind == "model.finished":
                    self.requests += 1
                    self.answered += 1
                    used = (event.get("usage") or {}).get("input_tokens")
                    if used:
                        self.inputs.append(used)
                    stop = event.get("stop")
                    self.stop = stop if isinstance(stop, str) else json.dumps(stop)
                elif kind in ("model.failed", "step.failed"):
                    self.failed.append(str(event.get("error", ""))[:160])
                    if not self.answered:
                        self.empty += 1


_records = {}


def state(name, now=None):
    """Everything known about one sweep, as a dict; `status` is one of
    queued / running / lost / complete / incomplete."""
    now = now or time.time()
    s = {"name": name, "status": "queued", "why": ""}
    if not os.path.exists(path(name, ".in")):
        return s
    records = _records.setdefault(name, Records())
    records.read(name)
    try:
        with open(path(name, ".err"), errors="replace") as f:
            prose = f.read()
    except FileNotFoundError:
        prose = ""  # not written yet, or moved aside while this read
    exits = re.findall(r"^exit (\d+)$", prose, re.M)
    # the latest of them: a model's answer streams to the prose while no record is written
    times = []
    for suffix in (".in", ".jsonl", ".err"):
        try:
            times.append(os.stat(path(name, suffix)).st_mtime)
        except FileNotFoundError:
            pass  # not written yet, or moved aside while this read
    changed = max(times, default=now)
    s.update(
        expected=expected(name),
        sent=records.sent,
        requests=records.requests,
        mean=sum(records.inputs) // max(len(records.inputs), 1),
        largest=max(records.inputs, default=0),
        failed=len(records.failed),
        empty=records.empty,
        turn=records.answered,
        retrying=len(RETRYING.findall(prose)),
        refused=prose.count(REFUSED),
        quiet=int(now - changed),
        stop=records.stop,
    )
    if not exits:
        try:
            pid = int(open(path(name, ".pid")).read())
        except (FileNotFoundError, ValueError):
            pid = None
        s["pid"] = pid
        if pid and alive(pid):
            s["status"] = "running"
        else:
            s["status"], s["why"] = "lost", "the session ended without saying how"
        return s
    code = int(exits[-1])
    s["exit"] = code
    whys = []
    if code != 0:
        whys.append({124: "its deadline ended it", 143: "it was terminated",
                     130: "it was interrupted"}.get(code, f"exit {code}"))
    if s["expected"] is not None and records.sent < s["expected"]:
        whys.append(f"{records.sent} of {s['expected']} messages sent")
    if records.empty:
        whys.append(f"{records.empty} turn(s) failed with nothing answered: {records.failed[-1]}")
    if records.stop not in ANSWERED:
        whys.append(f"the last answer stopped: {records.stop}")
    s["status"] = "incomplete" if whys else "complete"
    s["why"] = "; ".join(whys)
    return s
