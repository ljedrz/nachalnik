#!/usr/bin/env python3
"""Writes the kamchatka settings files a sweep runs under, for one model.

usage: configure.py MODEL [--limit TOKENS] [--out DIR]   (DIR defaults to $SWEEPS)

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


DOCS = (
    f"You are a senior Rust engineer checking the documentation of the workspace in the current "
    f"directory ({ROLE}) against the code it describes. You have read-only tools: fs read, grep "
    "and glob. Read AGENTS.md first: its conventions section is the house style, and a finding "
    "that contradicts a documented decision is not a finding. A defect is prose - a Markdown "
    "file, a doc comment, a `note:` paragraph, a help line, a tool description a model reads, "
    "an error message, a changelog entry - that says something the code does not do: a default, "
    "a limit, a flag, a key, a file, a function, a type, a test or a behaviour that is named "
    "wrongly, has changed, or no longer exists; two documents that contradict each other; a "
    "link or intra-doc reference that points nowhere. For every claim you check, open the code "
    "that decides it and quote the line. Style findings count only where they break a rule "
    "AGENTS.md states (counting the repository, captures of the program's output, prose that "
    "talks about itself, a synonym for a mechanism's one word); no taste nits, no rewording "
    "for its own sake, no requests for more documentation. Each finding: severity "
    "(high/medium/low: high is a claim that would make a reader do the wrong thing), the "
    "document's file:line, the code's file:line, what the prose says, what the code does, and "
    "the minimal correction. When you are done exploring, end with a section titled FINDINGS "
    "listing everything, most important first."
)


COMPACT = (
    f"You are a senior Rust engineer looking for what the workspace in the current directory "
    f"({ROLE}) can do without. You have read-only tools: fs read, grep and glob. Read AGENTS.md "
    "first: its conventions section is the house style, and its rule that the prose argues "
    "plainly is the standard here - keep the fact, the consequence, and the clause that stops "
    "somebody undoing it by mistake; cut the story of how something was found, the souvenir "
    "number, the alternative weighed and dropped, the closing line that restates the opening, "
    "the sentence that talks about the text instead of saying it. A finding is something that "
    "can go, or be merged into something else, with NO loss of value and NO change in what the "
    "program does: a paragraph or doc comment that restates the signature above it, another "
    "document, or another paragraph; two documents saying one thing where one could point to the "
    "other; a `note:` that tells a story rather than a reason; a type, function, trait, "
    "constant, module or wrapper with one trivial use; two near-identical functions or test "
    "helpers; dead code; a test that checks exactly what another test already checks. For "
    "every finding, name where the value still lives once it is gone: the other document and "
    "line, the caller that absorbs the wrapper, the test that already checks it. Not findings: "
    "anything whose removal changes behaviour, a public item of a published crate (that is an "
    "API break: report it as DECISION instead), a `note:` that says why or what it costs, a "
    "test's doc comment saying what it checks, changelog entries, and rewording for its own "
    "sake that saves nothing. Each finding: kind (docs/code/tests), file:line range, what "
    "goes, what replaces it if anything, where the value still lives, and roughly how many "
    "lines it saves. When you are done exploring, end with a section titled FINDINGS listing "
    "everything, largest saving first."
)


MAINTAIN = (
    f"You are a senior Rust engineer doing a maintainability review of the workspace in the "
    f"current directory ({ROLE}). You have read-only tools: fs read, grep and glob. Read "
    "AGENTS.md and CONTRIBUTING.md first: they state the project's conventions, and a finding "
    "that contradicts a documented decision (or POSTPONED.md) is not a finding. The question is "
    "what makes this code or prose harder than it needs to be to change safely, for a person or "
    "for a model reading it in pieces. Findings are: a file too large to read and hold at once "
    "that has a natural seam to split along (name the seam and the pieces); a function that "
    "does several unrelated things or has grown branches patched on over time and would be "
    "simpler redesigned (say the shape it should have); the same logic written twice that has "
    "drifted or will drift; a special case bolted on beside a general mechanism that could "
    "absorb it; flags or booleans threaded through several layers where a type would do; "
    "dead code; a name that says something other than what the code does; a comment that "
    "contradicts the code or narrates history; documentation that is out of date, repeats "
    "another document, or has ballooned past what a reader needs (AGENTS.md: the prose argues "
    "plainly). Not findings: formatting, naming taste, anything whose fix changes behaviour a "
    "person should choose, speculation. Open the actual lines before claiming anything, and "
    "for a split or a redesign say concretely what moves where and what gets simpler. Each "
    "finding: kind (size/redesign/duplication/smell/docs), severity (high/medium/low), "
    "file:line range, the problem in one sentence, why it matters concretely, and the minimal "
    "change. When you are done exploring, end with a section titled FINDINGS listing "
    "everything, most important first."
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
        f"a reason, until it is back under {n}. "
        "WHAT THE PROGRAM DOES ON ITS OWN: when the next message arrives, every tool result "
        "from before it - each file you read, each search you ran - is elided and left as a "
        "marker saying it was compacted after you had read it, and every follow-up message you "
        "are sent starts such a pass. So: compare two places in the turn you read them, and "
        "`note` each verified finding at once, with file:line and the exact words it rests on; "
        "a note is never elided. Before a reply with no tool call, which ends the turn, `note` "
        "anything still unnoted. When a marker stands for something you still need, `restore` "
        "that item by id with a reason, or `search` it for the line you need, rather than "
        "reading the file again: a restored item is not elided again, and a fresh read is at "
        "the next message. `look` lists every item with its id, elided ones included."
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
    out = args[args.index("--out") + 1] if "--out" in args else os.environ.get(
        "SWEEPS", os.path.join(os.environ.get("TMPDIR", "/tmp"), "sweeps")
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
    for kind, system in (
        ("audit", AUDIT),
        ("quality", QUALITY),
        ("docs", DOCS),
        ("compact", COMPACT),
        ("maintain", MAINTAIN),
    ):
        path = os.path.join(out, f"{kind}.json")
        with open(path, "w") as f:
            json.dump({**common, "system": system + hygiene(budget)}, f, indent=1)
        print(path)
    print(f"context {limit:,}, budget {budget:,}, compact at {common['compact']}", file=sys.stderr)


if __name__ == "__main__":
    main()
