#!/usr/bin/env python3
"""Runs the sweeps: every scope of each KIND, in the order given, as many at once as the endpoint
takes, until each one is complete or has been tried `--tries` times.

usage: run.py [--width N] [--tries N] [--stall MINUTES] [--only NAME,...] [KIND...]

KIND is audit, tests, quality, docs, maintain or compact (all six, in that order, if none is
given); a scope `scopes/KIND/X.txt` runs as the sweep named `<first letter of KIND>-X`. Run it in
the background (`nohup python3 run.py ... > /dev/null 2>&1 &`) and watch it with `status.py`; it
writes what it does to $SWEEPS/run.log and where it stands to $SWEEPS/run.json.

What it decides on its own:
- **No time limit.** A sweep runs until the model has answered its last message. One whose records
  have not grown for `--stall` minutes (default 45) is stopped, which writes its session, and is
  run again.
- **How many at once.** There is no ceiling but the endpoint: kamchatka is not the limit, and a
  session takes tens of megabytes. It starts at `--width` (default 8) and climbs by what the
  endpoint answers: once that many have run for a quarter of an hour, it adds a quarter again, and
  keeps the new width only if the answers a minute rose by a tenth and no turn failed on a 429. If
  not, it goes back to the width that did as well and holds there, for half an hour and twice as
  long each time in a row, up to two hours, since an endpoint's capacity changes over a day. A 429
  kamchatka waits out is not counted, since the answers a minute already show what it costs; a turn
  that fails on one loses the rest of its message's work, which they do not show. The step is a
  quarter because a sweep started at a width too wide runs to its end, which is hours.
- **An endpoint that is down.** A sweep that ends with a turn the model never answered, or on an
  error, pauses every new start - five minutes, doubling to an hour while it keeps happening - and
  takes a quarter of the width away.
- **What counts as done**; see `sweeps.py`. One that is not is moved aside to $SWEEPS/old/ and
  run again from the start, up to `--tries` times in all (default 3).
- **Picking up where it left off.** Run again, it skips what is complete and adopts sweeps still
  running from before rather than starting them twice.
"""

import collections
import glob
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import traceback

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sweeps  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
# another directory of scopes, laid out as `scopes/` is, for trying the runner out
SCOPES = os.environ.get("SCOPES") or os.path.join(HERE, "scopes")
KINDS = ["audit", "tests", "quality", "docs", "maintain", "compact"]
TICK = 30
# how long new sweeps take to reach the endpoint, how long the answers are counted for, by how much
# a wider run has to beat a narrower one to be kept, and how long a width that did not is held at
# first; each try in a row that fails doubles the hold, up to the longest
SETTLE, WINDOW, GAIN, HOLD, LONGEST = 180, 600, 1.1, 1800, 2 * 3600


