# working in this workspace

What to run, what CI runs, the house conventions, and the things that have cost somebody an
afternoon. [AGENTS.md](AGENTS.md) carries the short form of all of it; this is the long form,
with the evidence each rule came from.

See also [INVARIANTS.md](INVARIANTS.md) for what must not be broken, [MAP.md](MAP.md) for where
things live, [SECURITY.md](SECURITY.md) for the security position, and
[POSTPONED.md](POSTPONED.md) for what is deliberately not built.

---

## commands

```console
cargo test --workspace --all-features       # everything; the live suite skips itself with no key
cargo test --workspace --all-features --no-fail-fast   # when measuring what a test is worth
cargo test -p nachalnik                     # the runtime's offline suite
cargo test -p kamchatka --no-default-features          # the program with no screen and no MCP
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo doc --workspace --all-features --no-deps   # with RUSTDOCFLAGS=-D warnings, as CI does
```

These are pre-commit checks and not just CI steps, and the last one is the one that gets skipped.
`RUSTDOCFLAGS` is not in the environment the way `RUSTFLAGS: -D warnings` is in CI's, so without it
the command prints its warnings, exits `0` and reads as a pass; the `docs` job sets it and does not.
It is worth running, because nothing else in the toolchain reads a doc comment - neither `clippy`
nor the test suites resolve an intra-doc link - so what it catches is caught here or in CI and
nowhere in between. Two shapes, both of which arrived in the same commit. A link to an item under a
name it never had - `Shell::call`, where the method is `invoke` and arrives through a trait, so
there is nothing on the type to read the name off and nothing but rustdoc to say so. And a public
comment linking to a `pub(super)` item, which resolves for everyone in the module and for nobody on
docs.rs.

CI (`.github/workflows/ci.yml`) also builds with **default** features (the tests turn both on, so
nothing else exercises that configuration), checks `nachalnik`, `nachalnik-mcp` and `kamchatka`
with `--no-default-features`, runs the three keyless examples, and checks the whole workspace on the
MSRV, **1.88**. Edition is 2024. `RUSTFLAGS: -D warnings` throughout, so a warning is a failure.

