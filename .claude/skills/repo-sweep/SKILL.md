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
python3 $SKILL/configure.py <model>       # writes $SWEEPS/audit.json, quality.json, docs.json, compact.json, maintain.json
cargo build --release -p kamchatka        # run.py copies it, once, for the whole run
```

- `configure.py` reads the model's context length from the endpoint's listing (or take
  `--limit N`) and scales the context budget the prompts enforce: the next request stays under
  half the window or 100k, whichever is smaller, with kamchatka's own compaction as a backstop.
  The window goes to `$SWEEPS/limit` and from there to kamchatka, so both measure against it.
- **A listing can claim more than the endpoint serves**, and then `configure.py --limit` under
  it fixes it. But a lone 400 is not proof: once, a sweep's largest request so far came back
  `400 Bad Request`, which looked like the window's edge, and another sweep's next request was
  larger still and was answered. Look at the largest request that went through (`status.py`'s
  `max`) before concluding. Either way kamchatka compacts after a failed request and the sweep goes
  on, and a turn failed part-way does not make it incomplete (`sweeps.py`).
- **Look at the model's API page** for its supported parameters. Where reasoning or its effort is a
  parameter and not already at its highest by default, put it in `$SWEEPS/params.txt`, one
  `KEY JSON` per line (for example `reasoning {"effort":"high"}`); `sweep.sh` sends each as
  `/params` before the prompt. A null `reasoning_tokens` in usage does not mean no reasoning.
- **Another endpoint** is `KAMCHATKA_BASE_URL` set to its base (`https://host/v1`, without
  `/chat/completions`). Check it answers before starting anything: a key that lists models can
  still be refused completions (`Insufficient balance` on a free tier not yet enabled).
- `CARGO_TARGET_DIR` may point at the repository's `target`; the scripts honour it.

## 2. sweep

Scopes are in `scopes/`: `audit/` (correctness against INVARIANTS.md), `tests/` (test code only),
`quality/` (performance and code quality), `docs/` (prose against the code it describes),
`maintain/` (what makes the code or the prose harder to change than it needs to be - a file too
large to read at once, logic patched over time, duplication that has drifted; verify a proposed
split by checking that the moved lines are the same lines) and `compact/` (what can go or be
merged with nothing lost). Each is one module or one concern, names its files and lists concrete
failure classes - narrow scopes are what made the findings real.

A sweep is one session given its scope and three continuations, the last asking for the
FINDINGS. `run.py` runs them all, and decides what this section used to leave to whoever was
watching:

```sh
nohup python3 $SKILL/run.py > /dev/null 2>&1 &          # every KIND, in order; or name some
python3 $SKILL/status.py                               # where each sweep stands
python3 $SKILL/status.py --watch 15                    # in the background; wakes you on news
```

- **No time limit.** Models differ by ten times in how long an answer takes - a reasoning model
  on a free tier answered once every two and a half minutes - and a sweep cut off by a deadline
  had not got past its first message. A sweep is run until it has answered its last; one whose
  records stop growing for `--stall` minutes is stopped and run again.
- **Done means done**, read off the records (`sweeps.py`): exited on its own, every message sent,
  no failed turn, the last answer ended rather than cut off. Anything else is run again from the
  start, up to `--tries` times; what is still not done is listed as given up.
- **As many at once as the endpoint takes.** It starts at `--width` (4) and narrows by one when
  the sweeps print more than three retries each in ten minutes (`answered 429; trying again`),
  widening again after twenty minutes of none, up to `--max` (10). A sweep that ends on a failure
  narrows it too, and pauses new starts while the endpoint recovers.
- **A run reads one commit.** It makes a worktree at HEAD in `$SWEEPS/tree` and a copy of the
  binary in `$SWEEPS/kamchatka`, and every sweep reads and runs those, so fixes committed and files
  reverted for mutation checks meanwhile are nothing a sweep half-way through sees. The findings
  are about that commit: verify each against HEAD. Remove both once the run is read
  (`git worktree remove $SWEEPS/tree`); the next run makes them again.
- Run again, it skips what is complete and adopts what is still running. It leaves a sweep's
  earlier tries in `$SWEEPS/old/`.
- **Never kill sweeps by hand to change course.** A killed sweep's work is lost; `run.py` reruns
  it from the start. To change how many run, stop `run.py` alone (its pid is in
  `$SWEEPS/run.pid`) and start it again with other options: the sweeps carry on and are adopted.

**Watch it, every fifteen minutes or so, with `status.py --watch`.** It returns early when a
sweep ends or something needs looking at:
- **refused**: a sweep whose calls keep being refused is spending its turns on nothing (once, a
  model nested its tool arguments one level too deep and every call was refused). Read its
  `NAME.err`; if it is the model's habit rather than the scope, every sweep will do it, and that
  is a finding about the tools before it is anything else.
