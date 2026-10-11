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
for one message, it closes the input and the session ends. With `DRIVE_RESUMES` set, it then
resumes the session from its snapshot that many times at most, after a wait doubling from
`DRIVE_LONGEST`: the commands again, `/continue`, and the lines not yet delivered. `run.py` leaves
it unset and resumes a sweep itself, keeping each part.

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
        while True:
            for event in self.records.new():
                kind = event.get("event")
                if kind == "model.finished":
                    answers += 1
                elif kind in ("model.failed", "step.failed"):
                    failed = True
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

    def deliver(self, line):
        """Sends one message (or `/continue`) and carries its turn on until it ends in an answer.
        Returns `answered`, `left` (given up on, the message in the context), `none` (it started
        no turn) or `exited`."""
        wait, fails, turns = WAIT, 0, 0
        while True:
            if not self.send(line):
                return "exited"
            how, answers = self.turn()
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
            if turns >= TURNS:
                self.say(f"{turns} turns for one message; ending the session")
                return "left"
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
            for i, line in todo:
                if command(line):
                    if not session.send(line):
                        how = "exited"
                        break
                    at = i + 1 if i is not None else at
                    continue
                how = session.deliver(line)
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