The live suite is the only thing that can check that a real API accepts what this crate builds:

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --test-threads=1 --nocapture
```

It reads `OPENROUTER_API_KEY` or `NACHALNIK_API_KEY` (never a stray `OPENAI_API_KEY`), with
`NACHALNIK_BASE_URL`, `NACHALNIK_TEST_MODEL` and `NACHALNIK_CONTEXT_LIMIT` to point it elsewhere -
Google AI Studio's OpenAI-compatible endpoint and a local ollama both work, and the whole of it
passes against the first (27 tests, `NACHALNIK_VISION_MODEL` included). One endpoint difference
worth knowing when it does not: a request *ending with a model turn* - which is what carrying on
from an interrupted answer builds - is refused by that shim with a 400 and accepted by OpenAI's own
API and OpenRouter, so the interrupt test asks whether the flag was cleared rather than whether the
continuation was taken. It skips rather than fails without a key, or when a free tier has spent its
allowance. `kamchatka`'s live suite reads
`KAMCHATKA_API_KEY` / `KAMCHATKA_TEST_MODEL` / `KAMCHATKA_BASE_URL` instead - **`_TEST_MODEL`**,
where the binary's own flag is `KAMCHATKA_MODEL`, and getting that wrong is quiet: the suite falls
back to its default model, the endpoint refuses a name it does not serve, and eleven tests fail
about tool calls that never happened rather than about the model being wrong. Two of its tests
want more than a key: `KAMCHATKA_CONTEXT_LIMIT` small enough for the fixture to breach, since the
compactor fires on a fraction and a generous limit means it never runs (`12288` works; `32768`
leaves the context at a third of it). The compaction fixture used to land at 47-49% of `12288`
against a threshold of 50%, so the model's own verbosity decided it and the same model passed and
failed on consecutive runs; it is twice the size now and breaches on the file alone, firing at
74-81%. The test says which it was, so a sizing failure reads as one. Also
`KAMCHATKA_DOCUMENT_MODEL` for the one that attaches a PDF. The rest want
`KAMCHATKA_GEMINI_API_KEY`: they drive Google's *native* dialect, where a turn is an order of
blocks, and they will not borrow `KAMCHATKA_API_KEY` unless the base URL is plausibly Google's -
deliberately, because borrowing it once sent an OpenRouter key to
`generativelanguage.googleapis.com` and reported the 400 as three broken tests about turn order.
So a whole-suite run against an OpenAI-compatible endpoint leaves the ordered-blocks path
unexercised, which is worth knowing before reading the count as a clean sweep - and the count says
`23 passed` either way, because a skipped test passes. Measured 2026-09-13 against OpenRouter:
**17 of `kamchatka`'s 23 actually ran**, the other six being the five native-dialect tests and the
PDF one. `nachalnik` does better - 26 of the 27 it had then, the PDF again - and `--nocapture` with a grep for
`skip` is how to see it, since the skip lines are the only place it is said.

**`kamchatka`'s live suite sends requests to Google's shim unless told otherwise**, and that is a
default rather than a detection: `base_url()` in `tests/live.rs` falls back to
`generativelanguage.googleapis.com/v1beta/openai`. So a run with only `KAMCHATKA_API_KEY` set posts
whatever key that is to Google, which answers `400 ... Please pass a valid API key`, and eighteen
tests fail about tool calls, budgets and truncation - none of them about the endpoint. Set
`KAMCHATKA_BASE_URL` explicitly for anything that is not Google. The status line in the failure
output is what gives it away: it names the host.

Free OpenRouter models are enough for both live suites and cost nothing - measured 2026-09-13 with
`KAMCHATKA_BASE_URL=https://openrouter.ai/api/v1` and `KAMCHATKA_CONTEXT_LIMIT=12288`.
`kamchatka`'s 23 passed against both `nex-agi/nex-n2.5-mini:free` and
`nvidia/nemotron-3-super-120b-a12b:free`; `nachalnik`'s 27 passed against the second. Against the
first, `nachalnik` fails four or five of them and *not the same four or five twice*. Two things to
know before reading a result. The `:free` pool is rate-limited upstream and a model that answered
an hour ago can return `429` or an idle timeout now, which the runtime reports honestly as a
provider failure rather than as a test failure. And `nachalnik`'s suite asks a model to *do*
things - use a tool, keep a secret, be interrupted mid-stream - so a small model fails some of them
for being small: five failed on one free model and four of those five passed on another, with one
case failing and then passing on the same model. The list moves between runs of the same model,
so a failure that reproduces on a *second* model is the one worth reading -
`an_interrupt_stops_a_stream_that_is_watching` failed on two and turned out to be the test
pressing its button on the first delta of any kind, which on a reasoning model is the thinking.

**That same test has a second way to fail, and it is not the model being small.** Measured
2026-09-14 against Inception's `mercury-2.5`: it watches for the first *text* fragment now,
interrupts on it, and skips if none arrived - which is a proxy for *is there still a stream to
stop*, and one that holds only where an answer is produced token by token. A diffusion model emits
the whole of it in a burst, so the first fragment and the last are the same event: the guard
passes, the interrupt lands after the provider has already finished, and the stop reason comes
back `Length`. The whole test runs in 1.4 seconds there. It fails deterministically rather than
skipping, and nothing about the kernel is wrong in it - the case the test is about cannot arise on
that endpoint. Left alone on purpose: the obvious repair is to widen the skip, and no version of
it yet proposed can tell *the answer finished first* from *the interrupt was ignored*, which is
the thing the test is for. The same run put `a_reasoning_models_own_turn_comes_back_as_it_went_out`
in the failures too and it passed on re-run - that endpoint's filter sometimes answers the
secret-code-word prompt with a 400 whose body is a refusal sentence, which is worth recognising
before reading it as a malformed request. `kamchatka`'s is not exempt either:
`a_resumed_session_carries_on_and_the_endpoint_accepts_it` plants `LARKSPUR` in a resumed context
and asks which word the model was told to remember, and the shared system prompt two hundred lines
away plants `APRICOT` - so a model that picks the wrong one of two plausible words fails a test
about *projection*. It passed and failed three times running on the same model and the same commit.
Attribute a live failure to the model before attributing it to the code: re-run it, run it on a
second model, and `git worktree add` the last tag and run it there before believing anything.

**Pointing both halves at Google**, which is one key and covers everything except the `file` part:

```console
$ KAMCHATKA_GEMINI_API_KEY=... KAMCHATKA_GEMINI_MODEL=gemini-3.5-flash \
  KAMCHATKA_API_KEY=... KAMCHATKA_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai \
  KAMCHATKA_TEST_MODEL=gemini-3.5-flash-lite KAMCHATKA_CONTEXT_LIMIT=12288 \
    cargo test -p kamchatka --test live -- --test-threads=1
```

