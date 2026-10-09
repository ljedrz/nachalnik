#!/usr/bin/env python3
"""Where the sweeps stand: one line each, then what needs looking at.

usage: status.py [--watch MINUTES] [NAME...]     (every sweep in $SWEEPS if none is named)

With --watch it checks once a minute and returns as soon as a sweep ends, something new needs
looking at, or `run.py` stops - or after MINUTES with the table as it is. Run it in the
background and be woken by it; that is the check `run.py` cannot do for itself: reading what a
sweep found and deciding whether it went wrong.

Needs looking at:
- refused: the model's tool calls keep being refused - most often arguments nested one level too
  deep, or a model calling tools the sweep does not have. The sweep is spending its turns on
  nothing; read NAME.err.
- quiet: running with no records for twenty minutes. `run.py` stops it at its `--stall`.
- a long turn: over 300 requests in one turn. Read the end of NAME.err: a model reading its scope
  file after file is working; one making the same calls over and over is not, and nothing stops
  it - what to do about it is the person's call, not `run.py`'s.
- turns failed, a sweep given up, or `run.py` no longer running with work left.
"""

import glob
import json
import os
import re
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sweeps  # noqa: E402

# a turn this long may be a model that has stopped finishing turns at all: nothing else would end
# it, since a sweep has no time limit and a model going round in circles still writes records
LONG_TURN = 300


def kind(trouble):
    """What a line of trouble is about, without the figures in it.

    note: a figure in it changes while it stands - minutes quiet, requests in a turn, calls
    refused - and compared whole, the same trouble a minute later was new trouble, so `--watch`
    returned every minute it lasted.
    """
    return re.sub(r"\d[\d,]*", "N", trouble)


def names():
    return sorted(os.path.basename(p)[:-3] for p in glob.glob(os.path.join(sweeps.SWEEPS, "*.in")))


def run_state():
    try:
        run = json.load(open(os.path.join(sweeps.SWEEPS, "run.json")))
    except (FileNotFoundError, ValueError):
        return None
    run["alive"] = sweeps.alive(run["pid"])
    return run


def concerns(s):
    found = []
    if s["status"] == "running" and s["quiet"] > 1200:
        found.append(f"quiet for {s['quiet'] // 60} minutes")
    if s["status"] == "running" and s.get("turn", 0) > LONG_TURN:
        found.append(f"{s['turn']} requests in its current turn")
    if s.get("refused", 0) > 20:
        found.append(f"{s['refused']} calls refused")
    if s.get("empty"):
        found.append(f"{s['empty']} turn(s) failed with nothing answered")
    if s.get("failed", 0) > s.get("empty", 0):
        found.append(f"{s['failed'] - s['empty']} turn(s) failed part-way, and it went on")
    return found


def table(named):
    now = time.time()
    states = [sweeps.state(name, now) for name in named]
    lines, trouble = [], []
    run = run_state()
    if run:
        queued = len(run["queued"])
        paused = max(0, int(run["paused_until"] - now))
        lines.append(
            f"run.py {'running' if run['alive'] else 'NOT RUNNING'}: width {run['width']}, "
            f"{len(run['running'])} running, {queued} queued, "
            f"{len(run['done'])} complete, given up {run['given_up'] or 'none'}"
            + (f", starts paused for {paused // 60} more minutes" if paused else "")
        )
        if not run["alive"] and (queued or run["running"]):
            trouble.append("run.py is not running and has work left")
        for name in run["given_up"]:
            trouble.append(f"{name}: given up")
    for s in states:
        if s["status"] == "queued":
            continue
        lines.append(
            f"{s['name']:24} {s['status']:10} {s['sent']}/{s['expected']} sent, "
            f"{s['requests']} requests (mean {s['mean']:,}, max {s['largest']:,}), "
            f"{s['retrying']} retries, quiet {s['quiet'] // 60}m"
            + (f" - {s['why']}" if s["why"] else "")
        )
        trouble += [f"{s['name']}: {c}" for c in concerns(s)]
    return states, lines, trouble


def main():
    args = sys.argv[1:]
    watch = None
    if "--watch" in args:
        at = args.index("--watch")
        watch = float(args[at + 1]) * 60
        del args[at : at + 2]
    named = args or names()

    states, lines, trouble = table(named)
    if watch is None:
        print("\n".join(lines + [f"LOOK: {t}" for t in trouble]))
        return

    begun, known = time.time(), {kind(t) for t in trouble}
    ended = {s["name"] for s in states if s["status"] in ("complete", "incomplete", "lost")}
    while time.time() - begun < watch:
        time.sleep(60)
        states, lines, trouble = table(args or names())
        now_ended = {s["name"] for s in states if s["status"] in ("complete", "incomplete", "lost")}
        new = [t for t in trouble if kind(t) not in known]
        run = run_state()
        if now_ended - ended or new or (run and not run["alive"]):
            break
    else:
        now_ended, new = ended, []
    print("\n".join(lines))
    for name in sorted(now_ended - ended):
        print(f"ENDED: {name}")
    for t in trouble:
        print(f"LOOK: {t}" + (" (new)" if t in new else ""))


if __name__ == "__main__":
    main()
