#!/usr/bin/env python3
"""Where sweeps found the same thing: the places their findings name, grouped by file and nearby
line, so that a place two sweeps name is verified once.

usage: dedupe.py [NAME...]          the places more than one sweep names, and how many
       dedupe.py --give NAME        the places NAME names that a sweep given out before it does

A place is any `file:line` (or `file:start-end`) in a sweep's replies and notes, read as `read.py`
reads them; a path given in part (`kernel/mod.rs`) is taken for the one tracked file it ends.
Places in one file less than `NEAR` lines apart are one place, and `file:a-b,c-d` is two.

With --give, the lines printed go into the verification agent's prompt (`agent_prompt.md`), and
NAME is added to $SWEEPS/given, the sweeps given out so far, one a line: give each sweep out with
it as its agent is started, and the next agent is told what the ones before it already have.

note: grouped by place rather than by what the findings say, because two sweeps word one defect
differently and name the same lines. A place two sweeps name can still be two defects, which is
why the agent is given the other sweep's line and told to skip only what is the same.
"""

import collections
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import read  # noqa: E402
import status  # noqa: E402
import sweeps  # noqa: E402

NEAR = 5
# a range wider than this names a region rather than a place, and would make one place of
# everything in the file; it stands for its first line
WIDEST = 30
PLACE = re.compile(
    r"([A-Za-z0-9_./-]+\.(?:rs|md|toml|yml|yaml|sh|py|html|json))"
    r"(?::|#L)(\d+(?:\s*[-–]\s*L?\d+)?(?:,\s*\d+(?:\s*[-–]\s*\d+)?)*)"
)
SPAN = re.compile(r"(\d+)(?:\s*[-–]\s*L?(\d+))?")


def tracked():
    """The files a place can name: those of the commit the run reads, or of the repository."""
    tree = os.path.join(sweeps.SWEEPS, "tree")
    if not os.path.isdir(tree):
        tree = os.path.dirname(os.path.abspath(__file__))
    top = subprocess.run(["git", "-C", tree, "rev-parse", "--show-toplevel"],
                         capture_output=True, text=True).stdout.strip()
    out = subprocess.run(["git", "-C", top, "ls-files"], capture_output=True, text=True).stdout
    return out.split()


def resolver(files):
    exact = set(files)
    cache = {}

    def resolve(path):
        path = path.lstrip("./")
        if path in exact:
            return path
        if path not in cache:
            ends = [f for f in files if f.endswith("/" + path)]
            cache[path] = ends[0] if len(ends) == 1 else path
        return cache[path]

    return resolve


def places(name, resolve):
    """Every place NAME's findings name, as (file, start, end, the line naming it)."""
    found = set()
    for heading, said in read.entries(name):
        if heading is None or not said:
            continue
        for line in said.splitlines():
            for m in PLACE.finditer(line):
                # `mod.rs:233-244,278-282` is two places
                for span in SPAN.finditer(m.group(2)):
                    start = int(span.group(1))
                    end = max(start, int(span.group(2) or start))
                    if end - start > WIDEST:
                        end = start
                    found.add((resolve(m.group(1)), start, end, line.strip()[:240]))
    return found


def clusters(by_sweep):
    """Groups of places, one file each, as lists of (start, end, sweep, line)."""
    by_file = collections.defaultdict(list)
    for name, found in by_sweep.items():
        for path, start, end, line in found:
            by_file[path].append((start, end, name, line))
    groups = []
    for path, found in sorted(by_file.items()):
        group, reach = [], None
        for place in sorted(found):
            if group and place[0] > reach + NEAR:
                groups.append((path, group))
                group = []
            reach = place[1] if not group else max(reach, place[1])
            group.append(place)
        groups.append((path, group))
    return groups


def given_path():
    return os.path.join(sweeps.SWEEPS, "given")


def main():
    args = sys.argv[1:]
    resolve = resolver(tracked())
    if args[:1] == ["--give"]:
        if len(args) != 2:
            sys.exit(__doc__)
        name = args[1]
        try:
            given = open(given_path()).read().split()
        except FileNotFoundError:
            given = []
        before = [n for n in given if n != name]
        by_sweep = {n: places(n, resolve) for n in before + [name]}
        for path, group in clusters(by_sweep):
            if not any(n == name for _, _, n, _ in group):
                continue
            theirs = [(s, e, n, line) for s, e, n, line in group if n != name]
            for s, e, n, line in theirs[:3]:
                print(f"- {path}:{s}" + (f"-{e}" if e != s else "") + f", given with {n}: {line}")
        if name not in given:
            with open(given_path(), "a") as f:
                f.write(name + "\n")
        return
    named = args or status.names()
    by_sweep = {n: places(n, resolve) for n in named}
    groups = clusters(by_sweep)
    shared = [(p, g) for p, g in groups if len({n for _, _, n, _ in g}) > 1]
    for path, group in shared:
        start, end = group[0][0], max(e for _, e, _, _ in group)
        print(f"{path}:{start}-{end}  {', '.join(sorted({n for _, _, n, _ in group}))}")
        for s, _, n, line in group:
            print(f"    {n}: {line}")
    print(f"{len(groups)} places in {sum(1 for f in by_sweep.values() if f)} sweeps; "
          f"{len(shared)} named by more than one sweep", file=sys.stderr)


if __name__ == "__main__":
    main()
