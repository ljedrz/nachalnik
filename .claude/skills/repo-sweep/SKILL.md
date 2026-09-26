---
name: repo-sweep
description: Run a full code sweep of this workspace - audit, performance and code quality, test quality and documentation, and the friction the tools cost a model using them - with headless kamchatka sessions driven by a given model, verify every finding with agents in scratch worktrees, commit the valid fixes on a dedicated branch and write what needs a person's decision into POSTPONED.md. Also drives tests for the mutants `cargo mutants` finds surviving. Use when asked for "a full repo sweep using kamchatka and model X", for sweeps, audits or reviews driven by kamchatka, or for a mutants sweep.
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
python3 $SKILL/configure.py <model>       # writes $SWEEPS/audit.json, quality.json and docs.json
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
code quality), `tests/` (test code only), `docs/` (prose against the code it describes). Each is
one module or one concern, names its files and lists concrete failure classes - narrow scopes are
what made the findings real.

```sh
$SKILL/launch.sh audit a $SKILL/scopes/audit/*.txt
$SKILL/launch.sh quality q $SKILL/scopes/quality/*.txt
$SKILL/launch.sh tests t $SKILL/scopes/tests/*.txt
$SKILL/launch.sh docs d $SKILL/scopes/docs/*.txt
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

## friction: what the tools cost a model

A different sweep: not a model reading code, but a model *using* the program, and the question is
where the tools cost it a call. The answers are in the records rather than in anything it says.

```sh
python3 $SKILL/friction.py $SWEEPS/tmp/kamchatka/*.json     # every snapshot a sweep left, for free
```

Every snapshot the other sweeps wrote is already a baseline: an error result, bucketed by its text
with the names and numbers taken out, how often, and what the model did next. The row to act on is
one the model does not recover from - the same call again, or another tool, and the thing it was
doing never done.

For the tools a read-only sweep never offers, `scopes/friction/` holds ordinary tasks, one message
a line, each written to need particular operations without naming them:

```sh
python3 - <<'EOF'       # $SWEEPS/fr.json: every tool, every domain allowed, a question answered no
import json, os
json.dump({"model": "<model>", "requests": 0,
  "tools": ["fs", "shell", "context", "log", "setup", "fork"],
  "allow": ["fs", "exec", "context", "log", "setup", "fork"], "on-ask": "deny",
  "system": "You are a software engineer working in the Rust workspace in the current directory, "
    "for a person who is not watching and cannot answer questions. Do what each message asks "
    "with the tools you have, and say plainly when something cannot be done. Do not run cargo: "
    "there is no toolchain here."}, open(os.environ["SWEEPS"] + "/fr.json", "w"))
EOF
for t in $SKILL/scopes/friction/*.txt; do
    nohup $SKILL/friction.sh f-$(basename $t .txt) $t > /dev/null 2>&1 &
done
python3 $SKILL/friction.py $SWEEPS/fr/tmp/kamchatka/*.json
```

- Each session works in a worktree of its own, removed afterwards; what it changed is kept as
  `$SWEEPS/fr/NAME.diff`. The network is left to be asked about, so a command that reaches for it
  meets a real refusal.
- A read-only session and an open one fail differently: under a policy that allows a few
  operations, a call nobody can place is refused by the policy before the tool sees it, and that
  refusal is the sentence the model has to recover from. Read both.
- The fix is a sentence - a refusal that says nothing was done and what to call, an error that
  names the right argument - with a test that holds the sentence, and a changelog bullet, since a
  model reads it. A fix that changes what a tool *does* is a decision.
- Error counts are not the whole of it. Read the transcripts for an operation a task needed that
  the model never reached for, or one it used and misread.

## mutants: tests for what `cargo mutants` finds surviving

A third kind of sweep: the work list comes from `cargo mutants` rather than from a model, sessions
draft the tests, and agents verify them. Everything below `target/agents/` is git-ignored and on
disk; keep it there rather than on `/tmp`, which is a tmpfs.

1. **Run the crate against its own tests first**, then iterate over the survivors against the whole
   workspace, since a mutant another crate's tests catch is not a gap:
   ```sh
   $SKILL/mutants.sh X-own -p <crate> --all-features -j 6
   cp -r target/agents/out/X-own target/agents/out/X
   $SKILL/mutants.sh X --workspace --file '<crate>/src/**/*.rs' --all-features \
       --test-workspace=true --iterate --timeout 300 -j 4
   ```
   `mutants.sh` prints how many outcomes are tainted. Anything but `0 tainted, 0 untested` means
   `python3 $SKILL/tainted.py target/agents/out/X --strip` and the same `--iterate` run again,
   until it is. Check the "Found N mutants to test" line against what was expected, and spot-check
   a few `log/*.log`: `Compiling <crate>` for the mutated crate, and failures that are about the
   mutated code.
2. **Sessions**: `python3 $SKILL/mutants_tasks.py target/agents/out/X/mutants.out $SWEEPS/tasks/X 8`,
   then `queue.sh 4 $SWEEPS/tasks/X/*.txt`, with `SWEEPS=target/agents/sw` and the key sourced.
   `$SWEEPS/mt.json` is the session settings: every tool but `fs`, `shell` and `context` off, and a
   `sandbox-read` naming this session's `$CARGO_HOME`, `~/.rustup` and the repository's `.git`.
   Run `cargo fetch` first when `$CARGO_HOME` is new, because the sessions are offline.
3. **Verify** with one agent per group of about three sessions, at most four at a time, from
   `mutants_prompt.md`. Each keeps its verdicts in a file as it goes. A test is kept only when
   `scripts/mutate.sh` says nothing else caught it.
4. **Commit** as in section 5, redoing each mutation check yourself. Write every mutant left
   surviving into the report with its reason: equivalent (the mutated code cannot be told apart
   from the original by any caller), or the exact edge of a private threshold, which is not
   pinned.

### what goes wrong

- `CARGO_TARGET_DIR` set in the shell is passed on, so every copy builds into one target and
  tests run another copy's binary. `mutants.sh` unsets it.
- `-p <crate>` pins the tested packages and overrides `--test-workspace`. The whole workspace
  against one crate's mutants is `--workspace --file '<crate>/src/**/*.rs' --test-workspace=true`.