Three things that cost an hour each and are facts about the endpoint rather than about this code,
measured 2026-09-11. **`KAMCHATKA_DOCUMENT_MODEL` must not point at Google's shim**: it answers a
`file` content part with `400 Invalid content part type: file`, so that test fails with an empty
answer where the cause is a rejected request - the `file` part is OpenAI's and OpenRouter's to
accept. The same PDF reaches the same model through the native dialect's `inline_data` and is
read, which is what `a_pdf_goes_out_as_a_document_in_the_native_dialect` now pins. **A model that
`models.list` returns may still be refused**: `gemini-2.5-flash-lite` is listed and answers
`404 ... no longer available to new users`. And **the lite models return no thought summaries at
all** - `gemini-3.5-flash-lite` gave none in ten requests across every condition tried - so the
test about a turn carrying its thinking skips there and runs on `gemini-3.5-flash`.
`nachalnik-eval` reads the `NACHALNIK_` ones, since it talks through the same provider:

```console
$ NACHALNIK_API_KEY=ollama NACHALNIK_BASE_URL=http://localhost:11434/v1 \
    cargo run -p nachalnik-eval --example bench -- -m granite4.2:3b --json run.json
```

Its own live suite is a couple of tests and about twenty requests; the whole eight-experiment
suite is about a hundred and sixty, which is the `bench` example's job rather than `cargo test`'s.
Request counts stay because they are what a run costs and somebody has to budget for them; test
counts do not, here or in the readmes.

Test files: `nachalnik/tests/` is `kernel/` (what gets sent, what gets run, who decides, the record,
and the seams - a file each), `context/` (items, undo, compaction, and what the context projects
to), `state`, `session`, `tokens`, `concurrency`, `blocks`, `live`. `nachalnik-eval/tests/` is
`machinery` (the readings, the arithmetic and the pinned instrument digests), `harness` (the whole
loop against a provider whose causal structure the test wrote - the only way to check that the
harness recovers an influence nobody told it about, and, since the rulebook can emit tool calls, the
only way to check the handles without paying a model to use them) and `live`. `kamchatka/tests/`
draws the screen and reads the characters back (`screen/`, one binary made of ten files -
`harness.rs` is the terminal they all sit at, and the other nine are named for what they read off
it), drives the introspection tools through the real loop (`introspect`), runs real commands under
a real ruleset (`sandbox`), and asks the policy its own questions rather than reading the answers
off the screen (`policy`). `edges` is the sweep: every tab at every window size from 1x1 up, every
key at every tab with nothing to act on, and both scrolled past their own ends - a frame that
panics takes the session with it, which is the one failure this program cannot report. `headless`
is the program with nothing drawing it, and half of it runs the *binary*: a settings file, a
signal, a pty, and a session written where it said it was. The program builds its own provider out
of two environment variables in a process of its own, so a scripted one cannot be swapped into it
- `endpoint` there answers on a socket instead, which is the only seam a child process has, and is
what lets a tool call, a spend ceiling and a recorded session be driven without a key. `config` is
the settings file through the same door, and `mcp` is somebody else's server spawned as a child.
`nachalnik-providers/tests/` serves a recorded Gemini stream off a socket and checks what goes
back out (`gemini`), checks what is volunteered to an endpoint about the calling program and to
which one (`attribution`), answers two sockets that go silent, one before the first byte and one
mid-stream (`stalled`), holds each dialect's projection against what its own `to_wire` carries
(`projection`), pins where each puts a `Content::Blob` and that neither is handed one in a place
it would refuse (`blobs`), reads the answer that arrives in one piece (`whole_answers`), takes
thinking back out of the content a model wrote it into (`thinking`), and moves a session to a second
address to be told the model does not live there (`switching`).
`nachalnik-mcp/tests/` stands a real MCP server up rather than mocking one
(`bridge`), and `foreign` runs one written in another language.

The shapes a *stream* arrives in are not tested per provider, because the questions would be the
same each time. `nachalnik-providers/src/conformance.rs` is the suite, behind the `conformance`
feature: a provider is asked the same questions through a real socket, each one a bug that
actually happened, and a question added applies to everything held to it without any of them being
edited. `nachalnik-providers/tests/conformance.rs` holds both dialects to it. `whole_answers` is
what is left over - the answer that arrives in one piece, which only the OpenAI dialect has a path
for, so there is nothing for it to agree with.

