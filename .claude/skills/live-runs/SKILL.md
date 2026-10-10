---
name: live-runs
description: Hunt for bugs by using kamchatka rather than reading it - dozens of headless sessions driven by a given model, each a scenario built to stress one part of the program (the filesystem tool on hostile files, the sandbox and the network gate, background jobs, output floods, every slash command with bad arguments, the context tool used adversarially, compaction to the wall, resumes and kills, served sessions with several clients, MCP, attachments) plus a hostile fake endpoint and a logging proxy - then verify each finding, fix it on one branch with a test, and write the decisions into POSTPONED.md. Use when asked for "test runs", "debug runs", "random runs" or "live testing with kamchatka and model X".
---

# live runs

A model using the program finds what nobody thought to test, because it does things nobody would
script: names a file `~`, passes the wrong id, reads before it writes, pipes a second line after a
question. The method is breadth first - many short scenarios at once, each aimed at one corner -
and then depth on whatever looks wrong. What the person gives: a **model id** and a **key**.

## rules that do not bend

- **The key never goes in the repository**, a commit, a feed or a prompt. It lives in
  `$LIVE/key.env`, mode 600, under the git-ignored `target/`, and is deleted at the end.
- **Every model request goes through kamchatka.** Verification is reading the records and writing
  tests, done directly.
- **Fixes go on one branch** made off the current one, one fix per commit, each with a test that
  fails without it (`scripts/mutate.sh`, or restore the line by hand and watch it fail). Never
  pushed. AGENTS.md is the standard for every commit.
- **A finding is a lead until a test or a record proves it.** About half of what looks like a bug
  on first reading is the model, a documented decision, or this harness.

## 1. set up

```sh
SKILL=<repo>/.claude/skills/live-runs
export LIVE=<repo>/target/agents/live; mkdir -p $LIVE
( umask 077; cat > $LIVE/key.env <<EOF
export KAMCHATKA_API_KEY=<key>
export KAMCHATKA_BASE_URL=https://openrouter.ai/api/v1
export KAMCHATKA_MODEL=<model>
export OPENROUTER_API_KEY=<key>
export KAMCHATKA_TEST_MODEL=<model>
EOF
)
$SKILL/fixtures.sh                     # proj, edge, media, empty, scratch-allowed under $LIVE/fix
cargo build --release -p kamchatka     # rebuild after each fix, before the runs that verify it
```

## 2. run wide

`run.sh NAME FEED [args]` is one session in a fresh copy of a fixture, in a temporary directory of
its own, with `--check` run on its record afterwards. A feed line is a message or a command.
`system.txt` asks the model to quote every error verbatim, which is what makes the prose readable
as evidence. About twenty at once runs without rate-limit trouble on a free model.

```sh
S="$(cat $SKILL/system.txt)"; F=$SKILL/feeds
r() { n=$1 f=$2; shift 2; nohup $SKILL/run.sh $n $F/$f -s "$S" "$@" > /dev/null 2>&1 & }
FIX=edge r fs-edge fs-edge.txt --allow fs
r sandbox sandbox.txt --allow exec:run,fs
r network network.txt --allow exec:run                    # the gate, answered no
r jobs jobs.txt --allow exec:run,fs                         # then ps for what outlived the session
r output output.txt --allow exec:run
DEADLINE=600 r commands commands.txt                        # every command, bad arguments included
r context context.txt --allow fs,context
FIX=edge LIMIT=14000 r wall wall.txt --allow fs,context,log # compaction to the limit
FIX=edge LIMIT=14000 r wall-noctx wall.txt --allow fs
r malformed malformed.txt --allow fs,context,exec:run,log,setup,fork
r ctx-adversarial ctx-adversarial.txt --allow context,fs
r rules rules.txt --allow fs,exec:run --deny '.env*,secrets/'
r spend spend.txt --spend 6000 --allow fs                  # exit statuses: 3, 4, 124, 130, 143, 129
r requests requests.txt --requests 2 --allow fs
SIG="INT 2" r sigint coding.txt --allow fs,exec:run
r mcp mcp.txt --mcp "arith=python3 <repo>/kamchatka/tests/mcp_server.py" --allow-server arith
FIX=empty LIMIT=40000 DEADLINE=2400 r build build.txt --allow fs,exec:run,context --requests 30
```

The feeds cover more than that list: `ls $SKILL/feeds`. The other drivers:

- `chain.sh` - one session across a SIGTERM, a SIGKILL, a resume past a killed log, `/load`, two
  forks and `reconcile`.
- `serve.sh unix:$LIVE/s.sock $F/servers.txt -s "$S"` - a served session: a client that pipes a
  script and leaves its questions, one that answers them, one that quits. `tcp:127.0.0.1:PORT`
  with `$F/selfport.txt` and `--allow exec:run,net:reach` checks that a command cannot reach the
  port its own session is served on.
