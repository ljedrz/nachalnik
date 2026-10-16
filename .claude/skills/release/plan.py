#!/usr/bin/env python3
"""What a release would have to do, crate by crate: read off the tree, the tags and the changelogs.

usage: plan.py

For each published crate, in the order they depend on each other: its version, its last tag,
what its changelog's unreleased section holds (by section, with a count of entries), the commits
since that tag that touch it, and the workspace crates that depend on it and so may have to move
with it. It decides nothing - which number moves is `cargo semver-checks` and the changelog's
call, see SKILL.md - but it lists what there is to decide, and anything already out of order: an
unreleased heading spelt another way, a version that is not the last tag's and has no section of
its own, commits with no changelog entry.
"""

import json
import os
import re
import subprocess
import sys


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True).stdout.strip()


def packages():
    """The published members, each with the members it depends on (not as dev-dependencies)."""
    meta = json.loads(subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                                     capture_output=True, text=True, check=True).stdout)
    out = {}
    for p in meta["packages"]:
        # `publish = false` is an empty list; the unpublished `nachalnik-utils` is one
        if p.get("publish") == []:
            continue
        deps = sorted({d["name"] for d in p["dependencies"]
                       if d.get("path") and d.get("kind") is None})
        out[p["name"]] = {"version": p["version"], "deps": deps,
                          "dir": os.path.dirname(p["manifest_path"])}
    return out


def ordered(pkgs):
    """Dependencies before what depends on them."""
    done, order = set(), []

    def visit(name):
        if name in done or name not in pkgs:
            return
        done.add(name)
        for dep in pkgs[name]["deps"]:
            visit(dep)
        order.append(name)

    for name in sorted(pkgs):
        visit(name)
    return order


def last_tag(name):
    tags = git("tag", "--list", f"{name}-v*", "--sort=-version:refname").splitlines()
    return tags[0] if tags else None


def unreleased(changelog):
    """The unreleased section's heading as written, and its entries by subsection."""
    text = open(changelog).read()
    m = re.search(r"^## \[(unreleased)\][^\n]*\n(.*?)(?=^## \[|\Z)", text,
                  re.MULTILINE | re.DOTALL | re.IGNORECASE)
    if not m:
        return None, {}
    sections, current = {}, "(none)"
    for line in m.group(2).splitlines():
        if line.startswith("### "):
            current = line[4:].strip()
        elif line.startswith("- "):
            sections[current] = sections.get(current, 0) + 1
    return m.group(1), sections


def released(changelog, version):
    return re.search(rf"^## \[{re.escape(version)}\]", open(changelog).read(), re.MULTILINE)


def main():
    if len(sys.argv) > 1:
        sys.exit(__doc__)
    root = git("rev-parse", "--show-toplevel")
    os.chdir(root)
    pkgs = packages()
    dependents = {n: sorted(m for m, p in pkgs.items() if n in p["deps"]) for n in pkgs}
    warnings = []
    for name in ordered(pkgs):
        p = pkgs[name]
        rel = os.path.relpath(p["dir"], root)
        changelog = os.path.join(rel, "CHANGELOG.md")
        tag = last_tag(name)
        heading, sections = unreleased(changelog)
        commits = git("log", "--oneline", f"{tag}..HEAD", "--", rel).splitlines() if tag else []
        print(f"== {name} {p['version']}  (last tag {tag or 'none'})")
        print(f"   depends on: {', '.join(p['deps']) or '-'};"
              f" depended on by: {', '.join(dependents[name]) or '-'}")
        if sections:
            print("   unreleased: " + ", ".join(f"{k} {v}" for k, v in sections.items()))
        else:
            print("   unreleased: nothing")
        print(f"   commits since the tag touching {rel}/: {len(commits)}")
        for line in commits[:15]:
            print(f"     {line}")
        if len(commits) > 15:
            print(f"     ... and {len(commits) - 15} more")
        if heading and heading != "unreleased":
            warnings.append(f"{changelog}: the heading is `[{heading}]`; the others use "
                            "`[unreleased]`")
        if commits and not sections:
            warnings.append(f"{name}: {len(commits)} commits since {tag} and nothing unreleased "
                            "- tests, docs or tooling only, or an entry is missing")
        if tag and tag != f"{name}-v{p['version']}" and not released(changelog, p["version"]):
            warnings.append(f"{name}: at {p['version']}, not the last tag's version, with no "
                            "section of its own yet - a bump made early, which the release "
                            "keeps")
    if warnings:
        print("\nlook at:")
        for w in warnings:
            print(f"- {w}")
    workspace = git("tag", "--list", "v*", "--sort=-version:refname").splitlines()
    print(f"\nlast workspace tag: {workspace[0] if workspace else 'none'}")


if __name__ == "__main__":
    main()