---

## conventions

- **`note:` paragraphs.** Doc comments state what something is; a paragraph beginning `note:`
  states why it is that way, what was rejected, or what it costs. This is the house style and it
  is most of the value of the docs - match it. `#![deny(missing_docs)]` and `#![deny(unsafe_code)]`
  are on in every published crate.
- **Comments explain the decision, not the mechanics.** If a line needs a comment saying what it
  does, the line is wrong. Existing comments say why the lock is taken there, why the checkpoint
  is skipped, why the number is 256.
- **Dependencies are rationed.** `nachalnik` has five (`async-trait`, `parking_lot`, `serde`,
  `serde_json`, `tokio` with `rt` and `sync` only) and is not to grow a sixth without a reason
  worth writing down. All versions are declared in the workspace manifest so two members cannot
  end up on two versions of the same thing; every non-obvious one carries a comment saying why it
  is there.
- **`#[non_exhaustive]`** on every public enum that names things the world can add to: `Event`,
  `Error`, `State`, `Delta`, `Content`, `Role`, `StopReason`, `Capability`, `GrantSource`,
  `ContextKind`, `ContextState`, `Selector`, `Which`. A new variant is not a breaking change;
  forgetting the attribute on a new enum is. `Grant` and `Verdict` are deliberately without it:
  allow/deny and allow/ask/deny are the whole of what a decision can be, and `Verdict::strictest`
  is an ordering over exactly three.

  **And on a struct the kernel answers with, which is the half that is easy to miss.** `Budget`,
  `Usage`, `ModelInfo`, `ToolOutput`, `CompactionPlan`, `Projection` and `Skipped` all look like
  answers and are not: each is built by somebody implementing one of the six traits, and closing
  them would mean a downstream crate needing a core change to do an ordinary thing. What is left -
  `StateChange`, `Record`, `Removed`, `CompactionReport`, and `Going` over in `kamchatka` - is
  produced here and read there, so the attribute costs nothing and makes the next field a patch.
  The question to ask is not "is this an output" but **"does anything outside this crate build
  one"**, and `grep` answers it.

  The attribute on an enum does *not* cover its variants: a struct-like variant gaining a field
  breaks every caller who wrote the pattern out, which is what `Event::ModelFailed` gaining
  `overrun` did. Marking the variants would fix that and cost more than it is worth - measured,
  54 patterns in this workspace would need `..`, and 26 places in `kamchatka`'s screen suite that
  build an event to drive the app could not build one at all, since the attribute closes
  construction from outside the crate. A field on an `Event` variant is a break, it is rare, and
  the version number is where it is said.
- **One word per mechanism, and it is the word the result is read back in.** An output limit
  **truncates**, a compactor **elides**, `/exclude` **excludes**, `Kernel::supersede`
  **supersedes**. A second word for something that already has one is a second thing to learn and
  a thing two parts of the program can disagree about, and it always shows up in the same place:
  somebody does an operation under one name and reads the result under another. `context` had a
  `prune` action with a `state` argument, which put the word for *one* move over five of them -
  `pin` and `restore` included, so "prune to pin it" was the documented way to protect
  something - and an item you pruned then read back as `archived` on every screen that listed it.
  Two live models in a row spent a call each asking for `restore` as an action, were told it was a
  state and not an action, and gave up; they were right and the levels were wrong. The four moves
  are actions now, named for the state each leaves behind.

  This is about what the program **says**, not what it accepts. Taking a word somebody reached for
  costs nothing and refusing it costs them a turn, so `unelide`, `unpin`, `include`, the old
  `prune` and `/keep` all still work and none of them is documented. A synonym in an enum, a help
  line or a message is the bug; a synonym in a `match` is a kindness.
- **Seams identify themselves.** `Projector`, `TokenCounter`, `PermissionPolicy` and `Compactor`
  each carry a `name()` defaulting to the implementing type's path, so a client can put the six
  seams on a screen (`/seams` in `kamchatka`). It is for showing a person, not for matching on.
