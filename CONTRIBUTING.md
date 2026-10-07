# working in this workspace

What to run, what CI runs, the conventions, and the known pitfalls. [AGENTS.md](AGENTS.md) has the
short version.

See also [INVARIANTS.md](INVARIANTS.md) for what must not break, [MAP.md](MAP.md) for where things
are, [SECURITY.md](SECURITY.md) for the security model, and [POSTPONED.md](POSTPONED.md) for what is
deliberately not built yet.

---

## commands

```console
cargo nextest run --workspace --all-features   # everything, as CI runs it; `--profile ci` to match
cargo test --workspace --all-features --doc    # the doc tests, which nextest doesn't run
cargo test --workspace --all-features --no-fail-fast   # when checking what a test catches
cargo test -p kamchatka --no-default-features          # the program without a screen or MCP
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo doc --workspace --all-features --no-deps   # with RUSTDOCFLAGS=-D warnings, as CI does
scripts/references.sh                       # every file and test the docs name still exists
scripts/windows.sh                          # the libraries as CI builds them on Windows
```

Run these before committing, not just in CI. **The `cargo doc` one is the one people skip**, and
without `RUSTDOCFLAGS='-D warnings'` it prints warnings and exits `0`. Nothing else checks doc
comments, so a broken intra-doc link (a wrong name, or a public comment linking to a `pub(super)`
item) is only caught there or in CI.

`scripts/windows.sh` builds the libraries for Windows using `cargo xwin` (`kamchatka` only builds
for Linux). It catches compile-time problems, such as a helper that is dead code because all its
callers are `#[cfg(unix)]`, but not runtime differences; so tests that wait on the operating system
should wait for the actual event, not a fixed time.

`scripts/references.sh` checks what rustdoc doesn't: every file, test or item name in backticks
must exist. Changelogs are skipped, and names mentioned on purpose go in
`scripts/references.allow`.

CI (`.github/workflows/ci.yml`) also builds with default features (the tests enable every feature,
so nothing else checks the default build), checks the published crates with
`--no-default-features`, runs the examples that need no key, checks the dependency tree against
`deny.toml`, and checks the workspace on the MSRV, **1.95**, locked and with every direct dependency
at its minimum version (`cargo minimal-versions`). Edition 2024, with `-D warnings` everywhere.
Every action is pinned to a commit; run `zizmor .github` after changing a workflow. The MSRV may be
raised in any minor release, to a toolchain that has been stable for about six months.

---

## the live tests

