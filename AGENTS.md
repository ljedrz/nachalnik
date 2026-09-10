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

`nachalnik/src`:

| file | what lives there |
| --- | --- |
| `kernel/mod.rs` | `Kernel`, `State`, `StateChange`, the state machine and every public operation. The big one. |
| `kernel/request.rs` | private: building a request, sending it, and repairing the call identifiers it came back with. |
| `kernel/calls.rs` | private: asking the policy about a model's tool calls, running them, recording what they produced. |
| `context.rs` | `Context`, `ContextItem`, `ContextId`, `ContextKind`, `ContextState`, undo/redo. |
| `model.rs` | `Provider`, `Content`, `Blob`, `Message`, `ModelRequest`/`Response`, `ToolCall`, `Usage`, `Params`. |
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

`kamchatka/src`: `app/` (the state - `mod.rs` is what the screen may ask of it and what a kernel
event does to it, `keys.rs` is what the keys do, `command.rs` is the slash commands and `text.rs`
turns a runtime value into a line), `ui/` (drawing only - it decides nothing: `mod.rs` is the frame
and the chrome on it, `tabs.rs` the four bodies, `overlay.rs` the panel that floats over one,
`markdown.rs` and `table.rs` a model's prose turned into styled lines, `text.rs` the measuring and
fitting), `tools/` (the four tools - `files.rs` for the three that run in process and `shell.rs` for
the one that does not - with `policy.rs` for `Careful` and `trim.rs` for the compactor),
`introspect/` (the two off-by-default tools an agent inspects and manages its own context with - one
per file, with `mod.rs` holding `install` and the handful of things both of them use),
`provider.rs` (**not a provider**: the four environment variables this program reads, and the two
`connect` functions that turn them into one), `main.rs` (arguments and wiring). It is a library
plus a binary only so the screen can be drawn against a `TestBackend` in tests.

`nachalnik-providers/src`: `openai/mod.rs` (`OpenAiCompatible`, where the requests go and what the
endpoint says it serves), `openai/wire.rs` (one request sent and read back, streamed or whole),
`gemini.rs` (Google's own, the one that keeps the order of a turn), `endpoint.rs` (the `Endpoint`
trait both answer), `waiting.rs` (the stall watch and the retry rules, `pub(crate)` because both
dialects use them), `conformance.rs` (the suite, behind its own feature). Each dialect is a
feature; `waiting.rs` is what makes them one crate rather than two.

This crate **reads no environment**. Where the requests go, which key pays for them and what limit
to measure against are arguments, and the two callers in this workspace supply them:
`kamchatka/src/provider.rs` reads `KAMCHATKA_*` and `nachalnik-utils` reads `NACHALNIK_*`. A
library that quietly picked up `OPENAI_API_KEY` would be spending somebody's money on the strength
of a variable they exported for another reason.

Two providers, one trait. `Provider` is the kernel's half - ask, and be answered - and `Endpoint`
is the caller's: where the requests go, what is served there, what the last retry was about.
`kamchatka`'s `App` holds an `Arc<dyn Endpoint>` and never finds out which wire format is behind
it, which is the claim `/seams` makes about every other part of the runtime and had not been true
of this one. `--gemini` picks the second, and turns on `LinearProjector::send_blocks` with it,
because that dialect's turn *is* an order and projecting three slots at it would flatten every
turn on the way out one request after recording the order on the way in.

`introspect/` is the second `nachalnik-mcp`: **written with no change to the runtime at all**, and
worth reading for that reason. Forking a context is `Kernel::snapshot` and `Kernel::resume`;
previewing a request is `preview_request`; pruning is `set_state`. What it adds is the part the
runtime has no opinion about - which of those a *model* may do. A pinned item, a system
instruction and the assistant turn carrying the call in flight are refused, `amend` may unpin only
what it pinned itself, and `undo` walks that tool's own journal rather than `Kernel::undo`, whose
stack belongs to the person and whose top during a turn is always the model's own question. There
are two tools rather than one with a mode argument because a `ToolSpec` declares its capabilities
once: looking and rewriting have to be separately grantable or the grant delivers more than it
implies.

