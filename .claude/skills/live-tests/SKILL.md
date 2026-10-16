---
name: live-tests
description: Run this workspace's live test suites - the tests that send real requests to a real endpoint, in nachalnik, nachalnik-eval, nachalnik-mcp, nachalnik-providers (Anthropic's, OpenAI's Responses, Gemini's and System One's APIs) and kamchatka - with each suite's variables set right, and report per test what passed, failed, was skipped and why, telling a regression from a model that would not play along. Use when asked to "run the live tests", "run the live suites with model X", to check a change to how requests are built against a real API, or before a release.
---

# the live tests

The offline suites check what this workspace sends; only a live run checks that a real API accepts
it and that a real model can act on it. CONTRIBUTING.md's "the live tests" section is the
reference: every variable, and the mistakes each one has cost. This file is how to run them
without making those mistakes, and how to read what comes back.

What the person gives: **keys**, and **models** where the defaults won't do. OpenRouter with a
cheap model that can call tools runs most of it. Ask what a run may cost before starting one
that is not free; each suite's header says roughly how many requests it makes.

## rules that do not bend

- **Keys never go in the repository**, a commit, a log or a prompt. They live in
  `$TMPDIR/live/key.env`, mode 600, and are `source`d by the command that runs the suites.
- **A key goes only to its own endpoint.** The suites are written so that this holds - an
  OpenRouter key is read only when the base URL is OpenRouter's - and `live.py` refuses to run
  `kamchatka`'s suite without `KAMCHATKA_BASE_URL`, since it would otherwise send the key to
  Google's endpoint. Don't work round either.
- **A skipped test counts as passed** in cargo's summary. Read `live.py`'s, which runs each test on
  its own and reports SKIP with the reason the test printed.
- **Suspect the model before the code.** A live failure is a claim about the code only once it
  has failed again, on a second model, and not on the last release.

## 1. set up

```sh
SKILL=<repo>/.claude/skills/live-tests
mkdir -p $TMPDIR/live
( umask 077; cat > $TMPDIR/live/key.env <<EOF
# nachalnik, nachalnik-eval, nachalnik-mcp
export OPENROUTER_API_KEY=<key>
export NACHALNIK_TEST_MODEL=<a cheap model that calls tools>
export NACHALNIK_TEST_MODEL_B=<a second one, for the swap test>
# opted into one by one: a picture or a PDF sent to a text-only model is a 400 about nothing
# export NACHALNIK_VISION_MODEL=...  NACHALNIK_DOCUMENT_MODEL=...
# kamchatka
export KAMCHATKA_API_KEY=<key>
export KAMCHATKA_BASE_URL=https://openrouter.ai/api/v1
export KAMCHATKA_TEST_MODEL=<model>        # not KAMCHATKA_MODEL, which the suite does not read
# export KAMCHATKA_DOCUMENT_MODEL=...  KAMCHATKA_GEMINI_API_KEY=...  KAMCHATKA_GEMINI_MODEL=...
# the vendors' own APIs, each with its own key, or through OpenRouter
# export ANTHROPIC_API_KEY=...  OPENAI_API_KEY=...  GEMINI_API_KEY=...
# export NACHALNIK_ANTHROPIC_BASE_URL=https://openrouter.ai/api/v1   # with OPENROUTER_API_KEY
# export NACHALNIK_RESPONSES_BASE_URL=https://openrouter.ai/api/v1
# System One: a model OpenRouter lists at /models?output_modalities=decisions
# export NACHALNIK_SYSTEM1_MODEL=...  KAMCHATKA_SYSTEM1_MODEL=...
EOF
)
source $TMPDIR/live/key.env
python3 $SKILL/live.py --list          # which suites the variables set are enough for
```

- Leave `NACHALNIK_CONTEXT_LIMIT` out: `live.py` unsets it for `nachalnik`'s suite, where it makes
  the runtime report a limit the endpoint does not enforce. It sets `KAMCHATKA_CONTEXT_LIMIT` to
  `12288` (`16384` for a Claude model) when it is not set, which the compaction test needs.
- Check a key answers before a long run: a key that lists models can still be refused
  completions (`Insufficient balance` on a free tier not yet enabled).

## 2. run

```sh
python3 $SKILL/live.py --retry 1                 # every suite the variables allow
python3 $SKILL/live.py kamchatka --only compact  # one suite, the tests whose names match
```

- Each suite builds once, then runs its tests one at a time, each as a process of its own. The
  suites run side by side, which CONTRIBUTING.md says they can.
- A suite whose variables are missing is **NOT RUN** with what it needs, rather than run to skip
  every test. `--list` says the same before anything is built.
- The summary is in `$TMPDIR/live/<HEAD>/summary.txt`, and each test's whole output beside it.
  Results: `ok`, `SKIP` with the line the test printed, `FAIL` with its panic message, `FLAKY` (failed,
  then passed on a retry), `IGNORED`, `NOT RUN`, `BUILD FAILED`.

## 3. read what failed

For each FAIL, in this order, and stop at the first that explains it:

1. **Read its log.** A 429, a timeout, `Insufficient balance` or a 5xx is the endpoint. A 400
   naming a content part, a turn order or a model is the endpoint refusing what another accepts;
   the test's `note:` usually says where that has been seen.
2. **The model.** Small and free models fail tests that ask them to *do* something, and not the
   same ones each time. FLAKY is this. Run the test again on a second model
   (`NACHALNIK_TEST_MODEL=<other> live.py nachalnik --only <name>`).
3. **The last release.** Run the same test on the last tag:
   ```sh
   git worktree add $TMPDIR/live/at-tag <crate>-v<version>
   python3 $SKILL/live.py <suite> --only <name> --tree $TMPDIR/live/at-tag
   git worktree remove $TMPDIR/live/at-tag
   ```
   `--tree` builds into a target of its own. Failing there too, it is not this change.
4. **Then the code.** What is left is a finding: reproduce it offline where it can be (a scripted
   provider, `common::endpoint`'s socket, the conformance suite) so the fix has a test that does
   not need a key, and fix it as the repository's conventions say.

Also read the SKIPs: a test that skipped because a variable was missing was not run. The ones that
skip by design (`the whole answer arrived before a fragment could be seen` on a model that sends
its answer in one piece, no thought summary from a *lite* model) say so, and running them needs
another model rather than another variable.

## 4. report

Per suite: the model and endpoint, how many passed, and every test that did not, with its result
and reason - the endpoint's, the model's (with the second model's result), or the code's (with the
test that now reproduces it). Say which suites were not run and what they needed. Nothing that is
anyone's to do lives only in the report: a code failure gets fixed or goes into POSTPONED.md in
its form.

## gotchas

- **A run without a key compiles the live tests but skips them**, so the compiler catches a
  removed field and nothing catches an assertion reading the wrong one of two similar fields.
  After a change to the offline tests, search the live file for the same name.
- **Some endpoints truncate tool calls**, which looks like the model writing broken JSON. Check
  the raw response before blaming the accumulator.
- **`gemini_live` cannot go through OpenRouter**: it serves Google's models only through its own
  APIs, and the `Gemini` client speaks Google's native one.
- **`an_interrupt_stops_a_stream_that_is_watching` cannot pass** on a model that sends its whole
  answer in one fragment.
- `kamchatka`'s suite names its directories by its process, so two runs against different models
  can share a tree.