They're the only way to check that a real API accepts what this workspace sends, and each skips
itself without a key. Run them before a release and after any change to how requests are built.

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --test-threads=1 --nocapture
```

- **`nachalnik`'s** read `OPENROUTER_API_KEY` (only when the base URL is OpenRouter's) or
  `NACHALNIK_API_KEY`, plus `NACHALNIK_BASE_URL`, `NACHALNIK_TEST_MODEL` and
  `NACHALNIK_CONTEXT_LIMIT`. `nachalnik-eval`'s and `nachalnik-mcp`'s read the same.
- **`kamchatka`'s** read `KAMCHATKA_API_KEY`, `KAMCHATKA_BASE_URL` and **`KAMCHATKA_TEST_MODEL`**,
  not the binary's `KAMCHATKA_MODEL`. Using the wrong one falls back to the default model, and the
  tests then fail on tool calls rather than on the model name. They also need
  `KAMCHATKA_CONTEXT_LIMIT` small enough for the compaction test to exceed (`12288` works),
  `KAMCHATKA_DOCUMENT_MODEL` for the PDF test, and `KAMCHATKA_GEMINI_API_KEY` for the tests of
  Google's own API.
- **`nachalnik-providers`'** `anthropic_live` and `responses_live` read the vendors' own keys;
  `system1` reads `NACHALNIK_SYSTEM1_MODEL`, and `kamchatka`'s `advise` reads
  `KAMCHATKA_SYSTEM1_MODEL` (the `system1` module's docs list which servers they've been run
  against).

Pitfalls:

- **A run without a key compiles the live tests but skips them**, so the compiler catches a removed
  field, but nothing catches an assertion that reads the wrong one of two similar fields. When a
  change updates the offline tests, search `tests/live.rs` for the same name.
- **A skipped test counts as passed**, and the summary doesn't say what was skipped. Use
  `--nocapture` and search for `skip` to see what ran.
- **`kamchatka`'s tests default to Google's endpoint**, so a run with only a key set sends that key
  to Google and fails for unrelated reasons. Set `KAMCHATKA_BASE_URL` for any other endpoint.
- **`NACHALNIK_CONTEXT_LIMIT` is for `kamchatka`'s tests, not `nachalnik`'s.** In `nachalnik`'s it
  makes the runtime report a context window the endpoint doesn't enforce, and
  `a_counter_is_told_what_a_refused_request_came_to` fails because nothing gets refused.
- **Suspect the model before the code when a live test fails.** Small and free models fail tests
  that ask them to *do* things, and not the same ones each time; free tiers return `429`s and time
  out. Re-run, try a second model, and try the last release tag (`git worktree add`) before
  concluding anything. `an_interrupt_stops_a_stream_that_is_watching` can't pass on a model that
  sends its whole answer in one fragment, since there's no stream left to stop.
- **Some endpoints truncate tool calls**, which looks like the model writing broken JSON. Check
  `/raw` before blaming our code: the accumulator in `openai/wire.rs` could misassign a fragment to
  the *next* call, but never drops one.
- **Some endpoints reject what others accept**: a `file` content part, a request ending with a
  model turn, a model their listing still includes. Each test's note says where this was seen.

---

## the test suites

Each crate's `tests/` has one file or directory per subject, named after it, and the comment at
the top of each says what it covers. Three things aren't obvious:

- **Tests of anything inside a provider's `respond` use a real socket**, because calling a parser
  from outside would test a copy of the code. Stream formats aren't tested separately per API:
  `nachalnik-providers/src/conformance.rs` is one suite every API client must pass, where each case
  is a bug that actually happened.
- **Ordering is tested by pausing the kernel at a specific point through an extension point** (a
  counter or provider that waits until released) and doing the other thing then, not by racing
  threads.
- **`kamchatka`'s binary is tested as a binary.** It builds its provider from the environment in
  its own process, so `common::endpoint` serves a real socket, the only way to fake an endpoint
  for a child process.

---

## sweeps, live runs and soaks

A model using the program finds bugs the tests don't: it names a file `~`, passes the wrong id, and
reads before it writes. The skills in `.claude/skills` run headless `kamchatka` sessions on the
workspace, each written for an agent such as Claude Code to follow. Each needs a **model id** and an
**API key**, and is started by asking for it by name: "a full repo sweep using kamchatka and model
X", "a mutants sweep", "live runs with model X", "a soak with model X".

- **`repo-sweep`** audits the code, performance, quality, tests and docs, and measures how much
  trouble the tools give a model using them. Its mutants mode takes its work list from `cargo
  mutants` instead, has sessions write a test for each surviving mutant, and records every verdict
  in `.claude/skills/repo-sweep/verdicts.jsonl` so the next run skips the settled ones.
- **`live-runs`** runs many short scenarios, each aimed at one area, alongside a hostile fake
  endpoint and a logging proxy.
- **`soak`** runs one session across resumes, a SIGTERM, a SIGKILL, a full context, a checkpoint
  `/load` and an undo, then checks the whole chain of records with a checker that is first tested
  against deliberately corrupted records.

Use a different model each time: small and free models misuse tools in ways strong ones don't, and
that's what finds bugs. The same rules apply to all of them. Keep the key out of the repository. A
finding only counts once a test or a session record confirms it; many turn out to be the model's
fault, a documented decision, or the harness. Fixes go on their own branch with a test that fails
without the fix, and anything that needs a person to decide goes into
[POSTPONED.md](POSTPONED.md).

---

## conventions

- **Write plainly.** This applies to everything: documents, doc comments, `note:` paragraphs,
  commit messages, error messages and tool descriptions. Use ordinary words and short sentences,
  say things directly, and write the way you'd explain it to a colleague at the next desk. No
  riddles, no personified code, no aphorisms, and no sentences about the text itself ("four things
  differ, and three of them are the right way round", "which is the point"). If a sentence has to be
  read twice, rewrite it.
- **Keep what helps a reader decide what to do.** Keep the fact, the consequence, and whatever stops
  someone undoing it by mistake. Leave out how a bug was found, one-off measurements, alternatives
  that were dropped, and closing lines that repeat the opening. Include a number only if it's a
  default or a limit a reader will run into; name a model or endpoint only where the behaviour is
  specific to it, in that crate's own docs, not in a README. Longer isn't more thorough, and on tool
  descriptions every word costs tokens on every request.
- **`note:` paragraphs.** A doc comment says what something is; a paragraph starting with `note:`
  says why it's that way, what was rejected, or what it costs. `#![deny(missing_docs)]` and
  `#![deny(unsafe_code)]` are on in every published crate, and one module allows `unsafe`:
  `kamchatka::gate`, for seccomp calls that no crate wraps safely.