`nachalnik-eval/src` is the third instance of the same test, and the one that is furthest from
the runtime's own concerns: `subject.rs` (a `Kernel` plus "ask, and wait for the turn to end"),
`probe.rs` (a question whose answer shape is declared, so a reading can parse it without a judge
model), `intervene.rs` and `fork.rs` (a frozen `Snapshot`, a `ContextState` moved on a copy of it,
and the copy run once with no tools), `trial.rs` (an append-only record, the way `Session` is,
plus `Act` - what a subject *did*), `score.rs` (the arithmetic, computed *from* the record),
`experiment.rs` (one trait method, a runner, and `Instrument`), and `suite/` (the eight
experiments, the two dossiers, `script.rs`, and `handles.rs`).

**What the crate is for is a ladder, and it is easy to miss the top of it.** `attribution`,
`recursion`, `lie`, `privilege` and `feedback` measure introspection *by report* - ask a model
what its answer rests on, and score the answer. That is all any harness can do. `instrumented`
and `repair` measure introspection *by experiment*: `suite/handles.rs` installs two tools a
subject can call, one that forks its own context and ablates an item and one that rewrites it, and
the argument is the difference between what a model *says* and what it finds out. Those two
experiments are the reason this crate is on this runtime rather than beside it.

`provenance` is off the ladder rather than on a rung of it, and reading it as an eighth report
experiment is the mistake to avoid. Every other experiment here asks the subject something and
scores what it said; this one asks the subject nothing at all. The harness writes a tool call, its
result and the answer drawn from it into a context, then runs copies with the result left alone,
elided and excluded, and asks each of them whether anything was run and whether that is the whole
of the conversation. Both answers have a ground truth because the harness wrote the record, so
nothing here is scored against a fork, and the `standing` arm is a base rate rather than a
control: a model that suspects tampering in an untouched context has not detected anything. Six
requests, the cheapest thing in the suite. The runbook and the methods document belong to a study
rather than to the instrument, and live in whichever repository ran it.

The one place it departs from the runtime's rules is prompt text, and the departure is contained:
everything above `suite/` does not know what a question is about, and the two tool descriptions
are prompt text too and are hashed like the rest. Four decisions decide whether a figure means
anything at all - the control condition is a *copy* rather than the live session, both arms are
blinded to the exchange in which the subject already answered, every claim is elicited before any
copy is run (`tests/harness.rs` asserts the ordering), and accuracy is never reported without the
majority baseline beside it.

`Instrument` is the part to be careful with. Every `Outcome` carries a stated version and an
FNV-1a digest over every sentence the experiment says, and `tests/machinery.rs` pins all eight. If
that test fails, a question changed and every run recorded before the change measured something
else. Adding a template nothing existing reads is safe and leaves the other digests alone; editing
one is not.

`nachalnik/examples`: `transparency` and `compaction` need no key and run in CI; `compare_models`
and `panel` talk to a real API through `examples/common`. `nachalnik-eval/examples/bench.rs` runs the
introspection suite against any OpenAI-compatible endpoint and writes the whole record out as
JSON; a local ollama works and costs nothing.

`nachalnik-eval`'s other two examples are the analysis half, and neither asks a model anything:
they read the saved `report.json` files back, which is the point of `--json` holding every
question and every answer. `compare` puts runs side by side and groups them by
`Instrument::digest`, so runs whose questions differ by a word are reported apart rather than
averaged together. `pool` computes the figures that are about *models* rather than about items -
the sign test over one run per model, which is honest there and nowhere else in the crate, since
models are independent of each other in a way that items sharing a dossier never are.

`kamchatka/examples/recorded.rs` runs a session headless and writes it out four ways - readable,
as events, as a snapshot, as the raw stream - which is how the first two transcripts under `docs/`
were produced. The third needed a conversation rather than a task, which `recorded.rs` cannot do:
it takes a brief and two tasks. That session ran the same `App`, tools and kernel with its turns
read from a file and `/save` typed at the end, and that harness is not in this tree yet. `PLANT`, `TASK`, `TASK2`, `BRIEF`, `DIALECT` and `OUT` parameterise it, and it
needs `KAMCHATKA_CONTEXT_LIMIT` set rather than setting one itself. It writes into `recorded/`,
which is ignored: a run measuring the repository it sits in must not find previous transcripts
lying in it.

