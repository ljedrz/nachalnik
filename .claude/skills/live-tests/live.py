#!/usr/bin/env python3
"""Runs the workspace's live suites and says, per test, whether it passed, failed or skipped.

usage: live.py [SUITE...] [--retry N] [--out DIR] [--tree DIR] [--only REGEX] [--list]

SUITE is a name from `SUITES` below; without one, every suite whose variables are set. A suite
whose variables are not set is not run at all and is reported as such, with what it needs, rather
than run to skip every test. --list prints the suites and what each needs.

Each test runs as a process of its own (`--exact NAME`), one at a time within a suite, so its
output is its own: a test that exits 0 having printed a line with "skip" in it is SKIP, not ok.
The suites run at the same time, each in its own process, as CONTRIBUTING.md says they can.

--retry N runs a failed test again up to N times; one that passes on a retry is FLAKY, which
says more about the model than the code. --tree DIR runs the suites in another checkout - a
worktree at the last release tag - with a target directory of its own, to tell a regression
from a model that will not play along. --only REGEX keeps the tests whose names match it.

Writes DIR/SUITE/TEST.log for every test run and DIR/summary.txt; DIR defaults to
$TMPDIR/live/<the checkout's HEAD>. Keys come from the environment and are never written down.
"""

import concurrent.futures
import os
import re
import shutil
import subprocess
import sys
import time

OPENROUTER = "openrouter.ai"


def env(name):
    return os.environ.get(name, "")


def via_openrouter(base_var, default):
    return OPENROUTER in (env(base_var) or default)


def nachalnik_key():
    # as `nachalnik_utils::api_key`: an OpenRouter key only for OpenRouter's address
    if via_openrouter("NACHALNIK_BASE_URL", "https://openrouter.ai/api/v1"):
        return env("OPENROUTER_API_KEY") or env("NACHALNIK_API_KEY")
    return env("NACHALNIK_API_KEY")


def missing(*pairs):
    """The first requirement not met, as the sentence to report; None when all are."""
    for ok, need in pairs:
        if not ok:
            return need
    return None


def kamchatka_live():
    # note: refused rather than run when the base URL is not set, because the suite then
    # defaults to Google's endpoint and sends whatever key is set there; and when only
    # KAMCHATKA_MODEL is set, because the suite reads KAMCHATKA_TEST_MODEL and would quietly ask
    # its default model instead
    return missing(
        (env("KAMCHATKA_API_KEY") or env("NACHALNIK_API_KEY"), "KAMCHATKA_API_KEY"),
        (env("KAMCHATKA_BASE_URL"), "KAMCHATKA_BASE_URL, set even for Google's endpoint"),
        (env("KAMCHATKA_TEST_MODEL"), "KAMCHATKA_TEST_MODEL (not KAMCHATKA_MODEL)"),
    )


def kamchatka_env(e):
    # the compaction test needs a limit small enough to exceed, and a Claude model counts one
    # read of its file as more than the smaller one
    if "KAMCHATKA_CONTEXT_LIMIT" not in e:
        model = e.get("KAMCHATKA_TEST_MODEL", "").lower()
        e["KAMCHATKA_CONTEXT_LIMIT"] = "16384" if "claude" in model else "12288"
    return e


def nachalnik_env(e):
    # for an endpoint that publishes no limit; here it makes the runtime report one the endpoint
    # does not enforce, and the test of a refused request fails
    e.pop("NACHALNIK_CONTEXT_LIMIT", None)
    return e


