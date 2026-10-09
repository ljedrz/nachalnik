# AGENTS.md

An introduction for anyone, person or model, about to change this workspace. The `README.md` files
say what the crates are *for*; this says how they are built, what must not break, and which design
decisions have already been made.

The details are elsewhere: [INVARIANTS.md](INVARIANTS.md) for the rules that must hold and why,
[MAP.md](MAP.md) for where everything lives, [CONTRIBUTING.md](CONTRIBUTING.md) for commands,
conventions and known pitfalls, [SECURITY.md](SECURITY.md) for what is and isn't enforced, and
[POSTPONED.md](POSTPONED.md) for what is deliberately not built yet and what would unblock it.

---

## the thing being built

`nachalnik` is an agent runtime in which the context, the tools, the permissions and the requests
are explicit state that a caller can read, change and put back. It is a library with no UI, no
model, no tools, no prompt, no filesystem and no network. It owns the loop, the context, and the
record of what happened; everything else is someone else's code behind a trait.

> The agent is not the boss. You are.

Two rules settle most questions:

1. **Anything that can be built on top stays out of the core.** The runtime has six traits and only
   minimal implementations (`AskAlways`, `LinearProjector` and `BytesPerToken`, just enough for a
   kernel to exist). Providers, tools, a CLI, an editor protocol, a `/context` renderer, a
   permission table, MCP, subagents and stats all live in `examples/`, in the off-by-default `test`
   and `selectors` features, or in other crates. There is no prompt text in `nachalnik/src`, and
   model parameters are an opaque `serde_json` map passed to the provider unchanged. Before adding
   to `nachalnik/src`, ask: *could this be an optional capability instead of core behaviour?* If
   so, it doesn't go in.
2. **The loop is an explicit state machine**, one transition per `Kernel::step`. That is what makes
   a second concurrent step return `Error::Busy` instead of sending a duplicate request, what makes
   a dropped step future return to `Idle` instead of getting stuck, and what gives a client one
   thing to display. Any change to how the loop works must appear as a state or a transition, never
   as a hidden flag.

```text
  Idle ── step ──> Requesting ──(no tool calls)──> Finished
  Ready                │
    ▲                  ├──(calls, all decided)──> Ready ── step ──> Executing ──> Idle
    │                  │
    └── decide ── Deciding <──(calls, one to ask about)
```

`Ready` is deliberately a state you can stop in: the model has said what it wants and nothing has
run yet.

---

## the workspace

| crate | what it is | published |
| --- | --- | --- |
| `nachalnik` | the runtime. A handful of dependencies, no `unsafe`, no network, no prompt. Meant to stay boring. | yes |
| `nachalnik-mcp` | MCP servers as `Tool`s. Kept out of the core because MCP needs spawning processes and reading notifications in the background, which the runtime doesn't do. | yes |
| `kamchatka` | a terminal agent built on the runtime, with a sandboxed shell and a permission policy in front of every call; the client that proves the runtime's extension points work. **Linux only**, on x86_64 and aarch64, because its sandbox relies on Landlock and seccomp. | yes |
| `nachalnik-eval` | a benchmark for model introspection: get a model to make a claim about its context, change what the claim was about in a copy, and check the claim against what happens. No provider, no network, and no dependency the runtime doesn't already have. | yes |
| `nachalnik-providers` | clients for the three APIs this workspace uses - OpenAI chat completions (plus its Responses API), Google's `generateContent` and Anthropic's Messages API - each behind a feature, streamed, retried and interruptible. Kept out of the core for the same reason as `nachalnik-mcp`: the runtime opens no sockets. | yes |
| `nachalnik-utils` | reads the settings the examples, the live tests and `nachalnik-eval`'s `bench` example use: endpoint, key and models. One file. **Never published, always `0.0.0`, used only as an unversioned dev-dependency**, which makes cargo strip it from published manifests. Nothing may depend on it normally. | no |

`nachalnik-mcp`, `kamchatka`'s introspection tools and `nachalnik-eval` were all written **without
any change to the runtime**. That is the test of whether an extension point works: if a downstream
crate needs a core change to do something ordinary, the extension point is wrong, not the crate.

