---
name: soak
description: Run a soak of this workspace - one long headless kamchatka session driven by a given model, carried across several processes by resumes, a SIGTERM, a SIGKILL, a context wall, a checkpoint `/load` and undo, and then hold the whole chain of records to the invariants that span it with a checker that is proven against deliberate corruptions first. Use when asked for "a soak", "a long session", "a soak test with model X", or to check what survives resumes, kills and compaction over a long run.
---

# a soak

One session, long enough and interrupted often enough that the invariants which only hold *across*
things get a chance to break: identifiers across a resume, states across compaction, items across
`/load` and undo, the record across a kill. The unit tests check each of these small; a soak checks
them on a real record a model wrote. What the person gives: a **model id** and a **key** (and
optionally an endpoint; OpenRouter is the default).

## rules that do not bend

- **Every model request goes through kamchatka**, with the model the person named. No subagents,
  for driving or for verifying: the verification here is the checker and your own reading, run
  directly.
- **The key never goes in the repository**, a commit, a feed or a prompt. It lives in
  `$SOAK/key.env`, mode 600, under the git-ignored `target/`, and is `source`d by the commands that
  need it.
- **The sessions work in a scratch worktree** (`$SOAK/wt`), never the main tree. What they change
  there is thrown away with it.
- **A finding is not a fix.** A soak reports what broke with a reproduction; whether and how to
  fix it follows AGENTS.md like any other change, and a change to what the program does is the
  person's call.

## 1. set up

```sh
SKILL=<repo>/.claude/skills/soak
export SOAK=<repo>/target/agents/soak
mkdir -p $SOAK
( umask 077; cat > $SOAK/key.env <<EOF
export KAMCHATKA_API_KEY=<key>
export KAMCHATKA_BASE_URL=https://openrouter.ai/api/v1
EOF
)
source $SOAK/key.env
cargo build --release -p kamchatka
$SKILL/setup.sh <model>      # the worktree, cargo fetch for offline builds, $SOAK/soak.json
```

`soak.json` offers every tool and allows every domain but the network, answers any question `deny`,
reads the toolchain and the repository's `.git` through the sandbox, and compacts at `0.6`.
`segment.sh` sets the context limit with `KAMCHATKA_CONTEXT_LIMIT` (`LIMIT`, 40000 by default) -
small on purpose, so that compaction runs within minutes rather than hours.

## 2. run the segments, in order

Each segment is one process: `segment.sh NAME FEED [FROM] [SIGNAL N]` resumes from segment FROM's
snapshot, feeds the lines, and sends SIGNAL once N tool calls have started. Run it in the background
and wait on `$SOAK/segments/NAME.err` ending in `exit N`; each writes `NAME.stem`, the record pair
it left in `$SOAK/rec/kamchatka`.

```sh
cd $SKILL
./segment.sh a feeds/a-fresh.txt                          # fresh; ends at the end of its input
./segment.sh b feeds/b-term.txt a TERM 20                 # resumed; SIGTERM mid-turn
./segment.sh c feeds/c-wall.txt b KILL 15                 # resumed; reaches the context limit
LIMIT=80000 ./segment.sh d feeds/d-past-the-wall.txt c KILL 15    # past it; SIGKILL mid-turn
LIMIT=80000 ./segment.sh e feeds/e-stale.txt d            # from a snapshot behind its log
```

What each is for:

- **a** runs eight friction tasks with operator lines between them - `/save` a checkpoint, `/pin`,
  `/compact`, `/undo`, `/redo`, `/budget` - and leaves normally.
- **b** is cut by SIGTERM, which is a leaving: the turn is stopped and waited for, and the snapshot
  is current.
- **c** starts by asking the model to free room. At 40000 the context reaches the limit in c or
  before it - a run can get there in a: `ToolTrimmer` takes only tool results, and the model's own turns
  are what fills a long session. That is POSTPONED's *a context the model's own turns have filled*,
  and the soak's evidence for it rather than a finding. Past the limit, headless passes each
  message over unsent and says so once, so a segment that reaches it ends having done little, and
  its signal rarely fires.