- **a sweep that drifted off its scope** (a tests scope that decided to report production code)
  shows only in what it found: read the notes of the first sweeps to end before trusting the rest.
- **given up**, or `run.py` not running with work left.

## 3. read what a sweep found

`python3 $SKILL/read.py NAME` - its replies, then the model's `context` notes, which is where a
hygiene run keeps its findings; `-r` adds its reasoning. A sweep that could not write its session
is read from its records, and gives its notes alone.

## 4. verify, in waves

Start as the first sweeps complete, with the run going on beside it; `status.py --watch` says
when one has. Where agents are not to be used (the person's own instructions decide), verify the
same way yourself, one sweep at a time.

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
cargo +1.95.0 check --workspace --all-features --all-targets --locked
cargo +1.95.0 minimal-versions check --workspace --all-features --direct
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
for t in $SKILL/scopes/friction/*.txt; do          # three at a time; a session ends in `done`
    while [ "$(jobs -rp | wc -l)" -ge 3 ]; do sleep 15; done
    $SKILL/friction.sh f-$(basename $t .txt) $t > /dev/null 2>&1 &
    sleep 60
done
python3 $SKILL/friction.py $SWEEPS/fr/tmp/kamchatka/*.json
```

- `friction.sh` carries a session lost to the endpoint on, as the sweeps' resume does, so a free
  model's 429s cost time rather than the session; started all at once, most die anyway.
- Resumed sessions leave a snapshot per process, each holding the whole history before it, so
  `friction.py` counts an early error once per snapshot. Count sessions by the snapshot name with
  its `-N` suffix taken off before weighing a row.

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
   workspace, since a mutant another crate's tests catch is not a gap. One crate at a time, and
   one run at a time:
   ```sh
   $SKILL/shards.sh X-own 4 -p <crate> --all-features --timeout 300
   python3 $SKILL/merge.py target/agents/out/X-own-merged target/agents/out/X-own-{1,2,3,4}
   SEED=X-own-merged $SKILL/shards.sh X 4 --workspace --file '<crate>/src/**/*.rs' \
       --all-features --test-workspace=true --iterate --timeout 300
   python3 $SKILL/merge.py target/agents/out/X-merged target/agents/out/X-{1,2,3,4}
   ```
   `shards.sh` runs cargo-mutants `--in-place`, one `--shard` in each of the worktrees
   `$REPO/w1`..`wN` (kept out of `git status` by `.git/info/exclude`), under one `guard.py`. Not
   `mutants.sh`'s copies: a copy is at `target/agents/mt/cargo-mutants-PKG-XXXXXX.tmp`, and
   kamchatka's served-session tests bind a unix socket under the copy's own target directory -
   118 bytes there, over the 107 a socket path may be - so they fail unmutated and every mutant
   in a workspace pass looks caught. Nothing under the repository is short enough for a copy,
   and nothing outside it is writable. A worktree at `$REPO/wI` is. cargo-mutants' copies also
   take the worktrees along, since it does not read `.git/info/exclude`: once they exist, every
   run is a sharded one.

   The shards of a later `--iterate` run have to skip the same mutants, or each divides a
   different list and some are tested twice and others never: seed every shard from the merged
   output (`SEED`), and before a retry of tainted shards `merge.py --unify` them. `--iterate`
   rewrites `outcomes.json` with only what it tested again, so copy a shard aside before
   retrying it, and merge the copy first and the shard last (the last outcome wins).

   Each shard prints how many outcomes are tainted. Anything but `0 tainted, 0 untested` means
   `python3 $SKILL/tainted.py target/agents/out/X-I --strip`, `--unify`, and the same `--iterate`
   run again, until it is. A shard that drew no mutants writes no `outcomes.json`, and is clean.
   Check the "Found N mutants to test" lines against what was expected, and spot-check a few
   `log/*.log`: `Compiling <crate>` for the mutated crate, and failures that are about the
   mutated code - a `path must be shorter than SUN_LEN` is the long path, not a catch.
2. **Sessions**: `python3 $SKILL/mutants_tasks.py target/agents/out/X-merged/mutants.out $SWEEPS/tasks/X 8`,
   then `queue.sh 4 $SWEEPS/tasks/X/*.txt`, with `SWEEPS=target/agents/sw` and the key sourced.
   A mutant `verdicts.jsonl` already settles is left out and counted on stderr; add
   `--retry-unsettled` to give the ones an earlier sweep could not settle another session.
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
5. **Record what survives** in `$SKILL/verdicts.jsonl`, so that the next sweep does not spend a
   session on it again: every mutant left equivalent, not pinned, dead (`no-caller`, with its
   POSTPONED entry) or unsettled, with the reason:
   `python3 $SKILL/ledger.py add target/agents/out/X-merged/mutants.out equivalent "<why>" "<name>"...`,
   the names as `outcomes.json` gives them. `ledger.py` says how an entry is keyed - the
   file, the function, the mutation and the text of its line, never the line number - and
   `ledger.key(mutant, diff)` makes the key from an `outcomes.json` entry and the diff
   cargo-mutants wrote for it. An entry stops matching when its line is edited, which is the
   point: the code it was about is gone. A killed mutant needs no entry; its test is the record.
   Commit the ledger with the tests.

### what goes wrong

- `CARGO_TARGET_DIR` set in the shell is passed on, so every copy builds into one target and
  tests run another copy's binary. `mutants.sh` unsets it.
- `-p <crate>` pins the tested packages and overrides `--test-workspace`. The whole workspace
  against one crate's mutants is `--workspace --file '<crate>/src/**/*.rs' --test-workspace=true`.
- The automatic timeout comes from a baseline that ran only the crate's own tests. Pass
  `--timeout 300` with `--test-workspace`.
- A copy's path is long enough that seven tests fail unmutated, over the unix socket path limit
  or reading a `.gitignore` above the copy. `mutants.sh` skips them and passes `--no-fail-fast`, so
  one failing test binary does not stop the suite - but the served-session tests added since are
  more than seven, which is why runs are sharded now (step 1). In the worktrees two fail
  unmutated, the pty test this machine refuses and the `.gitignore` one, and `shards.sh` skips
  just those.
- `tainted.py` looks for EAGAIN as `os error 11` exactly: tests make `os error 111`, a refused
  connection, on purpose.
- A whole-workspace copy is about 3 GB. Put the copies in the repository's `target/agents`
  (`mutants.sh` does), not in a temporary directory with a quota.
- **A mutant can spawn without end** and fill the user's process limit, and then this session
  cannot fork and dies. `systemd-run` is out of reach in the confinement, so `mutants.sh` caps the
  run with `ulimit -u`. A runaway then starves the rest of its own run instead, whose builds fail
  to fork and are recorded as Unviable, and **`--iterate` skips an Unviable mutant forever**.
  That is what `tainted.py --strip` undoes.
- **The process cap is not a memory cap.** A thousand start-ups of the program exhaust RAM well
  under it, and three runs at once did once take the machine down. `mutants.sh` runs `guard.py`
  beside each run: it SIGKILLs orphaned mutated binaries, kills alone a binary built in a copy
  that passes `MAX_RSS_GIB` (8) - `Context::undo` replaced with `true` under a test that undoes
  until it cannot grew one to 44 GB; killed, its test fails and the mutant is caught - and kills
  the whole run when
  MemAvailable falls under `MIN_AVAIL_GIB` (10) or the run passes `MAX_PROCS` (4000) processes,
  logging the top consumers to `out/NAME.guard`. Run one `cargo mutants` at a time regardless.
- **A mutant can signal everything the person has.** `Stragglers::call_of` replaced with
  `Some(0)` made a unit test's `stop` take every process for its own, and SIGTERM then SIGKILL
  ended the desktop, the session driving the run and the run with it - no OOM, and the guard,
  dying with the rest, logged nothing. `.cargo/mutants.toml` excludes those mutants, and the tests
  that reach `stop` scope their own thread's signals. The confinement a sweep runs under used to
  scope nothing, `--confine-and-run` by hand having no session above it; it scopes signals itself
  now, so a run under it can still stop the session driving it, and nothing outside. A run that
  ends with every shard `ERROR interrupted` at the same second was signalled: look at what the
  mutants in flight do with signals before resuming.
- **A mutant can start the program over and over.** `Sandbox::argv` emptied made the sandbox
  probe a session runs at its start start a session instead, which probed again: 5566 processes
  when the guard fired, and what its path sweep missed in the `cargo test`'s process group grew
  back to 14601 after it had gone, asleep and so quiet. `.cargo/mutants.toml` excludes it, and
  the guard now freezes the run, kills the process groups cargo-mutants made under it as well as
  what is under ROOT, and sweeps until nothing is left. After an EMERGENCY in a `.guard` log,
  count what is still there (`pgrep -fc '^/home/.*/w[0-9]+/target/'`) before anything else.
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
  with those arguments gone the child is a whole start-up of its own, which probes again. Under
  `mutants.sh` at `-j 1` the cap held it at about a thousand processes; under eight shards it
  outgrew the guard (see "A mutant can start the program over and over" above), so
  `.cargo/mutants.toml` no longer generates either. Before a run, read this section and the notes
  above for every mutant they name, and check `mutants.toml` still excludes the dangerous ones.
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
