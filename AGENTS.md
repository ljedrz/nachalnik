# AGENTS.md

Orientation for whoever - person or model - is about to change this workspace. The `README.md`
files say what the crates are *for*; this says how they are built, what must not be broken, and
which way the arguments have already gone.

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
| `nachalnik-utils` | the OpenAI-compatible provider the examples and the live suite share. **Never published, permanently `0.0.0`, dev-dependency only** - cargo strips dev-dependencies from a published manifest, which is the whole trick. Nothing may depend on it normally. | no |

`nachalnik-mcp` was written with **no change to the runtime at all**. That remains the test of
whether a seam is real: if a downstream crate needs a core change to do an ordinary thing, the
seam is wrong, not the crate.

---

## where things are

`nachalnik/src`:

| file | what lives there |
| --- | --- |
| `kernel.rs` | `Kernel`, `State`, the state machine, every public operation. The big one. |
| `context.rs` | `Context`, `ContextItem`, `ContextId`, `ContextKind`, `ContextState`, undo/redo. |
| `model.rs` | `Provider`, `Content`, `Message`, `ModelRequest`/`Response`, `ToolCall`, `Usage`, `Params`. |
| `projection.rs` | `Projector`, `LinearProjector`, `Projection`, `Skipped` - context to wire messages. |
| `tool.rs` | `Tool`, `ToolSpec`, `ToolOutput`. |
| `permissions.rs` | `PermissionPolicy`, `Capability`, `Verdict`, `Grant`, `AskAlways`. |
| `tokens.rs` | `TokenCounter`, `BytesPerToken`, `Calibrating`. |
| `compaction.rs` | `Compactor`, `Budget`, `CompactionPlan`/`Report`. |
| `event.rs` | `Event` (the whole observability story), `Delta`, `DeltaSink`, `OutputSink`. |
| `session.rs` | `Session`, `Record`, `Snapshot`. |
| `config.rs`, `error.rs` | `Config` (with the reasoning for each default in the docs), `Error`. |
| `selectors.rs` | feature `selectors`: `17`, `tool:grep:latest`, `all:tool_results`, `file:src/foo.rs`. |
| `test.rs` | feature `test`: `ScriptedProvider`, `EchoTool`/`ConstTool`/`BrokenTool`, `AllowAll`/`DenyAll`/`Table`, `LargestFirstCompactor`. Use these rather than writing another mock. |

`kamchatka/src`: `app.rs` (state and key handling), `ui.rs` (drawing only - it decides nothing),
`tools.rs` (four tools, the `Careful` policy, the `Trim` compactor), `mind.rs` (the two
off-by-default tools that hand the agent its own context), `provider.rs`, `main.rs` (arguments and
wiring). It is a library plus a binary only so the screen can be drawn against a `TestBackend` in
tests.

`mind.rs` is the second `nachalnik-mcp`: **written with no change to the runtime at all**, and
worth reading for that reason. Forking a context is `Kernel::snapshot` and `Kernel::resume`;
previewing a request is `preview_request`; pruning is `set_state`. What it adds is the part the
runtime has no opinion about - which of those a *model* may do. A pinned item, a system
instruction and the assistant turn carrying the call in flight are refused, `amend` may unpin only
what it pinned itself, and `undo` walks that tool's own journal rather than `Kernel::undo`, whose
stack belongs to the person and whose top during a turn is always the model's own question. There
are two tools rather than one with a mode argument because a `ToolSpec` declares its capabilities
once: looking and rewriting have to be separately grantable or the grant delivers more than it
implies.

`nachalnik/examples`: `transparency` and `compaction` need no key and run in CI; `compare` and
`panel` talk to a real API through `examples/common`.

---

## commands

```console
cargo test --workspace --all-features       # everything; the live suite skips itself with no key
cargo test -p nachalnik                     # the runtime's offline suite
cargo fmt --all --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo doc --workspace --all-features --no-deps   # with RUSTDOCFLAGS=-D warnings, as CI does
```

CI (`.github/workflows/ci.yml`) also builds with **default** features (the tests turn both on, so
nothing else exercises that configuration), checks `nachalnik`, `nachalnik-mcp` and `kamchatka`
with `--no-default-features`, runs the two keyless examples, and checks the whole workspace on the
MSRV, **1.88**. Edition is 2024. `RUSTFLAGS: -D warnings` throughout, so a warning is a failure.