- **d** is the person raising the limit and sending the work again, and is cut by SIGKILL: the
  snapshot is from the start of the turn and the log runs past it. It needs the room to run fifteen
  calls, which is why it carries real tasks rather than asking about the messages that were passed
  over.
- **e** resumes from that snapshot, `/load`s segment a's checkpoint, and undoes twice and redoes
  once. The resume has to number past d's log rather than its snapshot, and says how many records
  it carried on without.

If a segment goes somewhere useless (the model stuck, a feed line misread), rerun that segment from
the same FROM; the chain is whichever stems you give the checker.

## 3. prove the checker, then check

```sh
S=$SOAK/segments; STEMS=$(for s in a b c d e; do cat $S/$s.stem; done)
python3 $SKILL/selftest.py --kamchatka <repo>/target/release/kamchatka $STEMS
python3 $SKILL/check.py    --kamchatka <repo>/target/release/kamchatka $STEMS
python3 $SKILL/stats.py $STEMS
```

`selftest.py` copies the chain, breaks one invariant at a time - an item, call or permission
identifier issued twice, a pin compacted, an item lost, a call run with no decision, a state
skipped, an excluded item sent, content changed across a resume, a resume at the wrong record or
count - and fails unless the checker names each one. Run it first on every chain: a checker that has not been seen to catch
anything on *this* record proves nothing by passing it. A case that finds nothing to break in the
chain says `skip`; the resume cases need two segments. The chain's own violations are printed
first - they are the soak's findings - and a case passes only on a violation its break added.

`check.py` runs `kamchatka --check` on each pair, then walks the chain. A **VIOLATION** is an
invariant broken, and that includes a resume that numbers a record, item, call or permission
again that the log before it had numbered past its snapshot. A **NOTE** is a fact that is not one:
a snapshot behind its log, and a call a kill left open.

`stats.py` gives each segment's counts, the floor - what each compaction left behind - and what the
last snapshot is made of. A floor climbing towards the limit is a session that will stop being able
to send.

## 4. read it

- Read `NAME.err` for each segment, not only the checker: what a line did, what the model said
  when something was refused, and whether it recovered. A refusal the model does not recover from
  is a friction lead (`repo-sweep`'s friction section says what to do with one).
- For each violation, find the records it names, then reproduce it without the soak - a test
  against the kernel or the program, run and seen to fail. A violation that does not reproduce on
  its own is a checker bug until shown otherwise; fix the checker and run `selftest.py` again.
- Check POSTPONED.md and the `note:`s before calling anything new.

## what the first soak found

Each is fixed, with a test, and a soak after the fix ran clean where the first one did not - so a
return of any of them is a regression, and the checker says so:

- **`Snapshot::problems` called a truncated output a second answer**, so `--check` failed every
  snapshot holding one. The kernel's whole copy is now told apart by its `WHOLE_OUTPUT` reason.
- **A resume from a snapshot behind its log reissued numbers**: records, items and permissions
  the killed run had already used. `-r` now numbers past the log beside the snapshot.
- **Headless kept messages refused for being over the limit**, each making the overflow larger
  and all going out together once there was room. They are passed over now, as past `--spend`.

## gotchas

- `segment.sh` feeds its input by process substitution so that `$!` is kamchatka. Behind a pipe or
  `timeout`, a SIGKILL reaches the wrapper and leaves kamchatka running.
- The records go to the session's temporary directory, which `segment.sh` points at `$SOAK/rec`, so
  they are on disk and not mixed with other runs. A resumed session writes `NAME-2`, `NAME-3` beside
  the first.
- A killed process's stdout can stop short of its record; the checker reads the record file.
- Foreground `sleep` is refused in this harness: wait with an `until` loop in the background.
- `/load` and `/save` take absolute paths; the feeds say `@SOAK@` and `segment.sh` fills it in.
- Clean up with `git worktree remove --force $SOAK/wt` and `rm -rf $SOAK` once the report is
  written; the key is in there.
