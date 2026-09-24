---
name: repo-sweep
description: Run a full code sweep of this workspace - audit, performance and code quality, and test quality - with headless kamchatka sessions driven by a given model, verify every finding with agents in scratch worktrees, commit the valid fixes on a dedicated branch and write what needs a person's decision into POSTPONED.md. Use when asked for "a full repo sweep using kamchatka and model X", or for sweeps, audits or reviews driven by kamchatka.
---

# a kamchatka-driven sweep

The whole method, as it was worked out over three rounds with `stealth/space-bunny-alpha`. What
the person gives: a **model id** and a **key** (and optionally an endpoint; OpenRouter is the
default). Everything else has a default below. Report progress as batches finish; do not stop to
ask about anything this file decides.

## rules that do not bend

- **The key never goes in the repository**, a commit, a patch or an agent prompt. It lives in
  `$SWEEPS/key.env`, mode 600, and is `source`d by the commands that need it.
- **Commits go on a dedicated branch** - the one the person names, or `sweeps/<model-slug>` made off
  the current branch. Never master, never pushed, no PR unless asked.
- **Only verified findings are committed.** A sweep's claims are leads, not facts: in earlier rounds
  most "high" findings were wrong, and a third to a half of the rest were overstated.
- **Agents never touch the main working tree.** Each works in its own `git worktree` with its own
  `CARGO_TARGET_DIR`, and leaves patches. Stage commits by file name, never a directory.
- The repository's conventions are the standard for every commit: read AGENTS.md first.

## 1. set up

```sh
SKILL=<repo>/.claude/skills/repo-sweep
export SWEEPS=$TMPDIR/sweeps; mkdir -p $SWEEPS/patches
( umask 077; cat > $SWEEPS/key.env <<EOF
export KAMCHATKA_API_KEY=<key>
export KAMCHATKA_BASE_URL=https://openrouter.ai/api/v1
export OPENROUTER_API_KEY=<key>           # for nachalnik's examples and live tests
export KAMCHATKA_TEST_MODEL=<model>       # for kamchatka's live suite
EOF
)
source $SWEEPS/key.env
python3 $SKILL/configure.py <model>       # writes $SWEEPS/audit.json and quality.json
cargo build --release -p kamchatka        # sweeps run the release binary; rebuild before a batch
```

- `configure.py` reads the model's context length from the endpoint's listing (or take
  `--limit N`) and scales the context budget the prompts enforce: the next request stays under
  half the window or 100k, whichever is smaller, with kamchatka's own compaction as a backstop.
- **Look at the model's API page** for its supported parameters. Where reasoning or its effort is a
  parameter and not already at its highest by default, put it in `$SWEEPS/params.txt`, one
  `KEY JSON` per line (for example `reasoning {"effort":"high"}`); `sweep.sh` sends each as
  `/params` before the prompt. A null `reasoning_tokens` in usage does not mean no reasoning.
- `CARGO_TARGET_DIR` may point at the repository's `target`; the scripts honour it.

## 2. sweep

Scopes are in `scopes/`: `audit/` (correctness against INVARIANTS.md), `quality/` (performance and
code quality), `tests/` (test code only). Each is one module or one concern, names its files and
lists concrete failure classes - narrow scopes are what made the findings real.

```sh
$SKILL/launch.sh audit a $SKILL/scopes/audit/*.txt
$SKILL/launch.sh quality q $SKILL/scopes/quality/*.txt
$SKILL/launch.sh tests t $SKILL/scopes/tests/*.txt
$SKILL/wait.sh a-kernel a-context ...     # in the background; you are woken when it returns
```

- About 20 at once has run with no rate-limit trouble on a free model. Each sweep runs up to
  75 minutes (`DEADLINE`, seconds) through its prompt and three continuations.
- **Check each batch 90 seconds in:** `python3 $SKILL/stats.py NAME...` for requests and sizes, and
  `grep -c 'not permitted' $SWEEPS/NAME.err`. A sweep making few small requests, or refused over
  and over, has gone wrong (once, a model nested its tool arguments one level too deep and every
  call was refused): move its files aside and run it again.