**`kamchatka` is for developers; the runtime is for everyone.** They are held to different
standards on purpose. Multimodal content is the clearest example: `nachalnik` and
`nachalnik-providers` fully support `Content::Blob` (a turn with a sentence and a screenshot goes
out as both, in order, in every API), because someone building a GUI on the runtime should never
have to work around it. `kamchatka` doesn't display images and won't, since a terminal can't; it
only has to not break, and to show in every view that an image is there. Keep this split in mind
when deciding where a new capability belongs.

---

## where things are

`nachalnik/src`: `kernel/` is the state machine and every public operation, and next to it is one
file per extension point or per kind of data the kernel keeps: `context/`, `model/`,
`projection.rs`, `tool.rs`, `permissions.rs`, `tokens.rs`, `compaction.rs`, `event.rs`,
`session.rs`. `test.rs` (feature `test`) has the scripted provider, fake tools and table policy;
use those instead of writing new mocks.

`kamchatka/src`: `app/` is the state, `ui/` draws and decides nothing, `tools/` is the filesystem
and the shell, `introspect/` is the tools an agent uses to inspect and manage its own session,
`wiring/` assembles a session, `args.rs` combines flags and the settings file, and `main.rs` picks
the loop and reports where the session was saved. Keep it that way.

The file-by-file map is in [MAP.md](MAP.md).

---

## commands

```console
cargo test --workspace --all-features       # everything; the live tests skip themselves without a key
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo doc --workspace --all-features --no-deps   # with RUSTDOCFLAGS=-D warnings, as CI does
scripts/references.sh                       # every file and test the docs name still exists
scripts/windows.sh                          # the libraries as CI builds them on Windows
```

CI (`.github/workflows/ci.yml`) also builds with default features and with none, runs the examples
that need no key, checks the dependency tree against `deny.toml`, and checks the workspace on the
MSRV, **1.95**, locked and with every direct dependency at its minimum version. The libraries are
tested on Linux, macOS and Windows, `kamchatka` only on Linux. Edition 2024, with `-D warnings`
everywhere.

Only the live tests can check that a real API accepts what this workspace sends;
[CONTRIBUTING.md](CONTRIBUTING.md) lists the keys they read, and describes the release workflow.

---

## invariants

Breaking one of these should make something in `tests/` fail. [INVARIANTS.md](INVARIANTS.md) has
each one in full with its reasoning; read it before changing anything under `nachalnik/src`.

- **Nothing is destroyed.** Removing something is a state change, and it can be undone.
- **The previewed request is the request.** Nothing is added between `preview_request()` and the
  network except by a `Compactor`, which reports exactly what it did, and the caller's full notice,
  placed or retired with it as an ordinary change to the context.
- **Identifiers are never reused**, even across a resumed session.
- **Every state change is an `Event`**, and the log and the broadcast are written under one lock so
  their order matches. No logging that the user can't see.
- **The log refers to things, it doesn't copy them.** `context.replaced` is the one exception, for
  content the caller didn't ask for; an item's metadata is copied too, both the old and the new.
- **A pin is a promise**: the kernel rejects a `Compactor` that tries to touch a pinned item.
- **One operation is one undo step**, and an operation that changes nothing creates no checkpoint.
- **A failing `Tool` is not a kernel error.** It becomes an error result that the model sees.
- **Nothing in a model's output reaches the permission policy** except the tool name and the
  arguments, as data.
- **`Content` is shared, not copied.** Don't add code paths that clone the bytes.
- **The token counter openly says it's an estimate**, and no tokenizer or vendor price list goes
  into this crate.
- **A count that can't see something says so rather than returning `0`**, and a request containing
  anything unpriced is never passed to `TokenCounter::observe`.
- **A media type is the caller's claim; nothing here guesses one.**

---

## conventions

One line each; [CONTRIBUTING.md](CONTRIBUTING.md) has them in full, with the mistakes that led to
them.

