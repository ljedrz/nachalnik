# working in this workspace

What to run, what CI runs, the house conventions, and the things that have cost somebody an
afternoon. [AGENTS.md](AGENTS.md) has the short form of all of it.

See also [INVARIANTS.md](INVARIANTS.md) for what must not be broken, [MAP.md](MAP.md) for where
things live, [SECURITY.md](SECURITY.md) for the security position, and
[POSTPONED.md](POSTPONED.md) for what is deliberately not built.

---

## commands

```console
cargo nextest run --workspace --all-features   # everything, as CI runs it; `--profile ci` to match
cargo test --workspace --all-features --doc    # and the doctests, which nextest does not run
cargo test --workspace --all-features --no-fail-fast   # when measuring what a test is worth
cargo test -p kamchatka --no-default-features          # the program with no screen and no MCP
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo doc --workspace --all-features --no-deps   # with RUSTDOCFLAGS=-D warnings, as CI does
scripts/references.sh                       # every file and test the prose names still exists
scripts/windows.sh                          # the libraries as CI builds them on Windows
```

These are pre-commit checks, not just CI steps. **The `cargo doc` one is the one that gets
skipped**, and without `RUSTDOCFLAGS='-D warnings'` it prints its warnings and exits `0`. Nothing
else reads a doc comment, so a broken intra-doc link - a name the item never had, or a public
comment linking to a `pub(super)` item - is caught there or by CI and nowhere in between.

`scripts/windows.sh` builds the libraries for Windows from here with `cargo xwin` (`kamchatka`
builds for Linux only). It catches what the compiler sees - a helper whose only callers are
`#[cfg(unix)]` is dead code there - and not what Windows does differently at run time, so a test
that waits on the operating system waits for the thing it is about, not for a fixed time.

`scripts/references.sh` checks the prose rustdoc does not read: a backticked file, test or item
name has to exist. Changelogs are skipped, and a name mentioned on purpose goes in
`scripts/references.allow`.

CI (`.github/workflows/ci.yml`) also builds with default features (the tests turn every feature
on, so nothing else exercises that configuration), checks the published crates with
`--no-default-features`, runs the keyless examples, holds the tree to `deny.toml`, and checks the
workspace on the MSRV, **1.95**, locked and with every direct dependency at its floor
(`cargo minimal-versions`). Edition 2024, `-D warnings` throughout. Every action is pinned to a
commit; run `zizmor .github` after touching a workflow. The MSRV may rise in any minor release, to
a toolchain that has been stable for about six months.

---

## the live suites

They are the only thing that can check that a real API accepts what this workspace builds, and
each skips itself without a key. Run them before a release, and after any change to the request
path.

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --test-threads=1 --nocapture
```

- **`nachalnik`'s** reads `OPENROUTER_API_KEY` (only where the base URL is OpenRouter's) or
  `NACHALNIK_API_KEY`, with `NACHALNIK_BASE_URL`, `NACHALNIK_TEST_MODEL` and
  `NACHALNIK_CONTEXT_LIMIT`. `nachalnik-eval`'s and `nachalnik-mcp`'s read the same.
- **`kamchatka`'s** reads `KAMCHATKA_API_KEY`, `KAMCHATKA_BASE_URL` and **`KAMCHATKA_TEST_MODEL`** -
  not the binary's `KAMCHATKA_MODEL`; the wrong one falls back to the suite's default model, and the
  tests then fail about tool calls rather than about the name. It also wants
  `KAMCHATKA_CONTEXT_LIMIT` small enough for the compaction fixture to breach (`12288` works),
  `KAMCHATKA_DOCUMENT_MODEL` for the PDF test, and `KAMCHATKA_GEMINI_API_KEY` for the tests of
  Google's native dialect.
- **`nachalnik-providers`'** `anthropic_live` and `responses_live` read the vendors' own keys;
  `system1` reads `NACHALNIK_SYSTEM1_MODEL`, and `kamchatka`'s `advise` `KAMCHATKA_SYSTEM1_MODEL`
  (the `system1` module's docs say which engines they have been run against).

Traps:

- **A keyless run compiles the live tests and skips them**, so the compiler catches a field that is
  gone and nothing catches an assertion still reading the wrong one of two. When a change brings
  the offline suites along, grep `tests/live.rs` for the same name.
- **A skipped test passes**, and the summary does not say what was skipped. `--nocapture` and a grep
  for `skip` is how to see what ran.
- **`kamchatka`'s suite defaults to Google's endpoint**, so a run with only a key set posts that key
  to Google and fails about everything but the endpoint. Set `KAMCHATKA_BASE_URL` for anything else.
- **`NACHALNIK_CONTEXT_LIMIT` is for `kamchatka`'s suite, not `nachalnik`'s.** There it makes the
  runtime report a window the endpoint does not enforce, and
  `a_counter_is_told_what_a_refused_request_came_to` fails because nothing was refused.
- **Attribute a live failure to the model before the code.** Small and free models fail tests that
  ask them to *do* things, and not the same ones twice; free tiers return `429`s and idle timeouts.
  Re-run it, run it on a second model, and run the last tag (`git worktree add`) before believing
  anything. `an_interrupt_stops_a_stream_that_is_watching` cannot pass on a model that delivers its
  whole answer in one fragment, since there is no stream left to stop.
- **An endpoint can truncate a tool call** so that it reads as the model writing broken JSON. Read
  `/raw` before blaming this end: the accumulator in `openai/wire.rs` misfiles a fragment onto the
  *next* call, never drops one.
- **Some endpoints refuse what others take** - a `file` content part, a request ending with a model
  turn, a model their listing still names. The test's own note says where it has been seen.

---

## the test suites

Every crate's `tests/` is one file or directory per subject, named for it, and the module note at
the top of each says what it covers. Three things about them are not obvious:

- **A test of something inside a provider's `respond` talks to a real socket**, because a parser
  called from outside it tests a copy of the code. The shapes a stream arrives in are not tested per
  dialect: `nachalnik-providers/src/conformance.rs` is one suite every dialect is held to, each
  case a bug that happened.
- **A claim about ordering is tested by holding the kernel at one moment through a seam** - a
  counter or a provider that waits to be let go - and doing the other thing then, not by racing
  threads.
- **`kamchatka`'s binary is tested as a binary.** It builds its provider from the environment in a
  process of its own, so `common::endpoint` answers on a socket, which is the one seam a child has.

---

## conventions

- **`note:` paragraphs.** A doc comment states what something is; a paragraph beginning `note:`
  states why it is that way, what was rejected, or what it costs. Match the style.
  `#![deny(missing_docs)]` and `#![deny(unsafe_code)]` are on in every published crate, and one
  module allows `unsafe` back: `kamchatka::gate`, for the seccomp calls nothing wraps safely.
