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

note: the records read are the session's own log, which kamchatka writes under
$TMPDIR/kamchatka/ by the session's name, and not the copy `sweep.sh` sends to NAME.jsonl. The
copy is only as safe as the shell that redirected it: a `sweep.sh` edited while sweeps ran was read
on by each of them when its session ended, from the byte it had got to, and a fragment of the
launch line truncated NAME.jsonl and NAME.err after four hours of work. The log is written by the
session alone and ends in `session.finished`; the copy's first record names it, and the name is
kept in NAME.session the first time it is read.
"""

import json
import os
import re
import time

SWEEPS = os.environ.get("SWEEPS") or os.path.join(os.environ.get("TMPDIR", "/tmp"), "sweeps")
# where kamchatka writes each session's log and snapshot: its temporary directory's `kamchatka`
LOGS = os.environ.get("KAMCHATKA_LOGS") or os.path.join(os.environ.get("TMPDIR", "/tmp"), "kamchatka")

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


def session(name):
    """The name of the sweep's session, which is the name of its log: kept in NAME.session, and
    read from the first record of NAME.jsonl the first time."""
    kept = path(name, ".session")
    try:
        return open(kept).read().strip() or None
    except FileNotFoundError:
        pass
    try:
        with open(path(name, ".jsonl"), "rb") as f:
            first = f.readline()
        event = json.loads(first)["event"]
    except (FileNotFoundError, ValueError, KeyError, TypeError):
        return None
    if event.get("event") != "session.started" or not event.get("session"):
        return None
    with open(kept, "w") as f:
        f.write(event["session"] + "\n")
    return event["session"]


def log(name):
    """The session's own record of itself, or the copy in NAME.jsonl while its name is not known."""
    named = session(name)
    if named and os.path.exists(os.path.join(LOGS, named + ".jsonl")):
        return os.path.join(LOGS, named + ".jsonl")
    return path(name, ".jsonl")


def snapshot(name):
    """The session kamchatka wrote when it ended, if it did."""
    named = session(name)
    found = named and os.path.join(LOGS, named + ".json")
    return found if found and os.path.exists(found) else None


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
        self.throttled = 0  # turns that failed on a 429 kamchatka's own retries did not outlast
        self.answered = 0  # answers in the turn under way
        self.stop = None
        self.finished = False  # the log's last record, written as the session ends on its own
        self.read_from = None

    def read(self, name):
        p = log(name)
        try:
            st = os.stat(p)
        except FileNotFoundError:
            return
        if p != self.read_from or st.st_ino != self.inode or st.st_size < self.offset:
            self.__init__()
            self.inode = st.st_ino
            self.read_from = p
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
                elif kind == "session.finished":
                    self.finished = True
                elif kind in ("model.failed", "step.failed"):
                    self.failed.append(str(event.get("error", ""))[:160])
                    self.throttled += "429" in self.failed[-1]
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
    for p in [path(name, suffix) for suffix in (".in", ".jsonl", ".err")] + [log(name)]:
        try:
            times.append(os.stat(p).st_mtime)
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
        throttled=records.throttled,
        turn=records.answered,
        retrying=len(RETRYING.findall(prose)),
        refused=prose.count(REFUSED),
        quiet=int(now - changed),
        stop=records.stop,
    )
    try:
        pid = int(open(path(name, ".pid")).read())
    except (FileNotFoundError, ValueError):
        pid = None
    s["pid"] = pid
    s["finished"] = records.finished
    if exits:
        s["exit"] = int(exits[-1])
    if pid and alive(pid):
        s["status"] = "running"
        return s
    if not exits and not records.finished:
        s["status"], s["why"] = "lost", "the session ended without saying how"
        return s
    whys = []
    # note: the exit code says why a session did not finish, and nothing about one that did:
    # kamchatka exits 1 when any turn failed, which a turn failed part-way is
    if not records.finished:
        code = s.get("exit")
        whys.append({124: "its deadline ended it", 143: "it was terminated",
                     130: "it was interrupted"}.get(code, f"it ended without finishing (exit {code})"))
    if s["expected"] is not None and records.sent < s["expected"]:
        whys.append(f"{records.sent} of {s['expected']} messages sent")
    if records.empty:
        whys.append(f"{records.empty} turn(s) failed with nothing answered: {records.failed[-1]}")
    if records.stop not in ANSWERED:
        whys.append(f"the last answer stopped: {records.stop}")
    s["status"] = "incomplete" if whys else "complete"
    s["why"] = "; ".join(whys)
    return s