- **A test's worth is measured, not assumed, and the measurement is one command.** Break the
  thing the test is about, run `cargo test --workspace --all-features --no-fail-fast`, and read
  *which* tests failed rather than whether any did. The question worth asking is never "was it
  caught" - it is "did anything **other** than the new test catch it", because a test that only
  duplicates coverage costs CI time and buys a false sense of a well-guarded seam.

  This has caught three different mistakes here, none of which reading the test would have found.
  A property whose generators never reached the case it was named after, so breaking that case
  failed nothing at all - which happened four times, and is why the generated suites carry a
  reachability check of their own. Two hand-written cases that duplicated
  `tests/context/undo.rs`, deleted once measured, because a duplicated case passes and reads
  exactly like coverage. And a mutation that had *not compiled*, which any script grepping for
  failing tests reports as a green suite - so build first, and treat "nothing failed" as three
  possibilities rather than one.

  `scripts/mutate.sh <patch> [pattern]` is the mechanics, and the only script in here: it refuses
  a dirty tree (a mutation goes into the working tree and comes back out of it), builds before it
  tests, takes a patch so that `git apply -R` reverts exactly what went in, and splits the
  failures into the tests being measured and everything else. The mutations themselves are not
  committed - they are ad hoc per investigation and a patch rots as soon as its context moves.

  `--no-fail-fast` is not optional. `cargo test` stops after the first failing test *binary*, so
  without it you see one binary's failures and nothing after them - which reads as "only the old
  tests caught this" and is the most misleading shape the answer can take.

  A test that is measured and found redundant is not automatically wasted: the projection
  invariants duplicate a lot and still found the ordering bug that a suite of five hundred and
  ninety-seven passing tests could not see. The point is to know which half of that is true
  before writing the changelog entry.
- **Before writing a test, look for it.** `tests/context/undo.rs`'s module note says what counts
  as one operation; two cases derived from `Kernel::undo`'s doc comment were written anyway, and
  both already existed there. Reading the source's own prose is a good way to find an invariant
  and a bad way to find out whether it is already checked.
- **Property suites keep no seed file.** `failure_persistence` is `None` in every one of them, and
  `**/proptest-regressions/` is ignored besides. A failure is reproduced by lifting the
  counterexample it printed into a named case with a note saying what it was, which is what every
  other test in here looks like; a file of opaque hashes is a regression suite nobody can read.
  The cost is that the seed is random each run, so a latent bug surfaces on some later run rather
  than the one that introduced it - accepted, because a suite that only ever tries the same cases
  is the thing generation was supposed to replace.
- **Changelogs** are per crate (`nachalnik/`, `nachalnik-mcp/`, `nachalnik-providers/`,
  `nachalnik-eval/`, `kamchatka/`), Keep a Changelog format, and are expected to be current
  before a release rather than reconstructed after one.
- **Which number moves is a fact about the public API, not about how the work felt.** Cargo reads
  `0.x.y` with the middle number as the major - `^0.3.2` resolves to `>=0.3.2, <0.4.0` - so `x` is
  the compatibility boundary and `y` carries everything a 1.0 crate would split between a minor and
  a patch. A change a caller cannot compile through bumps `x` and resets `y`; everything else, new
  API included, bumps `y`. A changelog heading decides nothing: `### added` is not a minor, and
  `nachalnik` 0.3.2 added `ModelResponse::thinking` and was right to be a patch, while `kamchatka`
  0.6.0 took `App::new`'s arguments apart and was right not to be one.

  Breaking is an item removed, a signature or a public field changed, a required method added to a
  trait, or a variant added to an enum that is not `#[non_exhaustive]` - which is what that
  attribute is on every public enum for. Not breaking: a new item, a variant on a
  `#[non_exhaustive]` enum, or a *defaulted* trait method - that last one with the caveat cargo's
  own reference gives it, since an implementor already carrying the name gets an ambiguity error
  rather than a default.

  Read it off the API rather than off the commit log or the diff. `cargo public-api --diff` where
  it is installed; otherwise `git worktree add` the last tag, run
  `cargo doc --workspace --all-features --no-deps` in both trees with separate `CARGO_TARGET_DIR`s,
  and compare every `item-decl` block and `code-header` in the HTML - **keyed by the page it is
  on**, or two identically-signed methods on different types cancel out and the comparison comes
  back empty. That is not hypothetical: it hid `ModelResponse::thinking` behind
  `ContextItem::thinking` and reported the release that added one as a release that added nothing.
  A comparison that finds no change at all across a cycle with entries in its changelog is a
  broken comparison until proven otherwise.

