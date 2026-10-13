#!/usr/bin/env python3
"""Writes the kamchatka settings files a sweep runs under, for one model.

usage: configure.py MODEL [--limit TOKENS] [--out DIR]   (DIR defaults to $SWEEPS)

Without --limit, the model's context length is read from the endpoint's `/models` listing
(KAMCHATKA_BASE_URL, OpenRouter's shape). Where kamchatka tells the model the context is full
scales with it: half the window, or 100k, whichever is smaller. The window itself goes to
DIR/limit, which `sweep.sh` hands kamchatka as KAMCHATKA_CONTEXT_LIMIT, so that what it compacts
against is the figure the settings came from.

A listing can say more than an endpoint serves. Before taking a 400 for the window's edge, look
for a larger request that went through: one 400 after a sweep's largest request so far once looked
like the edge, and another sweep's next request was larger still and answered.
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
    "project's rules. Report only findings you have verified in the source. No style nits, no "
    "speculation, no findings about missing features the docs say are deliberately absent "
    "(POSTPONED.md)."
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
    "CODE-QUALITY findings are: error handling that loses information, a lock held across an "
    "await or blocking work inside async code, a background task never cancelled or a loop that "
    "polls, needless clones of shared content, state kept in two places that can disagree, and "
    "code that breaks an architectural rule AGENTS.md states. No formatting or naming-taste "
    "nits, no speculation, nothing POSTPONED.md says is deliberately absent."
)


TESTS = (
    f"You are a senior Rust engineer reviewing the test code of the workspace in the current "
    f"directory ({ROLE}). You have read-only tools: fs read, grep and glob. Read AGENTS.md and "
    "CONTRIBUTING.md first: they state the project's conventions, and a finding that "
    "contradicts a documented decision is not a finding. A finding is a test that would not "
    "catch what it is there to catch: an assertion that cannot fail, a test that checks less "
    "than its name or doc comment says, one that would still pass with the behaviour it is "
    "named after removed (name the production line you would delete), or an enforced claim no "
    "test pins; the scope message lists the classes to look for. Open the actual lines before "
    "claiming anything."
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
    "for its own sake, no requests for more documentation."
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
    "constant, module or wrapper with one trivial use; dead code; two test helpers that do the "
    "same thing, test setup a helper already does, and a test that checks exactly what another "
    "test already checks. A doc comment on a public item of a library crate (nachalnik, "
    "nachalnik-providers, nachalnik-mcp, nachalnik-eval) stays even where a document says the "
    "same: the API documentation is where a caller reads it. For every finding, name where the "
    "value still lives once it is gone: the other document and line, the caller that absorbs the "
    "wrapper, the test that already checks it. Not findings: "
    "anything whose removal changes behaviour, a public item of a published crate (that is an "
    "API break: report it as DECISION instead), a `note:` that says why or what it costs, a "
    "test's doc comment saying what it checks, changelog entries, and rewording for its own "
    "sake that saves nothing."
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
    "simpler redesigned (say the shape it should have); the same production logic written "
    "twice that has drifted or will drift; a special case bolted on beside a general mechanism "
    "that could absorb it; flags or booleans threaded through several layers where a type would "
    "do; a name that says something other than what the code does; a document too large to "
    "read at once that has a natural seam to split along. Not findings: formatting, naming "
    "taste, anything whose fix changes behaviour a person should choose, speculation. Open the "
    "actual lines before claiming anything, and for a split or a redesign name what moves "
    "where."
)


# what each kind looks for and the others leave, so that sweeps of several kinds over the same
# files do not each report the same defect
#
# note: duplicated production code is maintain's and duplicated test code compact's, since one
# is about what drifts and the other about what can go; prose that is wrong is docs' wherever it
# is, and prose that repeats is compact's
OWNS = {
    "audit": "a behaviour defect - code that does the wrong thing, or breaks a rule INVARIANTS.md "
    "or SECURITY.md states",
    "tests": "a test that cannot fail, checks less than its name says, or would still pass with "
    "its behaviour removed",
    "quality": "performance, error handling that loses information, misuse of locks, tasks or "
    "async, and state kept twice that can disagree",
    "docs": "prose - a document, a doc comment, a note:, a help line, a tool description, an "
    "error message - that says something the code does not do",
    "maintain": "production code written twice that has drifted or will, a file or function too "
    "large or too patched to change safely, flags threaded where a type would do, and a name "
    "that misleads",
    "compact": "what can go with nothing lost - dead code, a helper or type with one trivial use, "
    "prose that restates the signature or another document, test helpers, setup or tests that "
    "repeat each other",
}


def others(kind: str) -> str:
    """The sentence that leaves the other kinds' findings to them."""
    rest = "; ".join(f"{k}: {v}" for k, v in OWNS.items() if k != kind)
    return (
        " Other sweeps read the same files for other things, and what they look for is not a "
        "finding here even where you see it - leave it to them and spend the turn on your own "
        f"kind. They are, by kind: {rest}."
    )