- **Comments explain decisions, not mechanics.** If a line needs a comment saying what it does,
  rewrite the line.
- **Dependencies are kept to a minimum.** `nachalnik` doesn't gain one without a reason worth
  writing down. Versions are declared in the workspace manifest so members can't drift apart, and
  any dependency whose purpose isn't obvious has a comment.
- **`#[non_exhaustive]`** on every public enum that may gain variants; adding a variant is then not
  a breaking change, but forgetting the attribute on a new enum is. An enum that covers every
  possible case (`Grant`, `Verdict`) goes without it. For structs, the question is **"does anything
  outside this crate construct one"**, which `grep` answers: a struct built by someone implementing
  a trait stays open, one produced here and only read elsewhere gets the attribute. It doesn't cover
  an enum's variants, so adding a field to an `Event` variant is a breaking change.
- **One word per mechanism, and it's the word the user sees.** An output limit **truncates**, a
  compactor **elides**, `/exclude` **excludes**, and `supersede` excludes the old item with a note
  naming the new one. That applies to what the program *accepts* as well as what it says: a command
  that changes the session has one name, and a tool's schema accepts no alternative spelling. Only
  housekeeping commands have aliases (`/quit` is also `/exit` and `/q`, `/help` is `/?`).
- **Extension points identify themselves** with a `name()`, which defaults to the type's path; it's
  for showing to people, not for matching on.
- **Check that tests catch something.** Break the code a test is about, run
  `cargo test --workspace --all-features --no-fail-fast`, and look at *which* tests failed: the
  question is whether anything **other** than the new test caught it.
  `scripts/mutate.sh <patch> [pattern]` applies a mutation, builds first (a mutation that doesn't
  compile would otherwise look like a passing suite), runs the tests, and reverts.
  `--no-fail-fast` matters: without it you only see one binary's failures. A redundant test isn't
  necessarily useless, but know whether it's redundant before the changelog says otherwise.
- **Before writing a test, look for an existing one.** The source's comments are a good way to find
  an invariant but a bad way to find out whether it's already tested.
- **Property tests keep no seed file.** A failure is turned into a named test case with a note; a
  file of opaque hashes is a regression suite nobody can read.
- **Changelogs** are per crate, follow Keep a Changelog, and are kept up to date before a release
  rather than reconstructed after.
- **Which version number changes depends on the public API.** In `0.x.y`, `x` is the compatibility
  boundary: a change that can break a caller's build bumps `x`, everything else (including new API)
  bumps `y`. Breaking changes are: removing an item, changing a signature or public field, adding a
  required trait method, or adding a variant to an enum that isn't `#[non_exhaustive]`. Check the
  API with `cargo semver-checks --workspace --all-features` first, and the changelog for what it
  can't see (a removed feature, the MSRV, a change in behaviour). If that's not enough, compare the
  `cargo doc` HTML of the last tag and this tree page by page; otherwise two methods with the same
  signature on different types cancel each other out.