- **A version moves as soon as something above it needs API the registry does not have.**
  `cargo package --workspace` builds each member from its own tarball, and a tarball carries
  version requirements rather than path dependencies - so `kamchatka` is resolved against whatever
  `nachalnik` the registry has, unless the requirement names one it does not. What that costs when
  it goes unnoticed is two incompatible copies of the runtime in one build: `kamchatka` depends on
  the bridge as well as on the runtime, so a bridge left at a version already on the registry brings
  the `nachalnik` *it* was published against down beside the one the workspace is building. Bumping
  to a version that is not published yet is what makes cargo reach for the crate next door, and the
  `package`
  job is the only thing in CI that notices: everything else builds the workspace, where the path
  dependency always wins. Every time this has happened it is the one that has said so, and it has
  happened on every runtime bump so far. Bump in a commit of its own that names what made it
  necessary, and check whether the crates in between have to follow - a *minor* moves the floor
  under `nachalnik-mcp` and the bridge has to be re-cut against it, a *patch* does not, since a
  published `^0.3.0` resolves to `0.3.1` on its own and a release with nothing behind it is not
  one.
- **A bump belongs to the release when nothing forced one earlier.** The bullet above is about a
  version that has to move mid-cycle, and most do not - so at a release, every crate carrying a
  non-empty `[unreleased]` section gets a bump, in a commit of its own, numbered by the comparison
  above; a crate with nothing unreleased gets neither a bump nor a tag. Do it before the release
  commit, so that the commit which dates the changelogs is the commit that gets tagged, and the
  number in each tag is the one the comparison justified.
- **A release is its own commit and it only dates the changelogs.** No code moves in it; the
  version numbers moved when they had to. It is the commit that gets tagged - annotated,
  `<crate>-v<version>` per crate that moved, plus a workspace `v<version>` taking the runtime's
  number - and the bump commits before it are deliberately left untagged.
- **One of those tags builds a binary.** `kamchatka-v*` starts `.github/workflows/release.yml`,
  which creates the GitHub release with that version's section of `kamchatka/CHANGELOG.md` as its
  body and attaches a static `x86_64-unknown-linux-musl` build with a `sha256` beside it. The
  other crates are libraries and their artifact is the crates.io tarball, which the `package` job
  already checks; the workspace `v*` tag builds nothing, since it would be the same binary under
  a name that does not say so.

  **Two files in the archive: the binary and `kamchatka.json`.** The settings file is worth more
  beside the binary than on a web page, because `cargo install` copies no files and an archive is
  the only way it reaches somebody who did not clone anything. Nothing else is, and the reasoning
  is the same in reverse: a copied document goes out of date in a downloads folder while the one
  it was copied from is corrected, and the readme, the guide, these notes and the changelog are a
  link away and always current.

  The changelog section has to exist under the number being tagged or the job fails, which is one
  more reason the release commit is the one that dates the changelogs. Static musl rather than
  glibc so that the download runs wherever the kernel is new enough rather than wherever the
  distribution is - and the confinement survives it: the whole of `kamchatka`'s suite passes
  against that target, the Landlock tests included, which is worth re-checking before trusting a
  single file that claims to sandbox what it runs. The only thing in the tree needing a cross
  toolchain is `ring`, which compiles C; rust ships its own musl libc for the rest.

  **Run it on `workflow_dispatch` before tagging.** It builds and packages and uploads nothing,
  which is how to find out that a musl toolchain, a C dependency and somebody else's action still
  work on a day that is not the day of the release.
- **Commit messages** are `crate: what changed, in one lowercase line`, followed by prose
  explaining what was wrong, what was decided, and what was checked - including what was
  deliberately *not* done and why. Read `git log` before writing one; the bar is high and
  consistent.
- **No counting the repository.** Do not put a test count, a line count or a percentage of one
  file against another into the prose. They are true for one commit, nothing checks them, and
  every one of them in this workspace had drifted or was wrong on the day it was written - a
  readme claiming 482 tests over a tree with 512, and `kamchatka` described as a couple of thousand
  lines when it was already over ten. The cost is not the wrong number, it is that a reader who
  finds one wrong stops believing the numbers that *are* measurements. Measurements stay: what a
  request cost, what a counter guessed against what a provider charged, what a model did in a
  recorded session, what a run of the eval suite spends. Those were taken against something real,
  they are dated by the commit they were written for, and they do not change when somebody adds a
  test. Anyone who wants a count has `cargo test --workspace` and the tree.
