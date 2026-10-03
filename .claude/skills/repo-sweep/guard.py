#!/usr/bin/env python3
"""Watch one cargo-mutants run and keep it from taking the machine down with it.

    guard.py PID ROOT LOG [--min-avail-gib N] [--max-procs N] [--max-rss-gib N]

PID is the cargo-mutants process, or several separated by commas (the shards of one run), ROOT
the directory its copies live under (mutants.sh's $MUTANTS/mt), or a prefix of the shards'
worktrees (shards.sh's $REPO/w). A process belongs to the run when its executable, working directory or command
line is under ROOT. Every second:

- a process of the run that init or the user's systemd has adopted, and that is older than
  `--orphan-age` seconds, is an orphan - a mutated `kamchatka --headless` that outlived its test
  ignores SIGTERM - and is SIGKILLed. Not sooner: a test that leaves a command running and then
  checks the program ended it would pass under a mutant that does not, if this ended it first;
- a process whose executable was built under ROOT (a test binary, a mutated program) with more
  resident memory than the per-process ceiling is SIGKILLed alone: a mutant that allocates
  without end (`undo` always saying there is more to undo, under a loop that undoes until there
  is not) fails its test that way, and is caught, instead of taking the run down. The compiler
  and cargo are not built under ROOT, so a large build is never touched;
- when MemAvailable falls under the floor, or the run holds more processes than the ceiling,
  the top consumers are logged and the whole run, PID included, is SIGSTOPped, SIGKILLed and
  swept again until nothing has been left for three seconds (two minutes at most) - by path,
  and by the process groups the run had, which hold what of it no longer looks like it.
  A `--iterate` run afterwards picks up where it stopped, once `tainted.py --strip` has undone
  what the kill recorded.

A process cap (`ulimit -u`) bounds how many processes a runaway makes, not how much memory they
take: a thousand start-ups of the program are enough to exhaust RAM. This bounds the memory.
Exits when every PID has.
"""
import argparse
import os
import signal
import time


def mem_available():
    with open("/proc/meminfo") as f:
        for line in f:
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) * 1024
    return 0


def info(pid):
    try:
        with open(f"/proc/{pid}/stat") as f:
            stat = f.read()
        # the command name is in parentheses and may contain anything; fields follow the last ')'
        rest = stat[stat.rindex(")") + 2:].split()
        ppid = int(rest[1])
        group = int(rest[2])
        # dead already, and waiting for a parent to read it: nothing a signal can do
        if rest[0] == "Z":
            return None
        started = int(rest[19]) / os.sysconf("SC_CLK_TCK")
        rss = int(rest[21]) * os.sysconf("SC_PAGE_SIZE")
    except (OSError, ValueError, IndexError):
        return None
    paths = []
    for link in ("exe", "cwd"):
        try:
            paths.append(os.readlink(f"/proc/{pid}/{link}"))
        except OSError:
            paths.append("")
    try:
        with open(f"/proc/{pid}/cmdline", "rb") as f:
            cmd = f.read().replace(b"\0", b" ").decode(errors="replace").strip()
    except OSError:
        cmd = ""
    return ppid, rss, paths, cmd, started, group


def scan(root, uid, groups=()):
    """The processes of the run: under ROOT, or in one of `groups`, the process groups it had
    when it was killed."""
    run = {}
    me = os.getpid()
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        pid = int(name)
        if pid == me:
            continue
        try:
            if os.stat(f"/proc/{pid}").st_uid != uid:
                continue
        except OSError:
            continue
        i = info(pid)
        if i is None:
            continue
        ppid, rss, paths, cmd, started, group = i
        if any(p.startswith(root) for p in paths) or root in cmd or group in groups:
            run[pid] = (ppid, rss, cmd, paths[0].startswith(root), started, group)
    return run


def adopted(ppid):
    if ppid <= 1:
        return True
    try:
        with open(f"/proc/{ppid}/comm") as f:
            return f.read().strip() == "systemd"
    except OSError:
        return False


def kill(pid):
    try:
        os.kill(pid, signal.SIGKILL)
    except OSError:
        pass


def stop(pid):
    try:
        os.kill(pid, signal.SIGSTOP)
    except OSError:
        pass


