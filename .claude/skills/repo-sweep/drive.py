#!/usr/bin/env python3
"""Feeds one headless kamchatka session its lines, a turn at a time, and carries a turn the
endpoint failed on with `/continue` rather than going on to the next message. `sweep.sh`,
`friction.sh` and `kill.sh` run it.

usage: drive.py NAME INFILE COMMAND...

COMMAND is the kamchatka command line. Under $DRIVE_OUT ($SWEEPS unless set) it writes NAME.pid,
the session's process id while it runs; NAME.jsonl, the session's records (its standard output,
appended to); NAME.err, its prose, appended to, with a line `drive: ...` for each decision made
here. Exits with the session's status, or 128 and the signal that ended it.

A command line (`/params ...`) is sent at once. A message, or a `/continue`, is sent and its turn
waited for, read off the records:
- a `finished` state whose stop is `end_turn` (or `refusal`, which there is nothing to continue
  from) is an answer, and the next line goes;
- a turn that failed (`model.failed` or `step.failed`, then `idle`) gets `/continue` after a wait,
  `DRIVE_WAIT` seconds (60) doubling to `DRIVE_LONGEST` (600), counted only while the turns fail
  with nothing answered;
- a turn cut off or interrupted mid-answer gets `/continue` after `DRIVE_WAIT`, and one paused at
  its request ceiling gets it at once;
- a message that starts no turn within `DRIVE_START` seconds (60) ends the session: kamchatka
  passes messages over once a ceiling or the context limit is reached.
After `DRIVE_TRIES` (10) failed turns in a row with nothing answered, or `DRIVE_TURNS` (40) turns
for one message, or `DRIVE_PAUSES` (100) pauses at the request ceiling, it closes the input and the
session ends. With `DRIVE_RESUMES` set, it then resumes the session from its snapshot that many
times at most, after a wait doubling from `DRIVE_LONGEST`: the commands again, `/continue`, and
the lines not yet delivered. `run.py` leaves it unset and resumes a sweep itself, keeping each
part.

With `DRIVE_CUT` set (tokens), a turn paused at its request ceiling whose last request was at
least that large, or one that failed for a request over the model's limit, and that has another
message after it, is cut rather than carried on: the session is sent `CARRY`, a message of its
own, and every tool result before it is elided as any new message elides them, while what the
model wrote stays. Over the limit, kamchatka passes messages over, so the results are excluded
first with `/exclude tool_results` and the message is `OVERRUN`. After `DRIVE_CUTS` (3) cuts it is
wrapped up instead: taken as answered, so that the next line - a sweep's request for its
findings - goes. Each cut and each wrap-up is a line, `carry` or `wrap`, in NAME.carried, since
the records cannot tell such a message from one in INFILE.

note: a cut and not a stop, because a stop loses what the model had not yet written down, and a
cut loses nothing: its replies stay whole, an elided result is a marker it can `restore` by id,
and what a lead in progress costs is reading one file again.

note: piped all at once, as `sweep.sh` used to, every line after a failed turn was a new message:
the turn's work stopped wherever the endpoint refused it, the message that would have finished it
was spent, and a sweep could use up all its continuations on 429s without an answer between them.
headless reads a line only between turns, so the order was right but nothing waited to see how
the turn had ended. A `/continue` sends the next request with the history as it is.

note: the records are read from NAME.jsonl as kamchatka writes it, not through a pipe, so that this
process holds only the session's input. Killed, it closes that, and kamchatka finishes the turn it
is in and writes its session as it would at the end of any input.
"""

import json
import os
import re
import signal
import subprocess
import sys
import time

OUT = os.environ.get("DRIVE_OUT") or os.environ.get("SWEEPS") \
    or os.path.join(os.environ.get("TMPDIR", "/tmp"), "sweeps")
WAIT = float(os.environ.get("DRIVE_WAIT", 60))
LONGEST = float(os.environ.get("DRIVE_LONGEST", 600))
TRIES = int(os.environ.get("DRIVE_TRIES", 10))
TURNS = int(os.environ.get("DRIVE_TURNS", 40))
START = float(os.environ.get("DRIVE_START", 60))
RESUMES = int(os.environ.get("DRIVE_RESUMES", 0))
PAUSES = int(os.environ.get("DRIVE_PAUSES", 100))
CUT = int(os.environ.get("DRIVE_CUT", 0))
CUTS = int(os.environ.get("DRIVE_CUTS", 3))
# what a cut sends; `sweeps.CARRIED`, how both start, is how a snapshot's copy is known
CARRY = (
    "The context passed its ceiling, so the tool results you had read are now elided: each stands "
    "as a marker you can `restore` by id with the `context` tool. Your FINDING lines stand; do "
    "not repeat them. Carry on with the scope where you left off: first finish what you were in "
    "the middle of, restoring only what it needs, then the parts not yet read."
)
# what a cut sends when the request was already over the model's limit, which kamchatka sends no
# message past; `/exclude tool_results` goes first, being a command, which it still reads
OVERRUN = (
    "The context passed its ceiling and the model's limit, so every tool result so far is now "
    "excluded to make room: `look` with the `context` tool lists them by id, and `restore` brings "
    "back one you need. Your FINDING lines stand; do not repeat them. Carry on with the scope "
    "where you left off: first finish what you were in the middle of, restoring only what it "
    "needs, then the parts not yet read."
)
# the stops after which there is nothing to continue: `/continue` refuses both
ENDED = {"end_turn", "refusal"}
# how kamchatka names the snapshot a session can be resumed from, as it leaves
SNAPSHOT = re.compile(r"`kamchatka -r ([^`]+)` carries on")