# name: (package, test target, what it needs - None when met, the environment it runs with)
SUITES = {
    "nachalnik": ("nachalnik", "live",
                  lambda: missing((nachalnik_key(), "OPENROUTER_API_KEY or NACHALNIK_API_KEY")),
                  nachalnik_env),
    "eval": ("nachalnik-eval", "live",
             lambda: missing((nachalnik_key(), "OPENROUTER_API_KEY or NACHALNIK_API_KEY")),
             None),
    "mcp": ("nachalnik-mcp", "foreign",
            lambda: missing((nachalnik_key(), "OPENROUTER_API_KEY or NACHALNIK_API_KEY"),
                            (shutil.which("python3"), "python3")),
            None),
    "anthropic": ("nachalnik-providers", "anthropic_live",
                  lambda: missing((env("OPENROUTER_API_KEY")
                                   if via_openrouter("NACHALNIK_ANTHROPIC_BASE_URL", "")
                                   else env("ANTHROPIC_API_KEY"),
                                   "ANTHROPIC_API_KEY, or OPENROUTER_API_KEY with "
                                   "NACHALNIK_ANTHROPIC_BASE_URL at OpenRouter")),
                  None),
    "responses": ("nachalnik-providers", "responses_live",
                  lambda: missing((env("OPENROUTER_API_KEY")
                                   if via_openrouter("NACHALNIK_RESPONSES_BASE_URL", "")
                                   else env("OPENAI_API_KEY"),
                                   "OPENAI_API_KEY, or OPENROUTER_API_KEY with "
                                   "NACHALNIK_RESPONSES_BASE_URL at OpenRouter")),
                  None),
    "gemini": ("nachalnik-providers", "gemini_live",
               lambda: missing((env("GEMINI_API_KEY"), "GEMINI_API_KEY")), None),
    "system1": ("nachalnik-providers", "system1",
                lambda: missing((env("NACHALNIK_SYSTEM1_MODEL"), "NACHALNIK_SYSTEM1_MODEL"),
                                (env("OPENROUTER_API_KEY") or env("NACHALNIK_SYSTEM1_BASE_URL"),
                                 "OPENROUTER_API_KEY or NACHALNIK_SYSTEM1_BASE_URL")),
                None),
    "kamchatka": ("kamchatka", "live", kamchatka_live, kamchatka_env),
    "advise": ("kamchatka", "advise",
               lambda: missing((env("KAMCHATKA_SYSTEM1_MODEL"), "KAMCHATKA_SYSTEM1_MODEL"),
                               (env("OPENROUTER_API_KEY"), "OPENROUTER_API_KEY")),
               None),
}

SKIP = re.compile(r"\bskip", re.IGNORECASE)
IGNORED = re.compile(r"test result: ok\. 0 passed; 0 failed; 1 ignored")


def cargo(package, target, tree, target_dir, *rest):
    return ["cargo", "test", "--manifest-path", os.path.join(tree, "Cargo.toml"), "-p", package,
            "--all-features", "--test", target, *rest]


def environment(name, target_dir):
    """What a suite's cargo runs with, built and run alike, so that a variable a build reads
    cannot make the run rebuild."""
    adjust = SUITES[name][3]
    e = dict(os.environ, CARGO_TARGET_DIR=target_dir)
    return adjust(e) if adjust else e


def build(name, tree, target_dir):
    """The suite's test binary, built once before its tests run."""
    package, target, _, _ = SUITES[name]
    e = environment(name, target_dir)
    done = subprocess.run(cargo(package, target, tree, target_dir, "--no-run"), env=e,
                          capture_output=True, text=True)
    return done.returncode == 0, done.stderr


def tests(name, tree, target_dir, only):
    package, target, _, _ = SUITES[name]
    e = environment(name, target_dir)
    done = subprocess.run(cargo(package, target, tree, target_dir, "--", "--list"), env=e,
                          capture_output=True, text=True)
    names = [line[:-len(": test")] for line in done.stdout.splitlines()
             if line.endswith(": test")]
    return [n for n in names if not only or re.search(only, n)]


def run_one(name, test, tree, target_dir, log):
    package, target, _, _ = SUITES[name]
    e = environment(name, target_dir)
    start = time.monotonic()
    done = subprocess.run(cargo(package, target, tree, target_dir, "--", "--exact", test,
                                "--test-threads=1", "--nocapture"),
                          env=e, capture_output=True, text=True)
    seconds = time.monotonic() - start
    output = done.stdout + done.stderr
    with open(log, "w") as f:
        f.write(output)
    if done.returncode != 0:
        # the panic's message is the line after the one naming where it happened
        lines = output.splitlines()
        at = next((i for i, line in enumerate(lines) if "panicked at" in line), None)
        why = lines[at + 1].strip() if at is not None and at + 1 < len(lines) else ""
        return "FAIL", seconds, why
    if IGNORED.search(output):
        return "IGNORED", seconds, ""
    skip = next((line.strip() for line in output.splitlines() if SKIP.search(line)), None)
    if skip is not None:
        return "SKIP", seconds, skip
    return "ok", seconds, ""


