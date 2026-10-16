#!/usr/bin/env python3
"""Runs what CI runs, here: every `run:` step of `.github/workflows/ci.yml`, read from the file.

usage: ci.py [JOB...] [--list] [--keep-going]

JOB is a job's id in the workflow (`lint`, `test`, `msrv`, ...); without one, every job. --list
prints the steps it would run. It stops at the first failing step unless --keep-going, and ends
with a line per step: ok or FAILED, and where its log is.

Read from the workflow rather than copied, so it cannot fall behind it:
- the workflow's, each job's and each step's `env` (`RUSTFLAGS=-D warnings`, the docs'
  `RUSTDOCFLAGS`) are set as CI sets them;
- a job's `dtolnay/rust-toolchain` pin is its toolchain: `cargo` becomes `cargo +1.95.0` in a job
  pinned to 1.95.0, and stays as it is for `stable`;
- `${{ matrix.skip }}` is empty, as on the Linux runners, which test everything;
- a job that is an action and runs nothing of its own (`deny`) is the command the action runs by default
  (`ACTIONS`), and is skipped with a note when there is none;
- `cargo package` builds into a fresh target directory of its own, since a second package of an
  unpublished version compiles the first one's tarball.

Logs go to $TMPDIR/ci/<HEAD>/.
"""

import os
import re
import subprocess
import sys
import tempfile

try:
    import yaml
except ImportError:
    sys.exit("needs PyYAML (python3 -m pip install pyyaml, or the distribution's python-yaml)")

# what a job made only of an action does, as a command
ACTIONS = {
    # the action's defaults: `arguments: --all-features`, `command: check`
    "EmbarkStudios/cargo-deny-action": "cargo deny --all-features check",
}
MATRIX = re.compile(r"\$\{\{\s*matrix\.\w+\s*\}\}")


def toolchain(job):
    for step in job.get("steps", []):
        if step.get("uses", "").startswith("dtolnay/rust-toolchain"):
            return (step.get("with") or {}).get("toolchain")
    return None


def steps(workflow, only):
    """(job, step name, command, environment) for every step to run, in the workflow's order."""
    base_env = {k: str(v) for k, v in (workflow.get("env") or {}).items()}
    for job_id, job in workflow["jobs"].items():
        if only and job_id not in only:
            continue
        env = {**base_env, **{k: str(v) for k, v in (job.get("env") or {}).items()}}
        pin = toolchain(job)
        ran = False
        for i, step in enumerate(job.get("steps", [])):
            if "run" in step:
                command = " ".join(MATRIX.sub("", step["run"]).split())
                if pin and pin != "stable":
                    command = re.sub(r"(^|&&\s*|;\s*)cargo ", rf"\1cargo +{pin} ", command)
                step_env = {k: str(v) for k, v in (step.get("env") or {}).items()}
                yield job_id, step.get("name") or f"step {i + 1}", command, {**env, **step_env}
                ran = True
            elif step.get("uses", "").split("@")[0] in ACTIONS:
                action = step["uses"].split("@")[0]
                yield job_id, action, ACTIONS[action], env
                ran = True
        if not ran:
            yield job_id, "(nothing to run here)", None, env


def main():
    args = sys.argv[1:]
    if "-h" in args or "--help" in args:
        sys.exit(__doc__)
    listing = "--list" in args
    keep_going = "--keep-going" in args
    only = [a for a in args if not a.startswith("--")]
    root = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True,
                          text=True, check=True).stdout.strip()
    workflow = yaml.safe_load(open(os.path.join(root, ".github", "workflows", "ci.yml")))
    unknown = [j for j in only if j not in workflow["jobs"]]
    if unknown:
        sys.exit(f"no job {', '.join(unknown)}; the jobs are {', '.join(workflow['jobs'])}")
    todo = list(steps(workflow, only))
    if listing:
        for job, name, command, _ in todo:
            print(f"{job:18} {name}: {command or 'skipped'}")
        return

    head = subprocess.run(["git", "-C", root, "rev-parse", "--short", "HEAD"],
                          capture_output=True, text=True).stdout.strip()
    logs = os.path.join(os.environ.get("TMPDIR", "/tmp"), "ci", head)
    os.makedirs(logs, exist_ok=True)
    results = []
    for n, (job, name, command, env) in enumerate(todo, 1):
        if command is None:
            results.append((job, name, "skipped", ""))
            continue
        e = {**os.environ, **env}
        e.pop("CARGO_TERM_COLOR", None)
        if re.search(r"\bcargo (\+\S+ )?package\b", command):
            e["CARGO_TARGET_DIR"] = tempfile.mkdtemp(prefix="package-", dir=logs)
        log = os.path.join(logs, f"{n:02}-{job}.log")
        print(f"[{n}/{len(todo)}] {job}: {name}", flush=True)
        with open(log, "w") as f:
            f.write(f"$ {command}\n")
            f.flush()
            code = subprocess.run(command, shell=True, cwd=root, env=e, stdout=f,
                                  stderr=subprocess.STDOUT).returncode
        results.append((job, name, "ok" if code == 0 else "FAILED", log))
        if code != 0 and not keep_going:
            break

    print()
    for job, name, result, log in results:
        print(f"{result:8} {job:18} {name}" + (f"  ({log})" if result == "FAILED" else ""))
    left = len(todo) - len(results)
    if left:
        print(f"not run: {left} steps after the failure; --keep-going runs them")
    sys.exit(1 if any(r[2] == "FAILED" for r in results) else 0)


if __name__ == "__main__":
    main()
