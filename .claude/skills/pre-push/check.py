#!/usr/bin/env python3
"""The mechanical half of a branch's review: what can be read off its diff and commits.

usage: check.py [BASE] [--no-semver]

BASE defaults to `master`; the branch is everything from where it left BASE to HEAD, plus what is
not committed yet. Prints what it found in three groups:

- BLOCKING: a key in an added line, a conflict marker, `dbg!`/`todo!`/`unimplemented!` left in
  library or program code, a break `cargo semver-checks` finds in a crate whose changelog has no
  `breaking` entry for it.
- LOOK AT: a commit that changes a crate's source without its changelog and does not say it is
  tests, docs or tooling only; a public enum added without `#[non_exhaustive]`; a test added with
  `#[ignore]` or a test removed; a file added, removed or renamed that MAP.md does not follow; a
  commit subject not in the `crate: what changed` form.
- FOR THE REVIEW: the tests the branch adds, each to be mutation-checked if it pins a fix.

It decides nothing about the prose or the design; SKILL.md is the rest of the review.
--no-semver skips `cargo semver-checks`, which builds the docs of each changed crate twice.
"""

import os
import re
import subprocess
import sys

PUBLISHED = ["nachalnik", "nachalnik-providers", "nachalnik-mcp", "nachalnik-eval", "kamchatka"]
SECRET = re.compile(
    r"sk-or-v1-[0-9a-f]{16,}|sk-ant-[A-Za-z0-9_-]{16,}|sk-[A-Za-z0-9_-]{32,}"
    r"|AIza[0-9A-Za-z_-]{35}|gh[pousr]_[A-Za-z0-9]{36}|-----BEGIN [A-Z ]*PRIVATE KEY-----"
)
CONFLICT = re.compile(r"^(<<<<<<<|>>>>>>>)( |$)|^=======$")
DEBUG = re.compile(r"\b(dbg|todo|unimplemented)!\s*\(")
TEST_ATTR = re.compile(r"#\[(tokio::)?test\b")
# a commit that changes nothing a caller sees says so in its message, as the house style has it:
# "No changelog entry", "Comment-only", "Tests only", "Docs and tooling only"
ONLY = re.compile(r"no changelog|\b(tests?|docs?|documentation|comments?|tooling|changelog|"
                  r"examples?)( and [a-z ]+)?[ -]only\b", re.IGNORECASE)


def git(*args):
    return subprocess.run(["git", *args], capture_output=True, text=True).stdout


def redact(text):
    return SECRET.sub(lambda m: m.group(0)[:6] + "..." + m.group(0)[-3:], text)


def crate_of(path):
    top = path.split("/", 1)[0]
    return top if top in PUBLISHED else None


def added_lines(diff):
    """(file, line number at HEAD, text) for every added line of a unified diff."""
    path, number = None, 0
    for line in diff.splitlines():
        if line.startswith("+++ "):
            path = line[6:] if line.startswith("+++ b/") else None
        elif line.startswith("@@"):
            number = int(re.search(r"\+(\d+)", line).group(1))
        elif path and line.startswith("+"):
            yield path, number, line[1:]
            number += 1
        elif path and not line.startswith("-"):
            number += 1


def removed_lines(diff):
    path = None
    for line in diff.splitlines():
        if line.startswith("--- "):
            path = line[6:] if line.startswith("--- a/") else None
        elif path and line.startswith("-") and not line.startswith("---"):
            yield path, line[1:]


def is_test_path(path):
    return "/tests/" in path or path.endswith("/tests.rs") or "/benches/" in path


def map_mentions():
    """MAP.md's text by crate section, to look a file's name up in."""
    sections, current = {}, None
    for line in open("MAP.md").read().splitlines():
        m = re.match(r"^## (\S+)", line)
        if m:
            current = m.group(1)
        if current:
            sections[current] = sections.get(current, "") + line + "\n"
    return sections


