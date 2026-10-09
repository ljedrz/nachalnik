#!/usr/bin/env python3
"""Runs the sweeps: every scope of each KIND, in the order given, as many at once as the endpoint
takes, until each one is complete or has been tried `--tries` times.

usage: run.py [--width N] [--max N] [--tries N] [--stall MINUTES] [--only NAME,...] [KIND...]

KIND is audit, tests, quality, docs, maintain or compact (all six, in that order, if none is
given); a scope `scopes/KIND/X.txt` runs as the sweep named `<first letter of KIND>-X`. Run it in
the background (`nohup python3 run.py ... > /dev/null 2>&1 &`) and watch it with `status.py`; it
writes what it does to $SWEEPS/run.log and where it stands to $SWEEPS/run.json.

What it decides on its own:
- **No time limit.** A sweep runs until the model has answered its last message. One whose records
  have not grown for `--stall` minutes (default 45) is stopped, which writes its session, and is
  run again.
- **How many at once.** It starts at `--width` (default 4) and moves between 1 and `--max`
  (default 10) by what kamchatka prints while it waits on the endpoint (`answered 429; trying
  again`, a timeout, a refused connection): more than three of those per running sweep in ten
  minutes takes one away, none for twenty minutes adds one back.
- **An endpoint that is down.** A sweep that ends with a turn the model never answered, or on an
  error, pauses every new start - five minutes, doubling to an hour while it keeps happening - and
  takes one away.
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
    width = option(args, "--width", 4)
    most = option(args, "--max", 10)
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
    log(f"start: {len(queue)} to run, {len(running)} running, width {width} (max {most}), "
        f"reading {tree} at {pinned}")

    pressure = collections.deque()  # (when, retry lines printed by every sweep so far)
    seen = collections.Counter()  # retry lines read from each sweep's current files
    printed = 0
    # when the width last moved: as though long ago, so that a flood of retries at the start is
    # answered at once
    changed = time.time() - 1200
    paused_until, streak = 0.0, 0
    try:  # a pause a run before this one was in the middle of still stands
        paused_until = json.load(open(os.path.join(sweeps.SWEEPS, "run.json")))["paused_until"]
    except (FileNotFoundError, ValueError, KeyError):
        pass
    stopping = {}  # name -> when it was asked to stop for stalling

    def save():
        with open(os.path.join(sweeps.SWEEPS, "run.json"), "w") as f:
            json.dump({"pid": os.getpid(), "width": width, "max": most,
                       "paused_until": paused_until, "queued": list(queue),
                       "running": sorted(running), "done": done, "given_up": given_up,
                       "at": time.time()}, f, indent=1)

    def narrow(why):
        nonlocal width, changed
        if width > 1:
            width -= 1
            log(f"width {width + 1} -> {width}: {why}")
        changed = time.time()

    def tick():
        """One look at every sweep: what has ended, how hard the endpoint pushes back, what to start."""
        nonlocal width, changed, paused_until, streak, printed
        now = time.time()
        states = {name: sweeps.state(name, now) for name in of}
        busy = len(running)  # before this check takes away what has ended

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
            if s.get("empty") or s.get("exit") not in (0, 124, 143):
                streak += 1
                pause = min(3600, 300 * 2 ** (streak - 1))
                paused_until = now + pause
                narrow(f"{name} ended on a failure")
                log(f"pausing new starts for {pause // 60} minutes")
            if earlier(name) < tries:
                queue.append(name)
            else:
                given_up.append(name)
                log(f"{name}: given up after {tries} tries")

        # how hard the endpoint is pushing back: the retry lines every sweep has printed, over time
        # counted as it grows, so that a sweep moved aside to run again takes nothing back
        for name, s in states.items():
            now_seen = s.get("retrying", 0)
            printed += max(0, now_seen - seen[name])
            seen[name] = now_seen
        pressure.append((now, printed))
        # kept back to the latest look at least twenty minutes old, which is what widening compares
        while len(pressure) > 1 and now - pressure[1][0] >= 1200:
            pressure.popleft()
        recent = printed - next((n for t, n in pressure if now - t <= 600), printed)
        if recent > 3 * max(1, busy) and now - changed > 600:
            narrow(f"{recent} retries in ten minutes across {busy} running")
        elif pressure[0][1] == printed and now - pressure[0][0] >= 1200 and now - changed > 1200 \
                and queue and len(running) >= width and width < most:
            width += 1
            changed = now
            log(f"width {width - 1} -> {width}: no retries for twenty minutes")

        while queue and len(running) < width and time.time() >= paused_until:
            name = queue.popleft()
            kind, scope = of[name]
            # the tries so far are what is kept in old/, so a restarted run counts them too
            tried = aside(name) + 1
            seen[name] = 0
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