# the one line a finding is, by kind: what a verifier needs to find it again and nothing more
FORMAT = {
    "audit": "SEVERITY file:line - the defect - the input or state that shows it - the fix",
    "tests": "SEVERITY file:line of the test - what it claims - why that does not hold",
    "quality": "perf|quality SEVERITY file:line - the problem - the fix",
    "docs": "SEVERITY doc file:line - code file:line - what the prose says against what the code "
    "does",
    "compact": "file:line range - what goes - where its value still lives - lines saved",
    "maintain": "size|redesign|duplication|smell SEVERITY file:line range - the problem - the "
    "change",
}


def report(kind: str) -> str:
    """How findings are written down: tersely, as they are found, in the reply text.

    note: terse and not elaborated, because a sweep's prose is what it pays for twice - once to
    write and again on every request after - and the verifier reads the code, not the argument.
    The reply text is the record because the program elides tool results and not replies, so a
    model needs nothing else to keep. It is not asked to keep notes or a context budget with the
    `context` tool: most models handle that badly, and every check is a whole request.
    """
    severity = ", where SEVERITY is high, medium or low" if "SEVERITY" in FORMAT[kind] else ""
    return (
        " REPORTING: the moment you have verified a finding, state it in your reply text (not "
        f"only in your reasoning) on one line: `FINDING: {FORMAT[kind]}`{severity}. Terse: at "
        "most 50 words a line, no elaboration, no headings, nothing restated from what you read. "
        "Then carry on exploring. A defect you have not verified in the source is not a finding. "
        "Tool results are elided when the next message arrives, and your replies are not, so "
        "your FINDING lines are your record. If the program tells you the context is full, "
        "`elide` the tool results you are done with, using the `context` "
        "tool."
    )


def listed_limit(model: str) -> int | None:
    base = os.environ.get("KAMCHATKA_BASE_URL", "https://openrouter.ai/api/v1").rstrip("/")
    key = os.environ.get("KAMCHATKA_API_KEY", "")
    # a user agent of its own: some endpoints sit behind a firewall that refuses Python's
    request = urllib.request.Request(
        f"{base}/models", headers={"Authorization": f"Bearer {key}", "User-Agent": "repo-sweep"}
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            listing = json.load(response)
    except Exception as e:  # a listing is a convenience; --limit is the way round it
        print(f"could not read {base}/models: {e}", file=sys.stderr)
        return None
    for entry in listing.get("data", []):
        if entry.get("id") == model:
            # OpenRouter says `context_length`, Vercel's AI Gateway `context_window`
            return (
                entry.get("context_length")
                or entry.get("context_window")
                or entry.get("top_provider", {}).get("context_length")
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

    # a backstop and not a budget: the model is not asked to keep to one, and kamchatka tells it
    # the context is full only past this
    ceiling = min(100_000, limit // 2)
    common = {
        "model": model,
        "requests": 0,
        "tools": ["fs", "context"],
        "allow": ["fs:read", "fs:grep", "fs:glob", "context"],
        "deny": ["fs:write", "fs:edit", "exec"],
        "on-ask": "deny",
        "compact": round(min(0.9, ceiling / limit), 3),
    }
    for kind, system in (
        ("audit", AUDIT),
        ("tests", TESTS),
        ("quality", QUALITY),
        ("docs", DOCS),
        ("compact", COMPACT),
        ("maintain", MAINTAIN),
    ):
        path = os.path.join(out, f"{kind}.json")
        with open(path, "w") as f:
            json.dump({**common, "system": system + others(kind) + report(kind)}, f, indent=1)
        print(path)
    with open(os.path.join(out, "limit"), "w") as f:
        f.write(f"{limit}\n")
    # for `status.py`, which points out a sweep that has gone far past it
    with open(os.path.join(out, "ceiling"), "w") as f:
        f.write(f"{ceiling}\n")
    print(f"context {limit:,}, full past {ceiling:,} (compact {common['compact']})",
          file=sys.stderr)


if __name__ == "__main__":
    main()
