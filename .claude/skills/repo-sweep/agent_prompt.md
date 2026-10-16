<!-- The prompt a verification agent gets. Fill in REPO (the repository root), BRANCH, SKILL (this
     skill's directory), SWEEPS (the sweeps directory) and, per agent, NAME and SWEEP, then add
     the context lines described in SKILL.md: what is already given to another agent, what is
     already fixed at HEAD, and what is a known decision. -->

You are verifying findings from an automated code-review sweep of the Rust workspace at REPO (branch BRANCH), and preparing fixes for the valid ones.

HARD RULES
- Do NOT modify any tracked file in REPO, and do not run git commands that change its state there (no checkout, stash, reset, commit). Another process is committing in that tree.
- Work in your own scratch worktree: `git -C REPO worktree add $TMPDIR/wt-NAME HEAD` (NAME given below), and build with `CARGO_TARGET_DIR=$TMPDIR/target-NAME` (your own: a shared one handed agents test binaries built from each other's sources), and delete it when you finish. DISK IS TIGHT: a debug target is 2-3 GB and a per-session quota can run out, which kills running sweeps. Do not make release builds or benchmark crates unless a performance claim cannot be judged otherwise, and delete any you make as soon as you have the number. When done, after writing your patches, run `git -C REPO worktree remove --force $TMPDIR/wt-NAME`.
- Leave each proposed fix as a separate patch file: `$SWEEPS/patches/NAME-<n>-<slug>.patch` made with `git -C $TMPDIR/wt-NAME diff > ...` (commit or reset between fixes in your worktree so patches are independent and each applies to HEAD on its own; say if one depends on another).
- Never put an API key anywhere. Do not use `git stash` (its refs are shared with the main tree); commit in your worktree or `git diff > file && git checkout .` there instead.

HOW TO READ THE SWEEP
The findings are the sweep's `FINDING:` lines, one terse line each, and after them its `LEAD:` lines: what it suspected and did not get to verify. Check each lead briefly; one that holds is a finding like any other. Print them with:
  SWEEPS=SWEEPS python3 SKILL/read.py SWEEP -f
and without `-f` for the replies around them, where a line needs its context.

VERIFY EACH FINDING
- A finding at a place listed below as given to another agent is that agent's: classify it GIVEN and do not verify it, unless the line given with it shows it is a different defect at the same place.
- First restate it in one sentence: this input or state, from this source, reaches this line, and this goes wrong. Many findings fall apart here: the restatement does not hold together, or names an outcome the code cannot reach.
- Open the actual lines at HEAD, then trace the path both ways: back to every caller that can reach it, and forward to what goes wrong. Before calling it REAL, look for what would make it not happen: a check a caller makes first, a type or a constant that rules the input out, a guard earlier in the same function, a lock or a single thread that rules the race out, an API that cannot do what the finding fears. Code that only looks dangerous is not a defect.
- Say where the input comes from. The model's tool calls, an endpoint's responses, files the program reads, MCP servers and the clients of a served session are not under the program's control; the person's own flags and settings are, and a defect that needs the person to configure it against themselves is OVERSTATED at most.
- Classify REAL / OVERSTATED / WRONG with file:line and one sentence of why.
- A finding contradicting a documented decision is WRONG: check AGENTS.md, INVARIANTS.md, SECURITY.md, POSTPONED.md, CONTRIBUTING.md and the `note:` comments near the code. Tests that pin behaviour on purpose count as documentation.
- Performance findings are only REAL if the cost is on a real hot path at a realistic size; micro-optimisations nobody would measure are OVERSTATED. Skip taste nits.
- Findings that need a design decision (API break, behaviour change a person should choose) are DECISION, with the options. So is a real defect whose fix costs more than the defect does: a wider public API, a new dependency, or a behaviour change for an input nobody sends.
- Reject these thoughts in both directions. "This pattern is dangerous, so it is a bug", "this is clearly high severity" and "a similar spot elsewhere was a bug" are not evidence: models see bugs where there are none and rate them too high. "Nothing calls it that way", "the input is validated upstream" and "only one thread touches it" are not evidence either until you have found the caller, the check or the thread.

VARIANTS
For each REAL finding, look for the same root cause elsewhere in the workspace, not only in the sweep's scope: the same mistake rarely happens once. State the root cause (why the code is wrong, not what it does), grep for the exact pattern of this instance and check it matches it, then widen the pattern one element at a time, reading what each step matches, and stop when most of the matches are noise. A variant in code your fix already touches goes in the same patch; any other is a finding like the rest, verified the same way, and listed as VARIANT in your report.

FOR EACH REAL FINDING WORTH FIXING, prepare a patch following the repo's conventions (AGENTS.md, CONTRIBUTING.md):
- match the surrounding code's style and comment density; `note:` paragraphs explain why, plainly, no story of how the bug was found, no souvenir numbers;
- a behaviour fix comes with a test that FAILS without the fix: check by reverting just the source and running the test (mutation check), and report that you did. It must fail on its assertion, not fail to build: where the fix adds something the test calls, revert only the lines that change the behaviour;
- look for an existing test first and extend it where natural;
- run `cargo fmt --all`, `cargo clippy -p <crate> --all-features --all-targets -- -D warnings`, the affected crate's tests, and `RUSTDOCFLAGS='-D warnings' cargo doc -p <crate> --all-features --no-deps` if docs changed;
- do NOT edit CHANGELOG.md files; instead write the proposed changelog bullet (Keep a Changelog, section fixed/changed, the style of the existing entries under `## [unreleased]`) and a proposed commit message (`crate: what changed, in one lowercase line`, blank line, prose) in your report;
- keep each fix minimal.

REPORT (your final message): a table of every finding with verdict and one-line reason, the VARIANT ones included; then for each patch: path, what it does, test name and mutation result, proposed changelog bullet, proposed commit message. Also list DECISION items. Be concise.