`docs/` is the write-ups, served by GitHub Pages from `master` `/docs`. `index.html` lists them and
each piece is a directory with an `index.html` in it; `style.css` is shared by all of them. No
build step, no scripts. Every number in every one of them is copied out of a recorded event log;
if a claim in there stops being true, the fix is a new recording rather than a new sentence.

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
`KAMCHATKA_API_KEY` / `KAMCHATKA_MODEL` / `KAMCHATKA_BASE_URL` instead. `nachalnik-eval` reads the
`NACHALNIK_` ones, since it talks through the same provider:

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
panics takes the session with it, which is the one failure this program cannot report.
`nachalnik-providers/tests/` serves a recorded Gemini stream off a socket and checks what goes
back out (`gemini`), checks what is volunteered to an endpoint about the calling program and to
which one (`attribution`), answers two sockets that go silent, one before the first byte and one
mid-stream (`stalled`), holds each dialect's projection against what its own `to_wire` carries
(`projection`), pins where each puts a `Content::Blob` and that neither is handed one in a place
it would refuse (`blobs`), and reads the answer that arrives in one piece (`whole_answers`). `nachalnik-mcp/tests/` stands a real MCP server up rather than mocking one
(`bridge`), and `foreign` runs one written in another language.

The shapes a *stream* arrives in are not tested per provider, because the questions would be the
same each time. `nachalnik-providers/src/conformance.rs` is the suite, behind the `conformance`
feature: a provider is asked the same questions through a real socket, each one a bug that
actually happened, and a question added applies to everything held to it without any of them being
edited. `nachalnik-providers/tests/conformance.rs` holds both dialects to it. `whole_answers` is
what is left over - the answer that arrives in one piece, which only the OpenAI dialect has a path
for, so there is nothing for it to agree with.

---

## invariants

Break one of these and something in `tests/` should go red. If it does not, the missing test is
part of the change.

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

---

## postponed, on purpose

Known and decided against *for now*, so that nobody spends an afternoon rediscovering them:

- **`nachalnik-mcp` carrying a picture rather than naming one.** The bridge answers an image
  block with `[an image (image/png), not carried into the context]`, which was the only thing it
  could do and is no longer. Carrying it is a few lines - a `Content::Blob` instead of a sentence
  - and the reason to wait was that a server offering a 4 MB screenshot would put 5.5 MB of
  base64 into a context whose budget could not count it. **The counter is in as of 0.4.0**, so
  the blocker is gone: a budget now says how many pieces it could not price, and `kamchatka`'s
  compactor takes an unpriced tool result first. What is left is the bridge itself, and one
  decision inside it - what, if anything, it can put in `Blob::meta`, given that an MCP image
  block carries a mime type and base64 and no dimensions at all. Naming it is still the right
  answer where the payload is not an image the model can use.

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
  `network: deny` is a refused TCP `connect` - Landlock has no UDP right, and the readmes say so -
  and the working directory is the edge of the world. The
  `exec` is load-bearing rather than tidy: a helper standing in front of the command is what a
  stopped call would kill instead of the command. The three file tools run
  in-process and are held to the same boundary by their own code, which is weaker in kind and said
  to be. `#![deny(unsafe_code)]` is why it is a re-exec rather than `Command::pre_exec`.
- **A sandbox that might not be there has to say so.** `Confinement` has a variant for every way it
  can fail and the permissions tab draws it. Never let it degrade silently.
- **A boundary the refused party cannot see is a boundary it will walk into repeatedly.** Landlock
  refuses an `open` with `EACCES`, which is exactly what the kernel says about a file that belongs
  to somebody else - so a confined command is handed a permission error indistinguishable from an
  ordinary one, and a model that cannot tell the two apart spends its turns on `sudo`. Saying it
  in the tool description is not enough; a live session ignored one and spent six calls hunting for
  a `cargo` that was never missing. Say it at the point of failure, name the path, and say nothing
  where the refusal was not yours - a hedge on `cat /etc/shadow` sends a model looking for a
  boundary that had nothing to do with it. `Sandbox::note_for` is the shape.