- **Comments explain the decision, not the mechanics.** If a line needs a comment saying what it
  does, the line is wrong.
- **Dependencies are rationed.** `nachalnik` does not grow one without a reason worth writing down.
  Versions are declared in the workspace manifest so two members cannot drift apart, and every
  non-obvious one carries a comment saying why it is there.
- **`#[non_exhaustive]`** on every public enum the world can add to; a new variant is then not a
  breaking change, and forgetting the attribute on a new enum is. An enum that is the whole of what
  can be - `Grant`, `Verdict` - goes without it. On structs the question is **"does anything outside
  this crate build one"**, and `grep` answers it: a struct built by somebody implementing a trait
  stays open, one produced here and read there is closed. The attribute does not cover an enum's
  variants, so a field on an `Event` variant is a break, and the version number says so.
- **One word per mechanism, and it is the word the result is read back in.** An output limit
  **truncates**, a compactor **elides**, `/exclude` **excludes**, and `supersede` excludes the old
  item with a note naming the new. That goes for what the program *accepts* as well as what it says:
  a command that does something to the session has one name, and a tool's schema takes no second
  spelling. Only the janitorial commands have aliases (`/quit` is `/exit` and `/q`, `/help` is
  `/?`).
- **Seams identify themselves** with a `name()` defaulting to the type's path, for showing a person,
  not for matching on.
- **A test's worth is measured, not assumed.** Break the thing it is about, run
  `cargo test --workspace --all-features --no-fail-fast`, and read *which* tests failed: the question
  is whether anything **other** than the new test caught it. `scripts/mutate.sh <patch> [pattern]`
  applies a mutation, builds first (a mutation that did not compile reads as a green suite),
  tests, and reverts. `--no-fail-fast` is not optional: without it you see one binary's failures.
  A redundant test is not automatically wasted; know whether it is before the changelog says so.
- **Before writing a test, look for it.** The source's prose is a good way to find an invariant and
  a bad way to find out whether it is already checked.
- **Property suites keep no seed file.** A failure is lifted into a named case with a note; a file
  of opaque hashes is a regression suite nobody can read.
- **Changelogs** are per crate, Keep a Changelog, and current before a release rather than
  reconstructed after one.
- **Which number moves is a fact about the public API.** In `0.x.y`, `x` is the compatibility
  boundary: a change a caller cannot compile through bumps `x`, everything else - new API included
  - bumps `y`. Breaking is an item removed, a signature or public field changed, a required trait
  method added, or a variant on an enum that is not `#[non_exhaustive]`. Read it off the API:
  `cargo semver-checks --workspace --all-features` first, and for what it cannot see - a dropped
  feature, the MSRV, a behaviour change - the changelog. Where more is needed, compare the
  `cargo doc` HTML of the last tag and this tree, keyed by page, or two identically-signed methods
  on different types cancel out.
