# AGENTS.md

Orientation for whoever - person or model - is about to change this workspace. The `README.md`
files say what the crates are *for*; this says how they are built, what must not be broken, and
which way the arguments have already gone.

Five files carry the long form, so that this one stays readable end to end:
[INVARIANTS.md](INVARIANTS.md) for what must not be broken and why each one is there,
[MAP.md](MAP.md) for where everything lives, [CONTRIBUTING.md](CONTRIBUTING.md) for the commands,
the house conventions and the things that have cost somebody an afternoon,
[SECURITY.md](SECURITY.md) for what is and is not enforced, and [POSTPONED.md](POSTPONED.md) for
what is deliberately not built and what would unblock it.

---

## the thing being built

`nachalnik` is an agent runtime in which the context, the tools, the permissions and the requests
are explicit state a caller reads, changes and puts back. It is a library with no UI, no model,
no tools, no prompt, no filesystem and no network. It owns the loop, the context, and the paper
trail; everything else is somebody else's code behind a trait.

> The agent is not the boss. You are.

Two rules decide most questions before they are asked:

1. **Anything implementable on top stays out of the core.** The runtime ships six traits and no
   implementations worth the name (`AskAlways`, `LinearProjector` and `BytesPerToken` are the
   minimum that lets a kernel exist). Providers, tools, a CLI, an editor protocol, a `/context`
   renderer, a permission table, MCP, subagents, stats: all of them live in `examples/`, in the
   off-by-default `test` and `selectors` features, or in another crate. There is not one line of
   prompt text in `nachalnik/src`, and model parameters are an opaque `serde_json` map carried to
   the provider verbatim. Before adding to `nachalnik/src`, answer: *can this be an optional
   capability instead of core behaviour?* If yes, it is not going in.
2. **The loop is an explicit state machine**, one transition per `Kernel::step`. This is
   load-bearing rather than decorative: it is what makes a second concurrent step `Error::Busy`
   instead of a duplicated request, what makes a dropped step future return to `Idle` instead of
   wedging, and what gives a client one thing to render. Anything that changes the shape of the
   loop shows up as a state or a transition, never as a hidden flag.

```text
  Idle ── step ──> Requesting ──(no tool calls)──> Finished
  Ready                │
    ▲                  ├──(calls, all decided)──> Ready ── step ──> Executing ──> Idle
    │                  │
    └── decide ── Deciding <──(calls, one to ask about)
```

`Ready` is a resting state on purpose: the model has said what it wants and nothing has run.

---

## the workspace

| crate | what it is | published |
| --- | --- | --- |
| `nachalnik` | the runtime. Five dependencies, no `unsafe`, no network, no prompt. Meant to stay boring. | yes |
| `nachalnik-mcp` | MCP servers as `Tool`s. Deliberately outside the core: speaking MCP means spawning processes and reading notifications in the background, which the runtime promises not to do. | yes |
| `kamchatka` | a terminal agent built on the runtime; the proof that the seams hold under a real client. | yes |
| `nachalnik-eval` | a benchmark for model introspection: elicit a claim about a context, move the thing it was about on a copy, and score the claim against what happened. No provider, no network, four dependencies. | yes |
| `nachalnik-providers` | the two dialects this workspace talks - OpenAI chat-completions and Google's `generateContent` - feature-gated, streamed, retried and interruptible. Deliberately outside the core for the same reason as `nachalnik-mcp`: the runtime opens no sockets. | yes |
| `nachalnik-utils` | the *environment* the examples, the live suites and `nachalnik-eval`'s `bench` example read - which endpoint, which key, which models. Ninety lines; it held the provider until `nachalnik-providers` could. **Never published, permanently `0.0.0`, dev-dependency only, and depended on without a version** - which is what makes cargo strip it from a published manifest. Nothing may depend on it normally. | no |

`nachalnik-mcp` was written with **no change to the runtime at all**, and so were
`kamchatka`'s introspection tools and `nachalnik-eval`. That remains the test of whether a seam is
real: if a downstream crate needs a core change to do an ordinary thing, the seam is wrong, not
the crate.

**`kamchatka` is for developers, and the runtime is for everybody.** The two are held to different
standards on purpose, and the clearest case is multimodal. `nachalnik` and `nachalnik-providers`
carry a `Content::Blob` **fully** - a turn that is a sentence and a screenshot goes out as both, in
order, in either dialect - because somebody building a GUI on this runtime should never have to
work around it. `kamchatka` renders no pictures and is not going to: a terminal cell is not a
pixel, and what it owes a blob is that it does not break and that every view says one is there.
Read a request for a capability with that split in mind before deciding where it belongs.

---

## where things are

`nachalnik/src`: `kernel/` is the state machine and every public operation (`mod.rs`, with
`request.rs` and `calls.rs` as private halves of it), and one file per seam beside it -
`context.rs`, `model.rs`, `projection.rs`, `tool.rs`, `permissions.rs`, `tokens.rs`,
`compaction.rs`, `event.rs`, `session.rs`. `test.rs` (feature `test`) holds the scripted provider,
the fake tools and the table policy: use those rather than writing another mock.

`kamchatka/src`: `app/` is the state, `ui/` draws and decides nothing, `tools/` is the four tools,
`introspect/` the off-by-default ones an agent reads and manages its own session with, `wiring.rs`
assembles a session in nine steps and `main.rs` is arguments and a loop - which is the shape to
keep it in.