class Climb:
    """The width, moved by how many answers a minute the endpoint gives at each one, and by the turns
    it turns away for good."""

    def __init__(self, width, now):
        self.width = width
        self.changed = now
        self.full_since = None  # since when exactly `width` sweeps have run
        self.base = None  # (width, answers a minute) before the latest widening
        self.hold_until = 0.0
        self.hold = HOLD  # how long the next width that does not pay is held
        self.series = collections.deque()  # (when, answers so far)
        # turns failed on a 429: so far, and at the last move, or at the first look, so that what
        # sweeps adopted from a run before met there is not taken for this width's doing
        self.throttled, self.throttled_at = 0, None

    def look(self, now, answered, throttled, running, waiting):
        """Takes the answers so far, the turns failed on a 429 so far, how many sweeps run and how
        many could start; returns what it changed, as a line for the log, or None."""
        self.series.append((now, answered))
        while self.series and now - self.series[0][0] > 2 * (SETTLE + WINDOW + HOLD):
            self.series.popleft()
        self.throttled = throttled
        if self.throttled_at is None:
            self.throttled_at = throttled
        # note: a turn that fails on a 429 loses the rest of its message's work, which the answers a
        # minute do not show, so one is enough to step back. Not while more run than the width:
        # those started at a width already left, and a sweep is never stopped to make room
        if running > self.width:
            self.throttled_at = throttled
        if throttled > self.throttled_at:
            failed, was = throttled - self.throttled_at, self.width
            if self.base and self.width > self.base[0]:
                self.width = self.base[0]
            else:
                self.width = max(1, was - max(1, was // 4))
            self.throttled_at = throttled
            return f"width {was} -> {self.width}: {failed} turn(s) failed on a 429; " + self.held(now)
        # note: measured only while exactly `width` run. Fewer is a run short of work or paused, and
        # more is one draining after a narrowing, and neither says what the width does
        if running != self.width:
            self.full_since = None
            return None
        if self.full_since is None:
            self.full_since = now
        start = max(self.changed, self.full_since) + SETTLE
        if not waiting or now - start < WINDOW:
            return None
        t0, n0 = next(((t, n) for t, n in self.series if t >= start), (now, answered))
        if now - t0 < WINDOW / 2:
            return None
        rate = (answered - n0) / (now - t0) * 60
        if self.base and self.width > self.base[0] and rate < self.base[1] * GAIN:
            was, (self.width, before) = self.width, self.base
            return (f"width {was} -> {self.width}: {rate:.1f} answers a minute against {before:.1f}"
                    f" at {self.width}; " + self.held(now))
        if now < self.hold_until:
            return None
        if self.base:  # the widening before this one paid
            self.hold = HOLD
        was = self.width
        self.base, self.changed = (was, rate), now
        self.width = min(was + max(1, was // 4), running + waiting)
        return f"width {was} -> {self.width}: {rate:.1f} answers a minute at {was}"

    def held(self, now):
        """Holds the width it has just moved to, and says for how long."""
        self.base, self.changed, self.hold_until = None, now, now + self.hold
        line = f"holding for {self.hold // 60} minutes"
        self.hold = min(2 * self.hold, LONGEST)
        return line

    def narrow(self, now):
        """A quarter less, held for a while, for an endpoint that has stopped answering."""
        was = self.width
        self.width = max(1, was - max(1, was // 4))
        self.held(now)
        self.throttled_at = self.throttled
        return was


def option(args, flag, default, kind=int):
    if flag in args:
        at = args.index(flag)
        value = kind(args[at + 1])
        del args[at : at + 2]
        return value
    return default


def log(line):
    stamp = time.strftime("%H:%M:%S")
    with open(os.path.join(sweeps.SWEEPS, "run.log"), "a") as f:
        f.write(f"{stamp} {line}\n")


def earlier(name):
    """How many tries of this sweep there have been: the one in place, and those kept in old/."""
    kept = len(glob.glob(os.path.join(sweeps.SWEEPS, "old", f"{name}.*.in")))
    return kept + os.path.exists(sweeps.path(name, ".in"))


def aside(name):
    """Moves a sweep's files to old/, numbered, so a fresh run starts from nothing, and says how
    many tries are kept there."""
    old = os.path.join(sweeps.SWEEPS, "old")
    os.makedirs(old, exist_ok=True)
    n = 1
    while glob.glob(os.path.join(old, f"{name}.{n}.*")):
        n += 1
    for suffix in (".in", ".jsonl", ".err", ".pid"):
        p = sweeps.path(name, suffix)
        if os.path.exists(p):
            os.rename(p, os.path.join(old, f"{name}.{n}{suffix}"))
    return len(glob.glob(os.path.join(old, f"{name}.*.in")))


def signal_(pid, sig):
    try:
        os.kill(pid, sig)
    except ProcessLookupError:
        pass


def main():
    args = sys.argv[1:]
    if "-h" in args or "--help" in args:
        sys.exit(__doc__)
    width = option(args, "--width", 8)
    tries = option(args, "--tries", 3)
    stall = option(args, "--stall", 45) * 60
    only = option(args, "--only", None, lambda s: set(s.split(",")))
    kinds = args or KINDS
    for kind in kinds:
        if kind not in KINDS:
            sys.exit(f"{kind}: KIND is one of {', '.join(KINDS)}")

    lock = os.path.join(sweeps.SWEEPS, "run.pid")
    try:
        other = int(open(lock).read())
        if other != os.getpid() and sweeps.alive(other):
            sys.exit(f"run.py is already running as {other}")
    except (FileNotFoundError, ValueError):
        pass
    with open(lock, "w") as f:
        f.write(str(os.getpid()))

    # the tree and the binary every sweep of this run uses, made once and kept for its length; see
    # sweep.sh. Removed by hand once the run is over and read: `git worktree remove $SWEEPS/tree`
    repo = subprocess.run(["git", "-C", HERE, "rev-parse", "--show-toplevel"],
                          capture_output=True, text=True, check=True).stdout.strip()
    tree = os.path.join(sweeps.SWEEPS, "tree")
    if not os.path.isdir(tree):
        subprocess.run(["git", "-C", repo, "worktree", "add", "--detach", tree, "HEAD"],
                       capture_output=True, check=True)
    binary = os.path.join(sweeps.SWEEPS, "kamchatka")
    if not os.path.exists(binary):
        built = os.path.join(os.environ.get("CARGO_TARGET_DIR") or os.path.join(repo, "target"),
                             "release", "kamchatka")
        if not os.path.exists(built):
            sys.exit(f"no {built}: cargo build --release -p kamchatka first")
        shutil.copy2(built, binary)
    pinned = subprocess.run(["git", "-C", tree, "rev-parse", "--short", "HEAD"],
                            capture_output=True, text=True).stdout.strip()

    plan = []  # (name, kind, scope), in the order they run
    for kind in kinds:
        for scope in sorted(glob.glob(os.path.join(SCOPES, kind, "*.txt"))):
            name = f"{kind[0]}-{os.path.splitext(os.path.basename(scope))[0]}"
            if only is None or name in only:
                plan.append((name, kind, scope))
    of = {name: (kind, scope) for name, kind, scope in plan}

    queue = collections.deque()
    running = {}  # name -> when it was started, or adopted
    done, given_up = [], []
    for name, _, _ in plan:
        s = sweeps.state(name)
        if s["status"] == "complete":
            continue
        if s["status"] == "running":
            running[name] = time.time()
            log(f"adopted {name}, still running from before")
        elif s["status"] != "queued" and earlier(name) >= tries:
            given_up.append(name)
        else:
            queue.append(name)
    log(f"start: {len(queue)} to run, {len(running)} running, width {width}, "
        f"reading {tree} at {pinned}")

    seen = collections.Counter()  # the answers counted from each sweep's current records
    seen_throttled = collections.Counter()  # and the turns failed on a 429
    answered = throttled = 0
    climb = Climb(width, time.time())
    paused_until, streak = 0.0, 0
    try:  # a pause a run before this one was in the middle of still stands
        paused_until = json.load(open(os.path.join(sweeps.SWEEPS, "run.json")))["paused_until"]
    except (FileNotFoundError, ValueError, KeyError):
        pass
    stopping = {}  # name -> when it was asked to stop for stalling

    def save():
        with open(os.path.join(sweeps.SWEEPS, "run.json"), "w") as f:
            json.dump({"pid": os.getpid(), "width": climb.width,
                       "paused_until": paused_until, "queued": list(queue),
                       "running": sorted(running), "done": done, "given_up": given_up,
                       "at": time.time()}, f, indent=1)

    def tick():
        """One look at every sweep: what has ended, what the endpoint answers, what to start."""
        nonlocal paused_until, streak, answered, throttled
        now = time.time()
        states = {name: sweeps.state(name, now) for name in of}

        for name in list(running):
            s = states[name]
            # a sweep only just started may not have its files yet
            if s["status"] in ("queued", "lost") and now - running[name] < 60:
                continue
            if s["status"] == "running":
                if s["quiet"] > stall and name not in stopping:
                    log(f"{name}: no records for {s['quiet'] // 60} minutes; stopping it")
                    signal_(s["pid"], signal.SIGTERM)
                    stopping[name] = now
                elif name in stopping and now - stopping[name] > 300:
                    log(f"{name}: still there five minutes after being stopped; killing it")
                    signal_(s["pid"], signal.SIGKILL)
                    stopping[name] = now
                continue
            del running[name]
            stopping.pop(name, None)
            if s["status"] == "complete":
                streak = 0
                done.append(name)
                log(f"{name}: complete, {s['requests']} requests")
                continue
            log(f"{name}: incomplete ({s['why']})")
            # what says the endpoint is not serving: a turn it never answered, or a session that
            # ended on an error
            if s.get("empty") or not s.get("finished") and s.get("exit") not in (124, 143):
                streak += 1
                pause = min(3600, 300 * 2 ** (streak - 1))
                paused_until = now + pause
                was = climb.narrow(now)
                log(f"width {was} -> {climb.width}: {name} ended on a failure")
                log(f"pausing new starts for {pause // 60} minutes")
            if earlier(name) < tries:
                queue.append(name)
            else:
                given_up.append(name)
                log(f"{name}: given up after {tries} tries")

        # every answer the endpoint has given this run, counted as each sweep's records grow; a
        # sweep resumed into a new log, or moved aside to run again, starts its count again
        for name, s in states.items():
            now_seen = s.get("requests", 0)
            answered += now_seen - seen[name] if now_seen >= seen[name] else now_seen
            seen[name] = now_seen
            now_seen = s.get("throttled", 0)
            throttled += now_seen - seen_throttled[name] if now_seen >= seen_throttled[name] \
                else now_seen
            seen_throttled[name] = now_seen
        waiting = len(queue) if now >= paused_until else 0
        moved = climb.look(now, answered, throttled, len(running), waiting)
        if moved:
            log(moved)

        while queue and len(running) < climb.width and time.time() >= paused_until:
            name = queue.popleft()
            kind, scope = of[name]
            # the tries so far are what is kept in old/, so a restarted run counts them too
            tried = aside(name) + 1
            seen[name] = seen_throttled[name] = 0
            subprocess.Popen([os.path.join(HERE, "sweep.sh"), kind, name, scope],
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, start_new_session=True,
                             env={**os.environ, "TREE": tree, "BIN": binary})
            running[name] = time.time()
            log(f"started {name} (try {tried}), {len(running)} running")
            time.sleep(5)  # so a batch does not arrive at the endpoint in one burst

    while queue or running:
        try:
            tick()
        except Exception:  # noqa: BLE001 - a run is hours long, and one bad read must not end it
            log("a check failed, and the run goes on:\n" + traceback.format_exc())
        save()
        time.sleep(TICK)

    save()
    log(f"finished: {len(done)} complete, {len(given_up)} given up {given_up}")
    os.remove(lock)


if __name__ == "__main__":
    main()