def run_suite(name, tree, target_dir, out, retry, only):
    rows = []
    built, errors = build(name, tree, target_dir)
    if not built:
        return [(name, "-", "BUILD FAILED", 0.0, errors.strip().splitlines()[-1] if errors else "")]
    os.makedirs(os.path.join(out, name), exist_ok=True)
    for test in tests(name, tree, target_dir, only):
        log = os.path.join(out, name, test.replace("::", ".") + ".log")
        result, seconds, why = run_one(name, test, tree, target_dir, log)
        tries = 0
        while result == "FAIL" and tries < retry:
            tries += 1
            again, more, _ = run_one(name, test, tree, target_dir, f"{log[:-4]}.{tries}.log")
            seconds += more
            if again != "FAIL":
                result, why = "FLAKY", f"failed, then {again} on try {tries + 1}"
        rows.append((name, test, result, seconds, why))
    return rows


def main():
    args = sys.argv[1:]
    if "-h" in args or "--help" in args:
        sys.exit(__doc__)
    if "--list" in args:
        for name, (package, target, need, _) in SUITES.items():
            wants = need()
            print(f"{name:10} {package} --test {target}: "
                  f"{'ready' if wants is None else 'needs ' + wants}")
        return

    def option(flag, default):
        if flag in args:
            i = args.index(flag)
            value = args[i + 1]
            del args[i:i + 2]
            return value
        return default

    retry = int(option("--retry", 0))
    tree = os.path.abspath(option("--tree", os.getcwd()))
    head = subprocess.run(["git", "-C", tree, "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True).stdout.strip() or "tree"
    out = os.path.abspath(option("--out", os.path.join(os.environ.get("TMPDIR", "/tmp"),
                                                       "live", head)))
    only = option("--only", None)
    # a tree of its own builds into a target of its own, so that two checkouts never hand each
    # other binaries
    if "--tree" in sys.argv:
        target_dir = os.path.join(out, "target")
    else:
        target_dir = os.environ.get("CARGO_TARGET_DIR") or os.path.join(tree, "target")
    names = args or list(SUITES)
    unknown = [n for n in names if n not in SUITES]
    if unknown:
        sys.exit(f"no suite {', '.join(unknown)}; --list names them")

    rows, ready = [], []
    for name in names:
        need = SUITES[name][2]()
        if need is None:
            ready.append(name)
        else:
            rows.append((name, "-", "NOT RUN", 0.0, f"needs {need}"))
    os.makedirs(out, exist_ok=True)
    # note: cargo locks the target directory, so the builds take turns; the tests then run side
    # by side
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, len(ready))) as pool:
        for result in pool.map(lambda n: run_suite(n, tree, target_dir, out, retry, only),
                               ready):
            rows.extend(result)

    lines = [f"{'suite':10} {'result':12} {'secs':>6}  test  (reason)"]
    for name, test, result, seconds, why in rows:
        lines.append(f"{name:10} {result:12} {seconds:6.0f}  {test}" + (f"  ({why})" if why else ""))
    counts = {}
    for row in rows:
        counts[row[2]] = counts.get(row[2], 0) + 1
    lines.append("")
    lines.append(", ".join(f"{k} {v}" for k, v in sorted(counts.items())))
    lines.append(f"logs: {out}")
    text = "\n".join(lines)
    with open(os.path.join(out, "summary.txt"), "w") as f:
        f.write(text + "\n")
    print(text)
    sys.exit(1 if any(r[2] in ("FAIL", "BUILD FAILED") for r in rows) else 0)


if __name__ == "__main__":
    main()
