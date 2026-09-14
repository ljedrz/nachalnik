# AGENTS.md

Orientation for whoever - person or model - is about to change this workspace. The `README.md`
files say what the crates are *for*; this says how they are built, what must not be broken, and
which way the arguments have already gone.

Four files carry the long form, so that this one stays readable end to end:
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

Break one of these and something in `tests/` should go red. If it does not, the missing test is
part of the change - and **measure** that rather than assuming it: see *a test's worth is
measured* under conventions, because it is cheap to get wrong in both directions.

- **Nothing is destroyed.** Removal is a state change. An excluded, archived or superseded item
  keeps its identifier, is still listed and inspectable, and comes back with a `set_state`, an
  `undo` or a `redo`. This holds for the output limit too: the whole of a truncated tool result is
  archived beside the truncated copy the model is shown (`Config::keep_truncated_output`).
- **The previewed request is the request.** There is no step between `preview_request()` and the
  wire where the kernel adds anything of its own. The one thing that may still intervene is a
  `Compactor`, and it reports exactly what it did.
- **Identifiers are never reused**, including by items that `undo` took away, and including tool
  call identifiers across a resumed session (`Snapshot::used_calls`, `repair_call_ids`).
- **Every state change is an `Event`**, and the log and the broadcast are written under one lock
  so their order agrees - with each other, and with the order the changes were actually applied in.
  No logging a user cannot see.
- **The log names things, it does not copy them.** `model.requested` records context ids, not
  messages. `context.replaced` is the one event carrying content, because overwritten text is the
  one thing nothing else can recover.
- **A pin is a promise**: the kernel refuses a `Compactor`'s attempt to remove a pinned item and
  says so in `CompactionReport::refused`.
- **One operation is one undo.** `push_all`, `set_state` over eight ids, `supersede`, a recorded
  turn - one checkpoint each. An operation that changes nothing takes no checkpoint, and one that
  is about to fail takes none either.
- **A failing `Tool` is not a kernel error.** It becomes an error tool result the model is shown.
  `Error` is only for conditions that stop the loop.
- **Nothing in a model's output reaches the policy** except the tool name and the arguments, both
  as data. A model insisting it already has permission has no effect.
- **`Content` is shared, not copied.** Every variant is behind an `Arc`; pruning a four-megabyte
  tool result moves a pointer. Do not introduce a path that clones the bytes.
- **The counter is honest about being an estimate.** No tokenizer goes into this crate - that
  would be a model-specific assumption. `Calibrating` corrects from what providers charge, from
  then on, and never silently rewrites figures already recorded (`Kernel::recount` does, loudly).
- **A count that cannot reach something says so rather than returning `0`.**
  `TokenCounter::uncounted` is how, and it rides up to `Budget::uncounted` and
  `ContextItem::uncounted`, so
  "measured, and free" and "there is a picture here and nothing priced it" are never the same
  figure. Two rules follow and both are load-bearing. A request carrying anything unpriced does
  not reach `TokenCounter::observe`: `Calibrating` corrects with a single multiplier, so a gap it
  cannot see gets spread over the bytes it can, and prose beside one screenshot ends up reading
  50% high while the screenshot still reads nothing. And whatever a counter *would* need in order
  to price a payload goes in `Blob::meta`, which the kernel never reads - on the blob rather than
  the item, because the budget is counted over projected messages and a `Message` carries a
  `Content` and nothing else a counter can see.

  This crate carries no vendor formula and is not going to. A dialect is a shape that changes
  over years and a price list is a per-model fact that changes whenever a vendor ships a model,
  so putting the formulas in `nachalnik-providers` would turn "we speak two dialects" into a
  subscription - and be wrong silently, which is what the abstention exists to end. The three
  formula *shapes* stay as prose on `BytesPerToken::count`. Two near misses, so nobody
  re-proposes them: a typed `dimensions` on `Blob` covers pictures and leaves a PDF's pages and a
  recording's seconds nowhere to go, and a `tokens: Option<usize>` on `Blob` is a per-model
  figure on a model-agnostic type, wrong the moment the model changes.

- **A media type is a claim, and nothing in here guesses one.** Three places act on it and all
  three would be wrong if it were inferred. `kamchatka`'s `attach::TYPES` maps ten extensions and
  refuses anything else that is not valid text, rather than sniffing the bytes - an uncompressed
  PDF is valid UTF-8 for pages at a time, so "is this text?" answers yes and the model is sent
  PDF source. The OpenAI dialect then reads the media type to pick between `image_url` and
  `file`, because in that dialect `image_url` means an image and a PDF sent through it is a 400;
  Google's `inline_data` needs no such split. And `Blob::meta["name"]` is what fills that `file`
  part's required filename - a convention between a caller and a provider, *not* a key the kernel
  knows, which is the whole point of `meta` being free-form. A derived `file.pdf` is the fallback
  because the part is refused without one.

  `nachalnik-providers` deliberately does not implement this dialect's third payload shape,
  `input_audio`. Nothing in the workspace produces a recording, so it would be a shape written
  from a specification and pinned by no test - which is exactly what the `file` part was until
  `a_document_goes_out_as_a_document` in `nachalnik/tests/live.rs` sent one at a real endpoint.
  That test is the reason to trust the shape; there is no offline equivalent.

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
what fails. Both are a sentence under conventions and both have caught something real.
