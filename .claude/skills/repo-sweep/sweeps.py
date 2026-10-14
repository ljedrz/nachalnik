"""What a sweep's files say about it, for `run.py` and `status.py`.

A sweep is complete when the session ended on its own, every message in `NAME.in` reached the
model, and the last of them got an answer that ended (a `finished` state with stop `end_turn`)
before the session did. Anything else that has ended is incomplete, with the reason, and `run.py`
resumes it, or runs it again where there is nothing to resume.

note: the last message, and not every one, because the last is the one a resume can still answer.
`drive.py` does not send a message before the one ahead of it is answered, so under it the two
are the same. A sweep piped its lines all at once, as `sweep.sh` used to, went on to the next
message after a failed turn, and the message passed over is counted in `skipped` and said by
`status.py` rather than making the sweep incomplete: nothing can answer it now, and judged
incomplete, such a sweep would be resumed with nothing to send until it ran out of parts, and then
run again from nothing.

note: a failed turn is not a failure of the sweep. `drive.py` carries it on with `/continue`, and
an endpoint that refuses one request in a hundred - one larger than it will take, say, which
kamchatka answers by compacting - is no reason to throw a sweep's hours away.

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

import glob
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
# the stop reasons of an answer that ended: `tool_use` there is a turn that stopped partway, and
# `length` or `{"other": "cut off"}` one cut off. A refusal is not continued: `/continue` refuses it
ANSWERED = {"end_turn", "refusal"}


# how a cut's message is known in a snapshot, which holds what each message said (`drive.py`'s
# `CARRY` and `OVERRUN` start so); the records do not, and NAME.carried counts them there instead
CARRIED = "The context passed its ceiling"


def carried(name):
    """How many messages `drive.py` sent of its own in this part, and how many messages it sent
    early: `(cuts, wrap-ups)`."""
    try:
        lines = open(path(name, ".carried")).read().split()
    except FileNotFoundError:
        return 0, 0
    return lines.count("carry"), lines.count("wrap")


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
    read from the first record of NAME.jsonl the first time.

    note: a resumed session's first record is `session.resumed`, naming the session it carried
    on from, and its own log is that name with a suffix (`-2`, `-3`). Which one is the log whose
    last `session.resumed` is the same line as the copy's first: a log begins with the one it was
    resumed from, carried forward, so a later one holds this resume as well, ahead of its own.
    Without it a resumed sweep had no snapshot of its own, and a second resume started from the
    first one's, losing what came between."""
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
    found = event.get("session")
    if not found:
        return None
    if event.get("event") == "session.resumed":
        found = None
        for candidate in glob.glob(os.path.join(LOGS, glob.escape(event["session"]) + "*.jsonl")):
            if last_resume(candidate) == first:
                found = os.path.basename(candidate)[: -len(".jsonl")]
                break
    elif event.get("event") != "session.started":
        return None
    if not found:
        return None
    with open(kept, "w") as f:
        f.write(found + "\n")
    return found


def last_resume(log):
    """The last `session.resumed` line of a log, as it was written; `None` where there is none."""
    last = None
    with open(log, "rb") as f:
        for line in f:
            if b'"session.resumed"' in line:
                try:
                    if json.loads(line)["event"]["event"] == "session.resumed":
                        last = line
                except (ValueError, KeyError, TypeError):
                    pass
    return last


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
        self.ended = False  # whether the last message has had an answer that ended
        self.skipped = 0  # messages sent before the one ahead of them had such an answer
        self.throttled = 0  # turns that failed on a 429 kamchatka's own retries did not outlast
        self.answered = 0  # answers in the turn under way
        # calls the policy refused, read off the records: the prose holds what a sweep read and
        # grepped as well, and one that searched for a refusal's wording counted as refused
        self.refused = 0
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
                    # not the first in this log: what came before it is in the snapshot a
                    # resumed session started from, which the log does not show
                    if self.sent and not self.ended:
                        self.skipped += 1
                    self.sent += 1
                    self.answered = 0
                    self.ended = False
                elif kind == "state.changed":
                    to = event.get("to") or {}
                    if to.get("state") == "finished" and isinstance(to.get("stop"), str) \
                            and to["stop"] in ANSWERED:
                        self.ended = True
                elif kind == "model.finished":
                    self.requests += 1
                    self.answered += 1
                    used = (event.get("usage") or {}).get("input_tokens")
                    if used:
                        self.inputs.append(used)
                    stop = event.get("stop")
                    self.stop = stop if isinstance(stop, str) else json.dumps(stop)
                elif kind == "permission.decided" and event.get("grant") == "deny":
                    self.refused += 1
                elif kind == "session.finished":
                    self.finished = True
                elif kind in ("model.failed", "step.failed"):
                    self.failed.append(str(event.get("error", ""))[:160])
                    self.throttled += "429" in self.failed[-1]


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
    # note: a cut is a message the session was sent and INFILE does not have, and it and a wrap-up
    # are each sent after a turn that did not end, on purpose
    cuts, wraps = carried(name)
    sent = max(0, records.sent - cuts)
    ended = records.ended
    # note: a resume with nothing to continue and no message in its log yet carries on from an
    # answer that ended, which is in its snapshot and not in its log. Read off the log alone, a
    # sweep killed after its last answer would be resumed, send nothing, and be incomplete again
    if not records.sent:
        try:
            ended = ended or json.load(open(path(name, ".resume"))).get("CONTINUE") == ""
        except (OSError, ValueError, AttributeError):
            pass
    s.update(
        expected=expected(name),
        sent=sent,
        cuts=cuts,
        requests=records.requests,
        mean=sum(records.inputs) // max(len(records.inputs), 1),
        largest=max(records.inputs, default=0),
        failed=len(records.failed),
        throttled=records.throttled,
        turn=records.answered,
        retrying=len(RETRYING.findall(prose)),
        refused=records.refused,
        quiet=int(now - changed),
        stop=records.stop,
        ended=ended,
        skipped=max(0, records.skipped - cuts - wraps),
        parts=len(glob.glob(os.path.join(SWEEPS, "old", "parts", f"{name}.*.in"))),
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
    if s["expected"] is not None and sent < s["expected"]:
        whys.append(f"{sent} of {s['expected']} messages sent")
    if not ended:
        whys.append(f"the last message has no answer that ended (last stop: {records.stop})"
                    + (f"; last failure: {records.failed[-1]}" if records.failed else ""))
    s["status"] = "incomplete" if whys else "complete"
    s["why"] = "; ".join(whys)
    return s