- **A sweep can drift off its scope** (a tests scope that decided to report production code).
  Read the notes before verifying; re-run a drifted one with the scope restated.

## 3. read what a sweep found

- Finished: `python3 $SKILL/extract.py "$($SKILL/snap.sh NAME)" -n` - the replies, then the
  model's `context` notes, which is where a hygiene run keeps its findings.
- Killed before it wrote a snapshot (`the session was not written` in `NAME.err`):
  `python3 $SKILL/notes_from_records.py $SWEEPS/NAME.jsonl > $SWEEPS/NAME.notes.txt` recovers
  every note from the call arguments in the stream records.

## 4. verify, in waves

One agent per finished sweep (two small ones may share), **at most about four at a time**: a debug
target is 2-3 GB and a per-session disk quota once ran out and killed every running sweep. Start
the next agent as one finishes.

The prompt is `agent_prompt.md` with REPO, BRANCH, SKILL, SWEEPS, NAME and SWEEP filled in, plus
two lists that make or break the result:
- **already fixed at HEAD** - the commits on this branch that touch the scope, one line each, so
  the agent does not re-find them;
- **known decisions** - what POSTPONED.md and the documented `note:`s already settle.

A read-only question (is this still true?) needs no worktree: say so and forbid builds.

## 5. commit

For each patch an agent leaves, in the main tree:
1. Read the diff. Reject or narrow anything that contradicts a documented decision, widens the
   public API without need, or changes behaviour a person should choose (that is a decision: step 6).
2. `git apply` it (`--3way` if HEAD moved), then `cargo fmt --all --check`,
   `cargo clippy -p <crate> --all-features --all-targets -- -D warnings` (and
   `--no-default-features` where the crate has features), the crate's tests.
3. **Redo the mutation check yourself**: `git show HEAD:<file> > <file>` for the source only, run
   the new test and see it fail, restore. An agent's check can lie (a shared target dir once handed
   agents binaries built from each other's sources).
4. A changelog bullet per crate under `## [unreleased]`, in the style there. Docs-, comment-,
   test- and example-only changes take none and say so in the message.
5. `git add <files by name>`, commit `crate: what changed, in one lowercase line` + prose, ending
   with the attribution line the session gives.

Fix trivial and clearly right things yourself; one fix per commit.

## 6. decisions

A finding that needs a person - an API break, a behaviour choice, a core change for a client's
sake - goes into **POSTPONED.md** as an entry in its form: what it is, why it waits, what would
settle it; plain prose, no souvenir numbers. Before adding, check the existing entries are still
true (the code moves on; an entry that was wrong says so rather than being quietly corrected).

## 7. finish

The CI-equivalent pass, with `RUSTFLAGS=-D warnings`:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets --locked -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --all-features --no-deps --locked
cargo build --workspace --locked
for c in nachalnik nachalnik-mcp nachalnik-providers kamchatka; do cargo check -p $c --no-default-features --locked; done
cargo test --workspace --all-features --locked
cargo test -p kamchatka --no-default-features --locked
scripts/references.sh
cargo +1.88.0 check --workspace --all-features --all-targets --locked
cargo run -p nachalnik --example transparency --features selectors --locked
cargo run -p nachalnik --example compaction --locked
cargo run -p nachalnik --example pricing_a_picture --locked
```

`scripts/windows.sh` needs `cargo-xwin` and Microsoft's licence accepted - the person's call.
Then report: commits, what was rejected and why, the new POSTPONED entries. Live testing with the
model (the kamchatka live suite, the networked examples) comes after the sweeps, not during.

## gotchas

- `pkill -f PATTERN` matches the shell running it; kill by pid.
- A command with `&&` before a heredoc, and `git commit` on the next line, commits even when the
  first command failed.
- `/tmp` itself is not writable here; use `$TMPDIR`.
- Delete `$TMPDIR/target-*` and old `$TMPDIR/kamchatka/*` session files between waves.
- `git stash` is shared between worktrees: agents must not use it.