def command(line):
    return line.startswith("/") and line.split()[0] != "/continue"


class Records:
    """The session's records, read as they are written, from where the file ended when it began."""

    def __init__(self, path):
        self.path = path
        self.offset = os.path.getsize(path)

    def new(self):
        with open(self.path, "rb") as f:
            f.seek(self.offset)
            for raw in f:
                if not raw.endswith(b"\n"):
                    break  # a record still being written
                self.offset += len(raw)
                try:
                    yield json.loads(raw)["event"]
                except (ValueError, KeyError, TypeError):
                    continue


class Session:
    """One kamchatka process."""

    def __init__(self, name, argv):
        self.pid_file = os.path.join(OUT, name + ".pid")
        self.err_path = os.path.join(OUT, name + ".err")
        self.err = open(self.err_path, "a", buffering=1)
        self.err_from = self.err.tell()
        out = open(os.path.join(OUT, name + ".jsonl"), "a")
        self.records = Records(out.name)
        self.name = name
        self.cuts = 0
        self.last_input = 0  # how large the latest request was, in tokens
        self.overrun = False  # whether the latest turn failed for a request over the limit
        self.proc = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=out, stderr=self.err,
                                     text=True)
        out.close()
        with open(self.pid_file, "w") as f:
            f.write(str(self.proc.pid))

    def say(self, line):
        self.err.write(f"drive: {line}\n")

    def send(self, line):
        """False once the session is gone."""
        try:
            self.proc.stdin.write(line + "\n")
            self.proc.stdin.flush()
            return True
        except (BrokenPipeError, ValueError):
            return False

    def gone(self):
        return self.proc.poll() is not None

    def turn(self):
        """Waits for the turn the line just sent starts and ends; returns how it ended and how many
        answers it got: `answer`, `failed`, `cut`, `paused`, `none` (no turn started) or `exited`."""
        sent, started, failed, answers = time.monotonic(), False, False, 0
        self.overrun = False
        while True:
            for event in self.records.new():
                kind = event.get("event")
                if kind == "model.finished":
                    answers += 1
                    self.last_input = (event.get("usage") or {}).get("input_tokens") or 0
                elif kind in ("model.failed", "step.failed"):
                    failed = True
                    self.overrun = bool(event.get("overrun"))
                elif kind == "turn.paused":
                    return "paused", answers
                elif kind == "state.changed":
                    to = event.get("to") or {}
                    if to.get("state") == "requesting":
                        started = True
                    elif to.get("state") == "finished" and started:
                        stop = to.get("stop")
                        return ("answer" if isinstance(stop, str) and stop in ENDED else "cut"), \
                            answers
                    # note: only after a failure. A turn running tools passes through `idle` on
                    # its way from `executing` back to `requesting`
                    elif to.get("state") == "idle" and failed:
                        return "failed", answers
            if self.gone():
                return "exited", answers
            if not started and time.monotonic() - sent > START:
                return "none", answers
            time.sleep(1)

    def carried(self, how):
        with open(os.path.join(OUT, self.name + ".carried"), "a") as f:
            f.write(how + "\n")

    def deliver(self, line, more=False):
        """Sends one message (or `/continue`) and carries its turn on until it ends in an answer.
        Returns `answered`, `left` (given up on, the message in the context), `none` (it started
        no turn) or `exited`. With `more`, a message follows it, and a turn past `CUT` may be cut
        or wrapped up (see above)."""
        wait, fails, turns, pauses = WAIT, 0, 0, 0
        while True:
            if not self.send(line):
                return "exited"
            how, answers = self.turn()
            # note: a pause is the request ceiling doing its job, not a turn going wrong, and an
            # exploration of a large scope pauses dozens of times
            if how == "paused":
                pauses += 1
            else:
                turns += 1
            if how == "answer":
                return "answered"
            if how == "exited":
                return "exited"
            if how == "none":
                if line == "/continue":
                    return "answered"  # nothing was waiting for an answer
                self.say("the message started no turn; ending the session")
                return "none"
            if turns >= TURNS or pauses >= PAUSES:
                self.say(f"{turns} turns and {pauses} pauses for one message; ending the session")
                return "left"
            # note: a turn whose request was too large to send is cut as well: every `/continue`
            # would fail on the same request, and only a new message makes it smaller
            over = (how == "paused" and CUT and self.last_input >= CUT) \
                or (how == "failed" and self.overrun)
            if over and more:
                what = ("a request too large to send" if how == "failed"
                        else f"a request of {self.last_input:,} tokens")
                # note: over the limit, kamchatka sends no message, and a command still goes
                if how == "failed" and not self.send("/exclude tool_results"):
                    return "exited"
                if self.cuts >= CUTS:
                    self.say(f"{what} after {self.cuts} cuts; wrapped up, and the next message "
                             "goes")
                    self.carried("wrap")
                    return "answered"
                self.cuts += 1
                self.say(f"{what}; cut {self.cuts} of {CUTS}")
                self.carried("carry")
                line = OVERRUN if how == "failed" else CARRY
                continue
            if how == "failed":
                # a failed turn that got answers made progress, and starts the count again
                if answers:
                    fails, wait = 0, WAIT
                else:
                    fails += 1
                if fails >= TRIES:
                    self.say(f"{fails} failed turns in a row with nothing answered; ending the "
                             "session")
                    return "left"
                pause = wait
                wait = min(2 * wait, LONGEST)
            elif how == "cut":
                pause = WAIT
            else:  # paused at the request ceiling
                pause = 0
            self.say(f"the turn ended {how}, after {answers} answer(s); /continue in {pause:.0f}s")
            line = "/continue"
            ends = time.monotonic() + pause
            while time.monotonic() < ends:
                if self.gone():
                    return "exited"
                time.sleep(1)

    def snapshot(self):
        """The snapshot this process said it can be resumed from, as it left."""
        with open(self.err_path, errors="replace") as f:
            f.seek(self.err_from)
            found = SNAPSHOT.findall(f.read())
        return found[-1] if found and os.path.exists(found[-1]) else None

    def end(self):
        try:
            self.proc.stdin.close()
        except BrokenPipeError:
            pass
        code = self.proc.wait()
        os.remove(self.pid_file)
        return 128 - code if code < 0 else code