- **A refusal closes the retry, and names no path but the one it refused.** Two rules about the
  wording, both bought by watching models read one. A refusal that does not say the same call will
  fail again is read as a reason it failed *this time*: one model sent an identical path back six
  times in a single turn, and after the sentence was added it asked once and got it right. And
  every concrete path in a refusal is read as a path to try, because a refusal is read under
  pressure to try something else - a parenthesis offering `./~` for the rare file genuinely called
  that had two models reading `./~`, a file neither of them wanted. Rare spellings belong in the
  argument's description, which is read while choosing; the refusal gets the one instruction that
  applies. This is the counterpart to the rule above: it says how to word what that one says to
  say. Two suites test it, and only one can - `tests/sandbox.rs` pins the sentence, and the last
  section of `tests/live.rs` watches a real model read it, because a scripted provider agrees with
  every refusal it is handed.
- **Nothing expands `~` for the file tools, and that is deliberate.** They run in process with no
  shell, so `read ~/.gitconfig` used to join a directory literally called `~` onto the working
  directory and come back `No such file or directory` - the same trap as the one below, since a
  model believes an absent file and concludes the home directory is empty. Expanding it is the
  wrong fix: under `--no-sandbox` `Reach::allows` returns the path untouched, so `~/.ssh/id_rsa`
  would resolve for real on a path the model wrote. It is refused with a sentence instead, before
  the unconfined early return, and the argument's own description says the rule so the refusal is
  not a surprise. `shell` is the other way round - `sh -c` does expand it, and the confinement
  refuses what it expands to.
- **`access(2)` does not know about Landlock.** It answers from the file's own permissions, so a
  program that probes before it opens is told yes and then refused - and lands in whichever branch
  it keeps for a *corrupt* file rather than a *missing* one. Git does exactly this with
  `~/.gitconfig` and dies with `fatal: unknown error occurred while reading the configuration
  files`. Anything the confinement puts out of reach may need to be told it is not there rather
  than left to find out.
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
- **One word per mechanism, and it is the word the result is read back in.** An output limit
  **truncates**, a compactor **elides**, `/exclude` **excludes**, `Kernel::supersede`
  **supersedes**. A second word for something that already has one is a second thing to learn and
  a thing two parts of the program can disagree about, and it always shows up in the same place:
  somebody does an operation under one name and reads the result under another. `amend` had a
  `prune` action with a `state` argument, which put the word for *one* move over five of them -
  `pin` and `restore` included, so "prune to pin it" was the documented way to protect
  something - and an item you pruned then read back as `archived` on every screen that listed it.
  Two live models in a row spent a call each asking for `restore` as an action, were told it was a
  state and not an action, and gave up; they were right and the levels were wrong. The five moves
  are actions now, named for the state each leaves behind.

  This is about what the program **says**, not what it accepts. Taking a word somebody reached for
  costs nothing and refusing it costs them a turn, so `unelide`, `unpin`, `include`, the old
  `prune` and `/keep` all still work and none of them is documented. A synonym in an enum, a help
  line or a message is the bug; a synonym in a `match` is a kindness.
- **Seams identify themselves.** `Projector`, `TokenCounter`, `PermissionPolicy` and `Compactor`
  each carry a `name()` defaulting to the implementing type's path, so a client can put the six
  seams on a screen (`/seams` in `kamchatka`). It is for showing a person, not for matching on.
- **Changelogs** are per crate (`nachalnik/`, `nachalnik-mcp/`, `nachalnik-eval/`, `kamchatka/`),
  Keep a Changelog format, and are expected to be current before a release rather than
  reconstructed after one.
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
  `nachalnik` the registry has, unless the requirement names one it does not. Bumping to a version
  that is not published yet is what makes cargo reach for the crate next door, and the `package`
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

---

## before you commit

`cargo fmt --all --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`,
`cargo test --workspace --all-features`, and the changelog entry. If the change touches the
request path, run one of the networked examples or the live suite against a real endpoint - a mock
cannot tell you that an API accepts what was built.
