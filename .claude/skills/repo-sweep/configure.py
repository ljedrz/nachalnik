#!/usr/bin/env python3
"""Writes the two kamchatka settings files a sweep runs under, for one model.

usage: configure.py MODEL [--limit TOKENS] [--out DIR]

Without --limit, the model's context length is read from the endpoint's `/models` listing
(KAMCHATKA_BASE_URL, OpenRouter's shape). The budget the prompts hold the model to, and the
compaction backstop, scale with it: a sweep keeps its next request under half the window, or
100k, whichever is smaller.
"""

import json
import os
import sys
import urllib.request

ROLE = (
    "a Cargo workspace: nachalnik = agent runtime core, nachalnik-providers, nachalnik-mcp, "
    "nachalnik-eval, kamchatka = terminal agent"
)

AUDIT = (
    f"You are a senior Rust reviewer auditing the workspace in the current directory ({ROLE}). "
    "You have read-only tools: fs read, grep and glob. Read the relevant code thoroughly before "
    "claiming anything; open the actual lines. Read AGENTS.md and INVARIANTS.md first for the "
    "project's rules. Report only findings you have verified in the source, each with: a severity "
    "(high/medium/low), file:line, a one-sentence statement of the defect, a concrete failure "
    "scenario (inputs/state -> wrong outcome), and a suggested minimal fix. No style nits, no "
    "speculation, no findings about missing features the docs say are deliberately absent "
    "(POSTPONED.md). When you are done exploring, end with a section titled FINDINGS listing "
    "everything, most severe first."
)

QUALITY = (
    f"You are a senior Rust engineer doing a performance and code-quality review of the workspace "
    f"in the current directory ({ROLE}). You have read-only tools: fs read, grep and glob. Read "
    "AGENTS.md and CONTRIBUTING.md first: they state the project's conventions, and a finding "
    "that contradicts a documented decision is not a finding. Open the actual lines before "
    "claiming anything. PERFORMANCE findings must name the hot path (per frame, per keystroke, "
    "per streamed chunk, per event, per step, per item), the scaling (for example O(n^2) in "
    "context items, a full copy of every message per frame), and a realistic size at which it "
    "costs something noticeable; skip micro-optimisations nobody would measure. Note that "
    "kamchatka's terminal loop redraws only when something changed, not at a fixed frame rate. "
    "CODE-QUALITY findings are: duplicated logic that has drifted or will drift, dead code, "
    "functions doing several unrelated things, names or messages that say something other than "
    "what the code does, comments that contradict the code, error handling that loses "
    "information, needless clones of shared content, and abstractions with a single trivial use. "
    "No formatting or naming-taste nits, no speculation, nothing POSTPONED.md says is "
    "deliberately absent. Each finding: kind (perf/quality), severity (high/medium/low), "
    "file:line, the problem in one sentence, why it matters concretely, and a minimal suggested "
    "change. When you are done exploring, end with a section titled FINDINGS listing everything, "
    "most important first."
)


def hygiene(budget: int) -> str:
    n = f"{budget:,}"
    return (
        " CONTEXT HYGIENE, which is part of the job: you have the `context` tool, and you keep "
        "your own context healthy with it at all times. Check `context` with action `budget` "
        f"every few tool calls. Keep the next request under about {n} tokens: whenever it is "
        "above that, or whenever you have finished with a file or a search result, first write "
        "down anything you will still need with action `note` (the verified finding with "
        "file:line, or the fact you learnt), then put the finished tool results away with action "
        "`elide` (or `exclude` for ones that are pure noise), giving a reason. Never put away "
        "something whose finding you have not written down; a finding you did not note is lost. "
        "`search` reads a line of an elided item back without restoring it, and `restore` "
        "returns one if you must. Your notes are what you write the final report from. HARD "
        "RULE: call `context` with action `budget` after every third tool call, without "
        f"exception. If the next request is above {n} tokens, cleaning up comes before anything "
        "else: first `note` what you still need (the verified finding with file:line, or the "
        "fact you learnt), then `elide` the finished tool results (or `exclude` pure noise) with "
        f"a reason, until it is back under {n}."
    )


def listed_limit(model: str) -> int | None:
    base = os.environ.get("KAMCHATKA_BASE_URL", "https://openrouter.ai/api/v1").rstrip("/")
    key = os.environ.get("KAMCHATKA_API_KEY", "")
    request = urllib.request.Request(f"{base}/models", headers={"Authorization": f"Bearer {key}"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            listing = json.load(response)
    except Exception as e:  # a listing is a convenience; --limit is the way round it
        print(f"could not read {base}/models: {e}", file=sys.stderr)
        return None
    for entry in listing.get("data", []):
        if entry.get("id") == model:
            return entry.get("context_length") or entry.get("top_provider", {}).get(
                "context_length"
            )
    return None


def main() -> None:
    args = sys.argv[1:]
    if not args or args[0].startswith("-"):
        sys.exit(__doc__)
    model = args[0]
    limit = int(args[args.index("--limit") + 1]) if "--limit" in args else listed_limit(model)
    if not limit:
        sys.exit(f"no context length for {model}; pass --limit TOKENS")
    out = args[args.index("--out") + 1] if "--out" in args else os.path.join(
        os.environ.get("TMPDIR", "/tmp"), "sweeps"
    )
    os.makedirs(out, exist_ok=True)

    budget = min(100_000, limit // 2)
    common = {
        "model": model,
        "requests": 0,
        "tools": ["fs", "context"],
        "allow": ["fs:read", "fs:grep", "fs:glob", "context"],
        "deny": ["fs:write", "fs:edit", "exec"],
        "on-ask": "deny",
        # the backstop, if the model does not clean up after itself: a little past the budget
        "compact": round(min(0.9, budget * 1.5 / limit), 3),
    }
    for kind, system in (("audit", AUDIT), ("quality", QUALITY)):
        path = os.path.join(out, f"{kind}.json")
        with open(path, "w") as f:
            json.dump({**common, "system": system + hygiene(budget)}, f, indent=1)
        print(path)
    print(f"context {limit:,}, budget {budget:,}, compact at {common['compact']}", file=sys.stderr)


if __name__ == "__main__":
    main()