def main():
    if len(sys.argv) < 4:
        sys.exit(__doc__)
    name, infile, argv = sys.argv[1], sys.argv[2], sys.argv[3:]
    with open(infile) as f:
        lines = [line.rstrip("\n") for line in f if line.strip()]
    os.makedirs(OUT, exist_ok=True)
    open(os.path.join(OUT, name + ".jsonl"), "a").close()

    session, terminated = None, False

    def terminate(*_):
        # a SIGTERM here is for the session: passed on, it ends as kamchatka ends on one, and no
        # resume follows
        nonlocal terminated
        terminated = True
        if session:
            session.proc.send_signal(signal.SIGTERM)

    signal.signal(signal.SIGTERM, terminate)
    # the first line not yet in the session's context, and whether the last one in it is waiting
    # for `/continue`
    at, waiting, resumes, pause = 0, False, 0, LONGEST
    snapshot = None
    while True:
        session = Session(name, argv + (["-r", snapshot] if snapshot else []))
        # (index in `lines`, line); a line sent again on a resume has no index of its own
        todo = [(None, line) for line in lines[:at] if command(line)] if snapshot else []
        todo += [(None, "/continue")] if waiting else []
        todo += list(enumerate(lines))[at:]
        how = "answered"
        try:
            for k, (i, line) in enumerate(todo):
                if command(line):
                    if not session.send(line):
                        how = "exited"
                        break
                    at = i + 1 if i is not None else at
                    continue
                more = any(not command(rest) for _, rest in todo[k + 1:])
                how = session.deliver(line, more)
                if how in ("answered", "left", "exited") and i is not None:
                    at = i + 1
                waiting = how in ("left", "exited")
                if how != "answered":
                    break
        finally:
            code = session.end()
        if how != "left" or resumes >= RESUMES or terminated:
            break
        snapshot = session.snapshot()
        if not snapshot:
            session.say("no snapshot to resume from")
            break
        resumes += 1
        session.say(f"resuming from {snapshot} in {pause:.0f}s ({resumes} of {RESUMES})")
        ends = time.monotonic() + pause
        while time.monotonic() < ends and not terminated:
            time.sleep(1)
        if terminated:
            break
        pause *= 2
    sys.exit(code)


if __name__ == "__main__":
    main()