def descends(pid, pids):
    """Whether `pid` is one of `pids` or was started under one of them."""
    for _ in range(64):
        if pid in pids:
            return True
        i = info(pid)
        if i is None or i[0] <= 1:
            return False
        pid = i[0]
    return False


def signal_groups(groups, sig):
    for group in groups:
        try:
            os.killpg(group, sig)
        except OSError:
            pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pid")
    ap.add_argument("root")
    ap.add_argument("log")
    ap.add_argument("--min-avail-gib", type=float, default=10)
    ap.add_argument("--max-procs", type=int, default=4000)
    ap.add_argument("--max-rss-gib", type=float, default=8)
    ap.add_argument("--orphan-age", type=float, default=600)
    a = ap.parse_args()
    pids = [int(p) for p in a.pid.split(",")]
    # a prefix of several directories is not one to resolve
    root = os.path.realpath(a.root) if os.path.isdir(a.root) else a.root
    uid = os.getuid()
    floor = a.min_avail_gib * 2**30
    ceiling = a.max_rss_gib * 2**30
    log = open(a.log, "a", buffering=1)

    def say(msg):
        log.write(time.strftime("%H:%M:%S ") + msg + "\n")

    say(f"guarding {a.pid} under {root}: floor {a.min_avail_gib} GiB, ceiling {a.max_procs}")
    peak = 0
    while any(os.path.exists(f"/proc/{p}") for p in pids):
        run = scan(root, uid)
        peak = max(peak, len(run))
        with open("/proc/uptime") as f:
            now = float(f.read().split()[0])
        for pid, (ppid, rss, cmd, built, started, _) in list(run.items()):
            if adopted(ppid) and now - started > a.orphan_age:
                say(f"orphan {pid} (parent {ppid}): {cmd[:200]}")
                kill(pid)
                del run[pid]
            elif built and rss > ceiling:
                say(f"over {a.max_rss_gib} GiB: {rss / 2**30:.1f} GiB in {pid}: {cmd[:200]}")
                kill(pid)
                del run[pid]
        avail = mem_available()
        if avail < floor or len(run) > a.max_procs:
            say(f"EMERGENCY: {avail / 2**30:.1f} GiB available, {len(run)} processes in the run")
            top = sorted(run.items(), key=lambda kv: -kv[1][1])[:15]
            for pid, (_, rss, cmd, _, _, _) in top:
                say(f"  {rss / 2**20:8.0f} MiB  {pid}  {cmd[:200]}")
            # frozen before any of it is killed, and swept until it stays empty, by path and by
            # process group: `Sandbox::argv` emptied made the probe at a session's start start
            # a session, which probed again; ten half-second sweeps by path left something of it
            # in the group of its `cargo test`, and three minutes later it was 14601 processes.
            # cargo-mutants gives every build and test a group of its own. Only those groups, the
            # ones led by a process under the PIDs given: anything can mention ROOT on its
            # command line, and a group is a terminal's job or a whole shell
            mine = os.getpgrp()
            groups = {g for g in {v[5] for v in run.values()} - {mine} if descends(g, pids)}
            for p in pids:
                stop(p)
            signal_groups(groups, signal.SIGSTOP)
            for _ in range(3):
                for pid in scan(root, uid, groups):
                    stop(pid)
            signal_groups(groups, signal.SIGKILL)
            for p in pids:
                kill(p)
            quiet = 0
            for _ in range(240):
                left = scan(root, uid, groups)
                for pid in left:
                    stop(pid)
                signal_groups(groups, signal.SIGKILL)
                for pid in left:
                    kill(pid)
                quiet = 0 if left else quiet + 1
                if quiet >= 6:
                    break
                time.sleep(0.5)
            left = scan(root, uid, groups)
            say(
                f"killed the run and {len(groups)} process groups"
                + (f"; {len(left)} processes still in it" if left else "")
            )
            return 1
        time.sleep(1)
    # the run is over; what is left under the copies is left over
    for pid, (_, _, cmd, _, _, _) in scan(root, uid).items():
        say(f"left over {pid}: {cmd[:200]}")
        kill(pid)
    say(f"run {a.pid} ended; peak {peak} processes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