- `fake.sh [MODE...]` - one session per misbehaviour of `fakeapi.py`, an OpenAI-compatible endpoint
  whose model id picks it: 429 with `Retry-After`, 500, a stream cut mid-JSON, a non-JSON event,
  no choices, headers then silence, no headers, a drip, `length`, odd or missing usage, a refusal
  for length, two megabytes of answer, a non-streamed body, gzip, forty calls at once, malformed or
  unknown or duplicated calls, an HTML 502.
- `proxy.py PORT $LIVE/out/raw` with `BASE=http://127.0.0.1:PORT/v1` on a run - every byte the real
  endpoint sent, for `rawcheck.py`.

`status.sh` is one line a run. Finished means a `.code` file.

## 3. read

For each run, in this order:

1. **The exit status against RUNNING.md's table**, and `NAME.check`: anything but "nothing that
   does not add up" is a record that does not hold together.
2. **The program's own lines**: `grep '^·' NAME.err`. These are the sentences a person reads; look
   for one that is wrong, that vouches for something that did not happen, or that is missing.
3. **What the tools answered**: `show.py NAME shell`, `show.py NAME fs:`, `show.py NAME context`.
   The snapshot, not the prose - the model paraphrases, and misreads.
4. **Errors across every run**: `errors.py`. A sentence the model did not recover from is a
   friction lead; a bucket of `_unparsed` arguments goes to the proxy.
5. **The model's own complaints**: a model that says "I can't tell whether…" or retracts something
   has usually met an answer that did not say what it did.

Then the hard facts: files written outside a workspace, processes still running after a session
(`ps -eo pid,args | grep 'sleep 10'`), and the key anywhere it should not be.

## 4. verify, fix, decide

- Find the code, then **reproduce without the model**: a unit or integration test against the tool,
  the kernel, `note_for`, the client. Watch it fail, fix, watch it pass, restore the old line and
  watch only the new test fail.
- Check POSTPONED.md and the `note:`s first. A behaviour a note defends is a decision, and so is
  any fix that changes what the program does rather than what it says: those go into POSTPONED.md
  in its form - what, why it waits, what would settle it - not into code.
- A wrong sentence is a fix: the refusal says it refused and what was not done, the error names the
  real cause. A changelog bullet, since a model reads it.
- Re-run the scenario that found it with the rebuilt binary, and keep the record that shows it
  gone.
- Finish with AGENTS.md's commands, `--no-fail-fast`, and the live suites if the request path
  moved. A test that fails the same way on an untouched `master` worktree is the environment.

## what this found the first time

Each is fixed with a test, so a return of any of them is a regression:

- a confined shell connecting to the X server's abstract socket - no cookie needed, so it could type
  into the person's terminal (Landlock's abstract-socket scope)
- a confined shell signalling any process of the person's, `kill -9 -1` included (the signal scope,
  on the layer every command shares - a command can still signal kamchatka itself)
- the full notice taking a request that fitted over the limit, so the model never read it
- a `--connect` script that piped a second line after a question hanging for ever
- `/limit fs:read 200` showing no line of any file
- a dangling or looping link counted as "a pipe, a socket, a device"
- refusals that did not say they were refusals, or blamed the wrong thing: a `revise` the model
  reported as done, a write refused where the session only reads, the session's own closed port,
  a request the tool definitions alone put over the limit, the selector for the model's own notes
- a `grep` or `glob` of a path that is not there, counted as "a file that could not be read"
- a full `context look` skipping the ids of removed items, with nothing to say a gap was removed
  rather than hidden

And three decisions: two in POSTPONED.md, the first undo of a fresh session and output limits that
ignore the window, and a `note:` in `introspect/context/changes.rs` on why `revise` refuses the
person's words.

## what goes wrong

- `pkill -f PATTERN` matches the shell running it, and so does any pattern that appears in the same
  command line - a `sed` mentioning `sleep 900` killed the shell that ran it. Kill by pid, in a
  command of its own.
- `--serve` refuses `--headless` and `--deadline`: with no terminal a served session is headless
  already, and it belongs to the program rather than to a run.
- A resumed session writes its record beside the one it carried on from, so a run's `.stem` is
  only its own while each run has its own `TMPDIR` - which `run.sh` gives it.
- `run.sh` changes into the workspace: a relative path in its arguments is relative to that.
- A waiter on `.code` must not find a stale one; `run.sh` clears a run's files before it starts.
- A pty cannot be made in some confinements, and two suite tests need one.
- Foreground `sleep` is refused in this harness: wait with an `until` loop in the background.
- Clean up with `rm -f $LIVE/key.env`, and `git worktree remove` for any worktree a run used.