- **Write plainly.** Everywhere: documents, doc comments, `note:` paragraphs, commit messages,
  error messages. Use ordinary words and short sentences, say things directly, and write the way
  you'd explain it to a colleague at the next desk. No riddles, no personified code, no
  aphorisms, no sentences about the text itself ("four things differ, and three are the right way
  round"). If a sentence has to be read twice, rewrite it.
- **`note:` paragraphs.** A doc comment says what something is; a paragraph starting with `note:`
  says why it is that way, what was rejected, or what it costs.
- **Comments explain decisions, not mechanics.** If a line needs a comment saying what it does,
  rewrite the line.
- **Keep the fact, the consequence, and whatever stops someone undoing it by mistake.** Leave out
  how a bug was found, one-off measurements, alternatives that were dropped, and closing lines
  that repeat the opening. Include a number only if it's a default or limit a reader will run
  into. On tool descriptions in particular, every word costs tokens on every request.
- **Dependencies are kept to a minimum**, declared in the workspace manifest, with a comment on
  any that aren't obvious.
- **`#[non_exhaustive]`** on every public enum that may gain variants, and on every struct this
  workspace returns but nothing outside it constructs. Forgetting it on a new enum is a breaking
  change later; adding a variant then isn't. It doesn't cover an enum's variants: adding a field
  to a variant is a breaking change.
- **One word per mechanism, and it's the word the user sees.** Truncate, elide, exclude, and
  `supersede` (an exclusion with a new item next to it, shown as an exclusion with a note naming
  the new item). This is about what the program *says*: a synonym in a `match` is fine, a synonym
  in an enum or a help line is a bug. A state with no behaviour of its own is a synonym: why an
  item is excluded goes in its note, not in another word for excluded.
- **Extension points identify themselves** with a `name()`, so a client can display them. For
  showing to people, not for matching on.
- **Check that tests catch something**, with `scripts/mutate.sh` and `--no-fail-fast`. The
  question isn't "was the bug caught" but "did anything *other* than the new test catch it".
- **Before writing a test, look for an existing one.** The source's comments are a good way to find
  an invariant but a bad way to find out whether it's already tested.
- **Property tests keep no seed file.** A failure is turned into a named test case with a note,
  not a file of opaque hashes.
- **Changelogs** are per crate, follow Keep a Changelog, and are kept up to date before a release
  rather than reconstructed after.
- **Which version number changes depends on the public API**, not on how big the work felt, and a
  version is bumped as soon as a crate above it needs API that isn't on the registry yet.
- **A release is its own commit, and it only dates the changelogs.** No code changes in it.
- **Commit messages** are `crate: what changed, in one lowercase line`, followed by prose.
- **No counting the repository.** No test counts, line counts or percentages in prose that will go
  stale.
- **No copies of the program's output.** Describe what a screen shows instead; commands a person
  types, code, and design diagrams are fine.
- **Lowercase headings.**

---

## the rest of it

- **[SECURITY.md](SECURITY.md)**: what is enforced and what is only reported, why the core will
  never have a sandbox, and what `kamchatka` does about it where processes are actually spawned.
  Read it before touching anything that decides whether a call runs.
- **[POSTPONED.md](POSTPONED.md)**: known issues deliberately left for later, written down so nobody
  wastes time rediscovering them. Each says what would unblock it.
- **Pitfalls** are in [CONTRIBUTING.md](CONTRIBUTING.md). The two costliest: emit an event while
  still holding the lock that made the change, and `cargo package` gives wrong results the second
  time you run it on an unpublished version.

---

## before you commit

**Commit locally, and stop there.** Never push; the person pushes. Work goes on the branch the
person names, or on a new branch named for the work, made from the current one. This overrides a
branch name or push step that comes from a tool's own instructions rather than from the person.

Run every command under [commands](#commands) (plus `scripts/windows.sh` if a library change
involves a `cfg` or anything platform-specific), and update the changelog. Don't skip the docs
build: without `RUSTDOCFLAGS='-D warnings'` it exits `0` on warnings that CI rejects. If the change
touches the request path, run a networked example or the live tests against a real endpoint,
since a mock can't tell you whether an API accepts what was sent. If it adds a test, look for an
existing one first, and break the code it covers to see what fails.