The live suite is the only thing that can check that a real API accepts what this crate builds:

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --test-threads=1 --nocapture
```

It reads `OPENROUTER_API_KEY` or `NACHALNIK_API_KEY` (never a stray `OPENAI_API_KEY`), with
`NACHALNIK_BASE_URL`, `NACHALNIK_TEST_MODEL` and `NACHALNIK_CONTEXT_LIMIT` to point it elsewhere -
Google AI Studio's OpenAI-compatible endpoint and a local ollama both work. It skips rather than
fails without a key, or when a free tier has spent its allowance. `kamchatka` reads
`KAMCHATKA_API_KEY` / `KAMCHATKA_MODEL` / `KAMCHATKA_BASE_URL` instead.

Test files: `nachalnik/tests/` is `kernel`, `context`, `state`, `session`, `tokens`,
`concurrency`, `live`. `kamchatka/tests/` draws the screen and reads the characters back
(`screen`), drives the metacognition tools through the real loop (`mind`), runs real commands
under a real ruleset (`sandbox`), and answers three sockets: one
that goes silent mid-stream (`stalled`) and two that serve the shapes a streamed tool call arrives
in (`streaming`). `edges` is the sweep: every tab at every window size from 1x1 up, every key at
every tab with nothing to act on, and both scrolled past their own ends - a frame that panics takes
the session with it, which is the one failure this program cannot report. `nachalnik-mcp/tests/`
stands a real MCP server up rather than mocking one.

---

## invariants

Break one of these and something in `tests/` should go red. If it does not, the missing test is
part of the change.

- **Nothing is destroyed.** Removal is a state change. An excluded, archived or superseded item
  keeps its identifier, is still listed and inspectable, and comes back with a `set_state`, an
  `undo` or a `redo`. This holds for the output limit too: the whole of a truncated tool result is
  archived beside the shortened copy the model is shown (`Config::keep_truncated_output`).
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
- **`Content` is shared, not copied.** Both variants are behind an `Arc`; pruning a four-megabyte
  tool result moves a pointer. Do not introduce a path that clones the bytes.
- **The counter is honest about being an estimate.** No tokenizer goes into this crate - that
  would be a model-specific assumption. `Calibrating` corrects from what providers charge, from
  then on, and never silently rewrites figures already recorded (`Kernel::recount` does, loudly).

---

## postponed, on purpose

Known and decided against *for now*, so that nobody spends an afternoon rediscovering them:

- **Content is `Text` or `Json`, and nothing else.** There is no image, audio or binary variant,
  so a multimodal provider has to carry bytes through `Content::Json`, and `BytesPerToken` then
  counts base64 length over four - a figure that is not so much wrong as meaningless, in a crate
  whose case rests on an honest budget. `Content` is `#[non_exhaustive]`, so a `Blob` variant is
  additive whenever it is wanted; what it would drag in is a `TokenCounter` that can say something
  sensible about pixels, which is the part that is not additive. **Not before multimodal support
  is actually being built** - a variant nothing produces and nothing counts would be worse than
  its absence.
- **`Message` carries one content slot, not a list of blocks**, which is the same limitation seen
  from the wire end: a dialect whose assistant turn is an ordered list of typed blocks can be
  approximated by a projector but not expressed by one. The constraint is stated where a projector
  author will meet it, in `projection.rs`; do not restate the broader claim that *any* dialect is
  reachable by swapping the projector alone.

---

## the security position

Stated once, because it is the thing most likely to be quietly assumed otherwise, and it is in the
README and the crate docs in longer form:

- **There is no sandbox, and the core will not grow one.** The kernel executes nothing - no
  filesystem, no network, no process spawning - so it has nothing to contain. Containment belongs
  inside a `Tool` or around the whole process.
- **What is enforced is one thing:** a refused call is never handed to `Tool::invoke`, and the
  refusal is an event and a tool result. A decision point with a paper trail, not a boundary.
- **A `Capability` is a declaration, not a verified property**, and `Shell` subsumes every other
  one. A client that shows `shell: allow` beside `network: deny` without saying so is reporting a
  restriction that does not exist - which is why `kamchatka`'s permissions tab says so.
- **Confinement lives where the process is spawned.** `kamchatka` puts its `shell` tool under
  Landlock by re-executing itself in a mode that restricts itself and then `exec`s the command, so
  `network: deny` is a refused `connect` and the working directory is the edge of the world. The
  `exec` is load-bearing rather than tidy: a helper standing in front of the command is what a
  stopped call would kill instead of the command. The three file tools run
  in-process and are held to the same boundary by their own code, which is weaker in kind and said
  to be. `#![deny(unsafe_code)]` is why it is a re-exec rather than `Command::pre_exec`.
- **A sandbox that might not be there has to say so.** `Confinement` has a variant for every way it
  can fail and the permissions tab draws it. Never let it degrade silently.
- **Do not add a check that implies more than it delivers.** `reaches_the_network` is allowed to
  exist because its documentation is exact about what it misses, and because refusing up front with
  a reason is kinder than letting a command run and fail. It is no longer what stands between the
  model and the network. Anything of that shape needs the same treatment.

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
  `ContextKind`, `ContextState`. A new variant is not a breaking change; forgetting the attribute
  on a new enum is.
- **Seams identify themselves.** `Projector`, `TokenCounter`, `PermissionPolicy` and `Compactor`
  each carry a `name()` defaulting to the implementing type's path, so a client can put the six
  seams on a screen (`/seams` in `kamchatka`). It is for showing a person, not for matching on.
- **Changelogs** are per crate (`nachalnik/`, `nachalnik-mcp/`, `kamchatka/`), Keep a Changelog
  format, and are expected to be current before a release rather than reconstructed after one.
- **Commit messages** are `crate: what changed, in one lowercase line`, followed by prose
  explaining what was wrong, what was decided, and what was checked - including what was
  deliberately *not* done and why. Read `git log` before writing one; the bar is high and
  consistent.
- **The prose argues.** Headings are lowercase, sentences are sentences, and a paragraph that
  merely lists what a thing has is not finished. Spelling leans British (`behaviour`, `defence`,
  `optimisation`, `honouring`) with `-ize` endings for `summarize`. Rust source uses hyphens; the
  `README.md`s use em dashes.

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

---

## before you commit

`cargo fmt --all --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
`cargo test --workspace --all-features`, and the changelog entry. If the change touches the
request path, run one of the networked examples or the live suite against a real endpoint - a mock
cannot tell you that an API accepts what was built.