- **A version moves as soon as something above it needs API the registry does not have.**
  `cargo package --workspace` resolves each member against the registry, so `kamchatka` would build
  against the published `nachalnik` and, through the bridge, a second one beside it. Bump in a
  commit of its own naming what made it necessary; the `package` job in CI is the only thing that
  notices. A *minor* moves the floor under `nachalnik-mcp`, which has to be re-cut; a *patch* does
  not.
- **At a release**, every crate with a non-empty `[unreleased]` gets a bump in a commit of its own,
  then **the release commit only dates the changelogs** and is the one tagged - annotated,
  `<crate>-v<version>` per crate that moved, plus a workspace `v<version>` with the runtime's
  number.
- **`kamchatka-v*` builds the binaries.** `.github/workflows/release.yml` creates the release with
  that version's changelog section as its body (the job fails if there is none) and attaches static
  x86_64 and aarch64 musl builds, each with a `sha256`, an attestation
  (`gh attestation verify ARCHIVE --repo ljedrz/nachalnik`), and `kamchatka.json` beside it. Each
  binary is built twice from different paths and must match byte for byte; the release notes give
  the `rustc -V` to reproduce it with:

  ```console
  RUSTFLAGS="--remap-path-prefix=$HOME/.cargo=/cargo" cargo +VERSION auditable build --release \
    --locked -p kamchatka --bin kamchatka --features shell-advisor \
    --target x86_64-unknown-linux-musl
  ```

  Run it on `workflow_dispatch` before tagging: it builds and uploads nothing.
- **Commit messages** are `crate: what changed, in one lowercase line`, followed by prose saying
  what was wrong, what was decided, what was checked, and what was deliberately not done. Read
  `git log` first.
- **No counting the repository.** No test counts, line counts or percentages in prose: nothing
  checks them, they drift, and a reader who finds one wrong stops believing the numbers that are
  measurements. Measurements - what a request cost, what a counter guessed against what was
  charged - stay.
- **No captures of the program's output.** A pasted screen is a copy of one session and goes stale.
  Say what the screen holds and what each part means; what a person types, settings, code and
  design diagrams stay.
- **The prose argues.** Headings are lowercase and sentences are sentences. Spelling leans British
  (`behaviour`, `defence`) with `-ize` for `summarize`. Rust source uses hyphens; the `README.md`s
  use em dashes.
- **And it argues plainly.** Every document is written for somebody deciding what to do next, and
  the test is whether a sentence changes that decision. Keep the fact, the consequence, and the
  clause that stops somebody undoing it by mistake. Cut the story of how a bug was found, the
  measurement from the run that found it, the alternatives weighed and dropped, and a closing line
  restating the opening. A number earns its place as a default or a limit a reader will meet; a
  model or an endpoint is named where the behaviour is that endpoint's, in that crate's own docs,
  and not in a readme. Length is not thoroughness; on a tool description a model pays for every
  request, it is a toll.
- **And nothing in it talks about it.** No sentence stepping outside the content to announce it
  ("four things differ, and three of them are the right way round", over a list of four) or to
  grade it ("which is the point"). Delete the clause and the sentence is still true. Nobody says
  that to somebody at the next desk.

---

## gotchas

- `Kernel` is a cheap `Arc` handle, and **not** safe against cycles: a `Kernel` stored inside a
  `Tool`, `Provider` or `PermissionPolicy` the same kernel holds keeps everything alive. Store a
  `Weak`.
- `Kernel::with_context` holds the context read lock for the whole closure, which must not call back
  into the kernel.
- **Emit while still holding the lock that made the change**, or two threads changing one item are
  logged in the other order. The lock order is machine → the components → context → session, and
  nothing goes back up it; a reader needing a component and the context to agree holds both. So a
  `Provider`'s `info`, a component's `name`, a `Tool`'s `spec`, a `Projector`'s `project` and a
  counter's `recalibrate` must not call back into the kernel.
- **A guard lives to the end of the statement that took it**, so a struct literal or tuple reading
  two locked fields holds both at once, and two such reads in opposite orders deadlock now and then.
  Read each lock into a local of its own.
- `test` and `selectors` are off by default but on for `nachalnik`'s own tests through a
  dev-dependency on itself; CI builds the plain configuration separately.
- `parallel_tool_calls` is the only place the kernel spawns tasks. Serial is the default because the
  order the model asked in is something callers build on.
- MCP tool annotations are hints from a server that may not be trusted, and `Trust` believes none of
  them by default. Do not "fix" that.
- **`cargo package` lies to you the second time you run it on an unpublished version.** It caches
  the tarball and its rlib under that version, and a registry crate is assumed immutable, so the
  second run compiles the first run's code: `no method named ...` against a tarball that has the
  method. `cargo clean -p nachalnik` is the fix.