- The automatic timeout comes from a baseline that ran only the crate's own tests. Pass
  `--timeout 300` with `--test-workspace`.
- A copy's path is long enough that seven tests fail unmutated, over the unix socket path limit
  or reading a `.gitignore` above the copy. `mutants.sh` skips them and passes `--no-fail-fast`, so
  one failing test binary does not stop the suite.
- A whole-workspace copy is about 3 GB. Put the copies on `/home` (`mutants.sh` does), not in a
  temporary directory with a quota.
- **A mutant can spawn without end** and fill the user's process limit, and then this session
  cannot fork and dies. `systemd-run` is out of reach in the confinement, so `mutants.sh` caps the
  run with `ulimit -u`. A runaway then starves the rest of its own run instead, whose builds fail
  to fork and are recorded as Unviable, and **`--iterate` skips an Unviable mutant forever**.
  That is what `tainted.py --strip` undoes.
- Mutated `kamchatka --headless` binaries ignore SIGTERM and outlive the run. Look for processes
  under `target/agents/mt/cargo-mutants-*` afterwards and SIGKILL them by pid.
- The sessions' network gate refuses loopback, so they cannot run a test that serves anything,
  and a session's claim about one means nothing until an agent has run it.
- A `--sandbox-read` on the command line replaces `mt.json`'s list instead of adding to it, and
  cargo loses its toolchain.
- Run one crate at a time. `kamchatka` is the long one: most of its mutants are Unviable, since its
  return types mostly have no `Default`.
- The runaway is `Sandbox::argv` replaced with `vec![]` or `vec![Default::default()]`. At start-up
  `sandbox::available` probes the confinement by running the program with `argv("exit 0")`, and
  with those arguments gone the child is a whole start-up of its own, which probes again. It is
  caught, as a timeout, with the cap holding at about a thousand processes. Run `sandbox.rs` alone
  at `-j 1`, and leave it out of the `--iterate` runs over the rest, or the two run again.
- `--iterate` rewrites `outcomes.json`, `missed.txt` and `mutants.json` with the current run's
  mutants only, and `mutants_tasks.py` reads `outcomes.json`. Copy the output directory aside
  before each further run over it, and make tasks from every copy that still has a miss.

## gotchas

- `pkill -f PATTERN` matches the shell running it; kill by pid.
- A command with `&&` before a heredoc, and `git commit` on the next line, commits even when the
  first command failed.
- `/tmp` itself is not writable here; use `$TMPDIR`.
- Delete `$TMPDIR/target-*` and old `$TMPDIR/kamchatka/*` session files between waves.
- `git stash` is shared between worktrees: agents must not use it.