- **The prose argues.** Headings are lowercase, sentences are sentences, and a paragraph that
  merely lists what a thing has is not finished. Spelling leans British (`behaviour`, `defence`,
  `optimisation`, `honouring`) with `-ize` endings for `summarize`. Rust source uses hyphens; the
  `README.md`s use em dashes.
- **And it argues plainly.** Every document here - readmes, guides, changelogs, doc comments,
  `note:` paragraphs - is written for somebody deciding what to do next, and the test is whether a
  sentence changes that decision. What does not: the story of how a bug was found, the measurement
  from the one run that found it, the alternatives weighed and dropped, a verdict on how good a
  test was, and a closing line that restates the opening one for effect. Keep the fact, the
  consequence, and the one clause that stops somebody undoing it by mistake.

  Say a number where the number is the fact - a default, a limit, a size a reader will meet - and
  not where it is a souvenir of one afternoon. Name a model or an endpoint where the behaviour is
  that endpoint's, and not to date the anecdote. A rule this file states is worth one line
  wherever it is restated, not a second copy of the argument.

  This applies hardest to the things that are read most and edited least: a `note:` on a hot
  function, a tool description a model pays for on every request, the first screen of a readme.
  Length there is not thoroughness, it is a toll.

- **And nothing in it talks about it.** The other failure is not length, it is a sentence that
  steps outside the content to comment on it. The ones that got written here, and were cut:

  - **The announcement**, telling the reader the shape of what is coming. "Four things differ,
    and three of them are the right way round", over a list of four things. "What was looked
    into, so that nobody looks again", over the thing that was looked into.
  - **The verdict**, telling them which part to be impressed by. "It is `sandbox-exec` or
    nothing, then". "Which is the point", "and that is the whole of it".

  Delete the clause; the sentence is still true and the paragraph is shorter. That is the test,
  and the second one is reading it aloud - nobody says "four things differ, and three of them are
  the right way round" to somebody at the next desk, and prose in a voice nobody speaks in has to
  be translated before it can be used.

  The reversal is not this, and stays: "what would unblock it is not a counter, it is a decision
  about where a picture reaches the model" carries a fact in each half and is how a correction
  gets stated here.

---

## gotchas

- `Kernel` is `Clone` and cheap - it is an `Arc` handle. It is **not** `Drop`-safe against cycles:
  a `Kernel` stored inside a `Tool`, `Provider` or `PermissionPolicy` that the same kernel holds
  keeps everything alive. Store a `Weak`, or drop the components.
- `Kernel::with_context` holds the context read lock for the whole closure. The closure must not
  call back into the kernel.
- **Emit while still holding the lock that made the change** - the machine lock for a transition,
  the context lock for anything the context did. Announcing after the release looks tidier and is
  wrong: two threads changing the same item apply in one order and get logged in the other. The
  lock order is machine → context → session and nothing goes back up it; `emit` takes the session
  lock and nothing else, and a broadcast `send` runs no subscriber code.
- The `test` and `selectors` features are off by default but on for `nachalnik`'s own tests, via a
  dev-dependency on itself. `cargo build -p nachalnik` is the configuration users get, and CI
  checks it separately for that reason.
- `parallel_tool_calls` is the only place the kernel spawns tasks, and only for the length of the
  step that spawned them. Serial is the default because the order the model asked in is something
  callers build on.
- A `Provider`'s `render` is its own account of itself, and the kernel has nothing to check it
  against. Render once and send what was rendered; a `preview_payload` that has quietly stopped
  matching is worse than none.
- MCP tool annotations are hints from a server that may not be trusted. `Trust` believes none of
  them by default, and the bridge's tests include a `delete_everything` that claims to be
  read-only. Do not "fix" that.
- **`cargo package` lies to you the second time you run it on an unpublished version.** It unpacks
  the tarball it has just built into `~/.cargo/registry/src/` under that name and version, and
  builds an rlib for it in `target/`. A registry crate is immutable by assumption, so neither is
  invalidated when the same version number packages different bytes: change the runtime, package
  again, and the second run compiles the first run's code. What that looks like is
  `no method named ... found for struct Kernel` against a tarball which demonstrably contains the
  method - before believing the compiler, read it out of the tarball cargo is actually resolving:
  `tar -xzOf target/package/tmp-registry/nachalnik-0.3.1.crate nachalnik-0.3.1/src/kernel.rs`.
  `cargo clean -p nachalnik` is the fix. CI has one run on a fresh machine and never sees this,
  which is exactly why it costs an afternoon here instead.