- **Bump a version as soon as a crate above it needs API that isn't on the registry yet.**
  `cargo package --workspace` resolves each member against the registry, so `kamchatka` would
  build against the published `nachalnik` and, through the MCP bridge, a second copy as well. Bump
  in a separate commit that names the reason; CI's `package` job is the only thing that notices. A
  *minor* bump raises the minimum version `nachalnik-mcp` requires, so it has to be released again;
  a *patch* bump doesn't.
- **At a release**, every crate with a non-empty `[unreleased]` section gets a version bump in its
  own commit, then **the release commit only dates the changelogs** and is the one that's tagged,
  with annotated tags: `<crate>-v<version>` for each crate that changed, plus a workspace
  `v<version>` with the runtime's version.
- **`kamchatka-v*` tags build the binaries.** `.github/workflows/release.yml` creates the release
  with that version's changelog section as its description (the job fails if there isn't one) and
  attaches static x86_64 and aarch64 musl builds, each with a `sha256`, an attestation
  (`gh attestation verify ARCHIVE --repo ljedrz/nachalnik`), and `kamchatka.json`. Each binary is
  built twice from different paths and must match byte for byte; the release notes give the
  `rustc -V` to reproduce it with:

  ```console
  RUSTFLAGS="--remap-path-prefix=$HOME/.cargo=/cargo" cargo +VERSION auditable build --release \
    --locked -p kamchatka --bin kamchatka --features shell-advisor \
    --target x86_64-unknown-linux-musl
  ```

  Run the workflow manually (`workflow_dispatch`) before tagging; it builds but uploads nothing.
- **Commit messages** are `crate: what changed, in one lowercase line`, followed by prose saying
  what was wrong, what was decided, what was checked, and what was deliberately left out. Read
  `git log` first.
- **No counting the repository.** No test counts, line counts or percentages in prose: nothing
  checks them, they go stale, and a reader who finds one wrong stops trusting the real
  measurements. Measurements (what a request cost, the counter's estimate against what was
  charged) are fine.
- **No copies of the program's output.** A pasted screen shows one session and goes out of date.
  Describe what the screen shows and what each part means instead; commands a person types,
  settings, code and design diagrams are fine.
- **Style.** Lowercase headings. British spelling (`behaviour`, `defence`), but `-ize` for words
  like `summarize`. Rust source and the workspace's own documents use hyphens; the `README.md`s and
  the guides next to them use em dashes.

---

## pitfalls

- `Kernel` is a cheap `Arc` handle and **not** safe against reference cycles: a `Kernel` stored
  inside a `Tool`, `Provider` or `PermissionPolicy` held by the same kernel keeps everything alive.
  Store a `Weak` instead.
- `Kernel::with_context` holds the context read lock for the whole closure, so the closure must not
  call back into the kernel.
- **Emit events while still holding the lock that made the change**, or two threads changing one
  item get logged in the wrong order. The lock order is machine → components → context → session,
  and nothing takes a lock earlier in that order while holding a later one; code that needs a
  component and the context to agree holds both. So a `Provider`'s `info`, a component's `name`, a
  `Tool`'s `spec`, a `Projector`'s `project` and a counter's `recalibrate` must not call back into
  the kernel.
- **A lock guard lives until the end of the statement that created it**, so a struct literal or
  tuple that reads two locked fields holds both locks at once, and two such reads in opposite orders
  occasionally deadlock. Read each lock into its own local variable.
- `test` and `selectors` are off by default but enabled for `nachalnik`'s own tests through a
  dev-dependency on itself; CI builds the plain configuration separately.
- `parallel_tool_calls` is the only place the kernel spawns tasks. Running calls in order is the
  default, because callers rely on calls running in the order the model asked for them.
- MCP tool annotations are hints from a server that may not be trustworthy, and `Trust` believes
  none of them by default. Don't "fix" that.
- **`cargo package` gives wrong results the second time you run it on an unpublished version.** It
  caches the tarball and its compiled library under that version, and assumes registry crates never
  change, so the second run compiles the first run's code, giving errors like `no method named ...`
  for a method the tarball does have. Fix it with `cargo clean -p nachalnik`.