The file-by-file map, and the reasoning behind the shapes that are not obvious from the names, is
in [MAP.md](MAP.md).

---

## commands

```console
cargo test --workspace --all-features       # everything; the live suites skip themselves with no key
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

CI (`.github/workflows/ci.yml`) also builds with **default** features, checks `nachalnik`,
`nachalnik-mcp` and `kamchatka` with `--no-default-features`, runs the two keyless examples, and
checks the whole workspace on the MSRV, **1.88**. Edition is 2024, `RUSTFLAGS: -D warnings`
throughout, so a warning is a failure.

The live suites are the only thing that can check that a real API accepts what this workspace
builds. Which keys and variables each reads, which endpoints are known to work, where they are
known to differ, and how to measure whether a test is worth keeping: [CONTRIBUTING.md](CONTRIBUTING.md).

---

## invariants

Break one of these and something in `tests/` should go red. The whole of each, with the reasoning
that put it there, is in [INVARIANTS.md](INVARIANTS.md) - read that before changing anything under
`nachalnik/src`.

- **Nothing is destroyed.** Removal is a state change, and it comes back.
- **The previewed request is the request.** Nothing is added between `preview_request()` and the
  wire but a `Compactor`, which reports exactly what it did.
- **Identifiers are never reused**, including across a resumed session.
- **Every state change is an `Event`**, and the log and the broadcast are written under one lock so
  their order agrees. No logging a user cannot see.
- **The log names things, it does not copy them.** `context.replaced` is the one exception.
- **A pin is a promise**: the kernel refuses a `Compactor` that reaches for a pinned item.
- **One operation is one undo**, and an operation that changes nothing takes no checkpoint.
- **A failing `Tool` is not a kernel error.** It becomes an error tool result the model is shown.
- **Nothing in a model's output reaches the policy** except the tool name and the arguments, as
  data.
- **`Content` is shared, not copied.** Do not introduce a path that clones the bytes.
- **The counter is honest about being an estimate**, and no tokenizer or vendor price list goes
  into this crate.
- **A count that cannot reach something says so rather than returning `0`**, and a request carrying
  anything unpriced never reaches `TokenCounter::observe`.
- **A media type is a claim, and nothing in here guesses one.**

---

## conventions

The rule in one line each; [CONTRIBUTING.md](CONTRIBUTING.md) has every one of them in full, with
the mistake it came from - which is the half that makes them stick.

- **`note:` paragraphs.** A doc comment states what something is; a paragraph beginning `note:`
  states why it is that way, what was rejected, or what it costs. Most of the value of the docs
  is in those. Match the style.
- **Comments explain the decision, not the mechanics.** If a line needs a comment saying what it
  does, the line is wrong.
- **Dependencies are rationed.** Five in `nachalnik`, declared in the workspace manifest, each
  non-obvious one carrying a comment saying why.
- **`#[non_exhaustive]`** on every public enum the world can add to. Forgetting it on a new enum
  is the breaking change; adding a variant is not.
- **One word per mechanism, and it is the word the result is read back in.** Truncate, elide,
  exclude, supersede. This is about what the program *says*: a synonym in a `match` is a kindness,
  a synonym in an enum or a help line is the bug.
- **Seams identify themselves.** Four of them carry a `name()`, so a client can put the six on a
  screen. For showing a person, not for matching on.
- **A test's worth is measured, not assumed**, with `scripts/mutate.sh` and `--no-fail-fast`. The
  question is never "was it caught" but "did anything *other* than the new test catch it".
- **Before writing a test, look for it.** The source's own prose is a good way to find an
  invariant and a bad way to find out whether it is already checked.
- **Property suites keep no seed file.** A failure comes back as a named case with a note, not as
  a file of opaque hashes.
- **Changelogs** are per crate, Keep a Changelog, current before a release rather than
  reconstructed after one.
- **Which number moves is a fact about the public API**, not about how the work felt - and a
  version moves as soon as something above it needs API the registry does not have.
- **A release is its own commit and it only dates the changelogs.** No code moves in it.
- **Commit messages** are `crate: what changed, in one lowercase line`, followed by prose.
- **No counting the repository.** No test counts, line counts or percentages in prose that will
  outlive them.
- **The prose argues.** Lowercase headings, sentences that are sentences, and a paragraph that
  earns its place rather than restating the signature above it.

---

## the rest of it

- **[SECURITY.md](SECURITY.md)** - what is enforced and what is only reported, why the core will
  never grow a sandbox, and what `kamchatka` does about it where the process is actually spawned.
  Read it before touching anything that decides whether a call runs.
- **[POSTPONED.md](POSTPONED.md)** - known, decided against *for now*, and written down so nobody
  spends an afternoon rediscovering them. Each entry says what would unblock it.
- **gotchas** - two of them, both expensive, both in [CONTRIBUTING.md](CONTRIBUTING.md): emit an
  event while still holding the lock that made the change, and `cargo package` lying to you the
  second time you run it on an unpublished version.

---

## before you commit

`cargo fmt --all --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
`cargo test --workspace --all-features`, and the changelog entry. If the change touches the
request path, run one of the networked examples or the live suite against a real endpoint - a mock
cannot tell you that an API accepts what was built.

If the change adds a test, two more: look for the test first, and break what it is about to see
what fails. Both are a sentence under conventions, spelled out in
[CONTRIBUTING.md](CONTRIBUTING.md), and both have caught something real.
