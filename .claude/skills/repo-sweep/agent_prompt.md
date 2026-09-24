<!-- The prompt a verification agent gets. Fill in REPO (the repository root), BRANCH, SKILL (this
     skill's directory), SWEEPS (the sweeps directory) and, per agent, NAME and SWEEP, then add
     the context lines described in SKILL.md: what is already fixed at HEAD, and what is a known
     decision. -->

You are verifying findings from an automated code-review sweep of the Rust workspace at REPO (branch BRANCH), and preparing fixes for the valid ones.

HARD RULES
- Do NOT modify any tracked file in REPO, and do not run git commands that change its state there (no checkout, stash, reset, commit). Another process is committing in that tree.
- Work in your own scratch worktree: `git -C REPO worktree add $TMPDIR/wt-NAME HEAD` (NAME given below), and build with `CARGO_TARGET_DIR=$TMPDIR/target-NAME` (your own: a shared one handed agents test binaries built from each other's sources), and delete it when you finish. DISK IS TIGHT: a debug target is 2-3 GB and a per-session quota can run out, which kills running sweeps. Do not make release builds or benchmark crates unless a performance claim cannot be judged otherwise, and delete any you make as soon as you have the number. When done, after writing your patches, run `git -C REPO worktree remove --force $TMPDIR/wt-NAME`.
- Leave each proposed fix as a separate patch file: `$SWEEPS/patches/NAME-<n>-<slug>.patch` made with `git -C $TMPDIR/wt-NAME diff > ...` (commit or reset between fixes in your worktree so patches are independent and each applies to HEAD on its own; say if one depends on another).
- Never put an API key anywhere. Do not use `git stash` (its refs are shared with the main tree); commit in your worktree or `git diff > file && git checkout .` there instead.

HOW TO READ THE SWEEP
If a file $SWEEPS/SWEEP.notes.txt exists, the sweep was cut short and its findings are only in that file (the model's notes, recovered from its call arguments; there is no final report). Otherwise:
The findings are in the model's `context` notes and final reply. Print them with:
  python3 SKILL/extract.py "$(SKILL/snap.sh SWEEP)" -n
(the notes are near the end; the final FINDINGS text is the last assistant message).

VERIFY EACH FINDING
- Open the actual lines at HEAD. Classify REAL / OVERSTATED / WRONG with file:line and one sentence of why.
- A finding contradicting a documented decision is WRONG: check AGENTS.md, INVARIANTS.md, SECURITY.md, POSTPONED.md, CONTRIBUTING.md and the `note:` comments near the code. Tests that pin behaviour on purpose count as documentation.
- Performance findings are only REAL if the cost is on a real hot path at a realistic size; micro-optimisations nobody would measure are OVERSTATED. Skip taste nits.
- Findings that need a design decision (API break, behaviour change a person should choose) are DECISION, with the options.

FOR EACH REAL FINDING WORTH FIXING, prepare a patch following the repo's conventions (AGENTS.md, CONTRIBUTING.md):
- match the surrounding code's style and comment density; `note:` paragraphs explain why, plainly, no story of how the bug was found, no souvenir numbers;
- a behaviour fix comes with a test that FAILS without the fix: check by reverting just the source and running the test (mutation check), and report that you did;
- look for an existing test first and extend it where natural;
- run `cargo fmt --all`, `cargo clippy -p <crate> --all-features --all-targets -- -D warnings`, the affected crate's tests, and `RUSTDOCFLAGS='-D warnings' cargo doc -p <crate> --all-features --no-deps` if docs changed;
- do NOT edit CHANGELOG.md files; instead write the proposed changelog bullet (Keep a Changelog, section fixed/changed, the style of the existing entries under `## [unreleased]`) and a proposed commit message (`crate: what changed, in one lowercase line`, blank line, prose) in your report;
- keep each fix minimal.

REPORT (your final message): a table of every finding with verdict and one-line reason; then for each patch: path, what it does, test name and mutation result, proposed changelog bullet, proposed commit message. Also list DECISION items. Be concise.