def mentioned(section, path):
    name = os.path.basename(path)
    if name in ("mod.rs", "lib.rs", "main.rs"):
        name = os.path.basename(os.path.dirname(path)) + "/"
    return f"`{name}`" in section or f"{name}`" in section or f"`{name.rstrip('/')}" in section


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if "-h" in sys.argv or "--help" in sys.argv:
        sys.exit(__doc__)
    os.chdir(git("rev-parse", "--show-toplevel").strip())
    base = args[0] if args else "master"
    fork = git("merge-base", base, "HEAD").strip()
    if not fork:
        sys.exit(f"no common ancestor with {base}")
    blocking, look, review = [], [], []

    # the branch's diff, committed and not, against where it left BASE
    diff = git("diff", "-U0", fork, "--")
    for path, number, text in added_lines(diff):
        where = f"{path}:{number}"
        if SECRET.search(text):
            blocking.append(f"{where}: what looks like a key: {redact(text.strip())[:80]}")
        if CONFLICT.match(text):
            blocking.append(f"{where}: a conflict marker")
        if path.endswith(".rs") and crate_of(path) and not is_test_path(path) \
                and DEBUG.search(text) and not text.lstrip().startswith("//"):
            blocking.append(f"{where}: {DEBUG.search(text).group(0)} left in")
        if path.endswith(".rs") and crate_of(path) and "/src/" in path \
                and re.match(r"\s*pub enum \w+", text):
            above = open(path).read().splitlines()[max(0, number - 8):number - 1]
            if not any("non_exhaustive" in a for a in above):
                look.append(f"{where}: `{text.strip()}` without `#[non_exhaustive]` - "
                            "needed unless it can never gain a variant")
        if path.endswith(".rs") and "#[ignore" in text:
            look.append(f"{where}: a test added as `#[ignore]` - say why, or run it")
        if path.endswith(".rs") and TEST_ATTR.search(text):
            source = open(path).read().splitlines()
            name = next((m.group(1) for line in source[number:number + 4]
                         if (m := re.search(r"fn (\w+)", line))), "?")
            review.append(f"{path}:{number}: `{name}`")
    removed_tests = sum(1 for path, text in removed_lines(git("diff", "-U0", fork, "--"))
                        if path.endswith(".rs") and TEST_ATTR.search(text))
    if removed_tests:
        look.append(f"{removed_tests} test attribute(s) removed - a test deleted or merged; say "
                    "which test now checks what it did")

    # changelogs, commit by commit: one that changes a crate's source says so in its changelog,
    # or says it is tests, docs or tooling only
    for line in git("log", "--format=%H %s", f"{fork}..HEAD").splitlines():
        sha, subject = line.split(" ", 1)
        body = git("show", "-s", "--format=%B", sha)
        files = git("show", "--name-only", "--format=", sha).split()
        if not re.match(r"^[\w.,/ `-]+: \S", subject):
            look.append(f"{sha[:8]} `{subject}`: not `crate: what changed`")
        elif re.match(r"^[^:]+: [A-Z][a-z]", subject):
            look.append(f"{sha[:8]} `{subject}`: the line after the colon is lowercase")
        source = {crate_of(f) for f in files
                  if crate_of(f) and "/src/" in f and f.endswith(".rs")}
        logged = {crate_of(f) for f in files if f.endswith("CHANGELOG.md") and crate_of(f)}
        missing = sorted(c for c in source - logged if c)
        if missing and not ONLY.search(body):
            look.append(f"{sha[:8]} `{subject}`: changes {', '.join(missing)} without its "
                        "changelog, and does not say it is tests, docs or tooling only")

    # MAP.md follows the files
    sections = map_mentions()
    for line in git("diff", "--name-status", "-M", fork, "--").splitlines():
        status, *paths = line.split("\t")
        crate = crate_of(paths[-1])
        if not crate or not paths[-1].endswith(".rs") or "/tests/" in paths[-1]:
            continue
        section = sections.get(crate, "")
        if status.startswith(("A", "R")) and not mentioned(section, paths[-1]):
            look.append(f"{paths[-1]}: new, and MAP.md's {crate} section does not name it")
        if status.startswith(("D", "R")) and mentioned(section, paths[0]) \
                and not os.path.exists(paths[0]):
            look.append(f"{paths[0]}: gone, and MAP.md's {crate} section still names it")

    # semver, for every published crate whose source changed
    if "--no-semver" not in sys.argv:
        changed = sorted({crate_of(p) for p in git("diff", "--name-only", fork, "--").split()
                          if crate_of(p) and "/src/" in p})
        for crate in changed:
            # note: `--release-type patch` because otherwise it reads the crate's version, and on
            # a branch that bumped it already it skips every check and passes
            done = subprocess.run(["cargo", "semver-checks", "-p", crate, "--all-features",
                                   "--baseline-rev", fork, "--release-type", "patch"],
                                  capture_output=True, text=True)
            if done.returncode == 0:
                continue
            output = done.stdout + done.stderr
            if "--- failure" not in output:
                look.append(f"{crate}: cargo semver-checks did not run: "
                            f"{output.strip().splitlines()[-1] if output.strip() else '?'}")
                continue
            failures = re.findall(r"--- failure (\w+)", output)
            changelog = open(f"{crate}/CHANGELOG.md").read()
            unreleased = re.search(r"^## \[unreleased\](.*?)(?=^## \[|\Z)", changelog,
                                   re.MULTILINE | re.DOTALL | re.IGNORECASE)
            has_breaking = unreleased and re.search(r"^### breaking", unreleased.group(1),
                                                    re.MULTILINE)
            where = "and its changelog says so" if has_breaking else "and its changelog has no " \
                "`breaking` entry"
            (look if has_breaking else blocking).append(
                f"{crate}: cargo semver-checks finds {', '.join(failures)} against {fork[:8]}, "
                f"{where}")

    commits = git("log", "--oneline", f"{fork}..HEAD").splitlines()
    dirty = git("status", "--short").strip()
    print(f"{len(commits)} commits since {base} ({fork[:8]})"
          + ("; uncommitted changes included" if dirty else ""))
    for title, items in (("BLOCKING", blocking), ("LOOK AT", look),
                         ("FOR THE REVIEW: tests added", review)):
        print(f"\n{title}" + ("" if items else ": nothing"))
        for item in items:
            print(f"- {item}")
    sys.exit(1 if blocking else 0)


if __name__ == "__main__":
    main()
