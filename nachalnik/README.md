# nachalnik

[![crates.io](https://img.shields.io/crates/v/nachalnik.svg)](https://crates.io/crates/nachalnik)
[![docs.rs](https://docs.rs/nachalnik/badge.svg)](https://docs.rs/nachalnik)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**An agent runtime in which the context, the tools, the permissions and the requests are explicit
state — state you can read, change and put back.** Not decisions taken inside a framework and
reported to you afterwards.

> The agent is not the boss. You are.

It is a library, not a program: it owns no UI, no editor, no model, no tools and no prompt. What
it owns is the loop, the context, and the paper trail. The rest of the
[workspace][workspace] - a terminal agent, an MCP bridge, an introspection benchmark - is what
gets built on top.

---

### ⏱️ in thirty seconds

**See the exact request before it goes out** - not a trace of it afterwards:

```rust
let request = kernel.preview_request()?;   // every message, tool definition and parameter
let payload = kernel.preview_payload()?;   // and the provider's own bytes, if it renders them
```

**Throw out what you do not want, and change your mind about it.** A tool just returned 600 lines
of passing tests:

```rust
let ids = Selector::parse("tool:cargo_test:latest")?.matches(&kernel.items());
kernel.set_state(ids, ContextState::Excluded, Some("13k tokens of nothing".into()));
assert_eq!(kernel.budget().used(), 126);   // it was 13,173
kernel.undo();                             // and it is back, with its note and its identifier
```

**Stop between transitions, not between functions.** `Ready` is the state in which the model has
said which tools it wants and none of them have run:

```rust
match kernel.step().await? {
    State::Ready { calls } => { /* look at them, prune, or cancel_pending_calls */ }
    State::Deciding { .. } => { /* ask a person, then kernel.decide(..) */ }
    _ => {}
}
```

---

### 🎯 what it is for, and what it is not

Reach for it when **what was in the context is part of your answer**:

* **Evaluation and model comparison.** The same items into several kernels, with a digest of the
  projected messages showing that the only variable was the model - and the tokenizers disagreeing
  with each other about identical bytes, which you can see rather than assume.
  (`cargo run --example compare`, `--example panel`)
* **Editor and IDE integration.** A `/context` view, a permission prompt and an undo that are
  yours to render, over a loop that stops between transitions instead of acting and reporting.
* **Anything that has to be auditable or reproducible.** An append-only log of typed events, plus
  a snapshot that resumes the same session in another process — and an assistant turn recorded in
  the order the model produced it, thinking and tool calls interleaved, rather than rearranged
  into whichever shape the wire format wanted.
* **Agents that read and manage their own context.** Everything here is public API a `Tool` can
  call, so the same view and the same controls can be handed to the model. `kamchatka` does; see
  the [write-ups][writeup] of sessions where an agent found a false note in its own context and
  rewrote it, and where one took back a hallucination of its own the same way.

Reach for something else if you want **an agent today**. This crate ships no provider, no tools,
no prompt and no UI, so a working agent is yours to assemble; [`kamchatka`][kamchatka] in this
workspace is what that costs, and most of it is tools and rendering. If you want batteries
included, `goose` and `codex` are good and also Rust. `nachalnik` is what you build a harness
*out of*.

---

### ⚡ why?

* **Nothing is hidden.** Every context item has an identity, a size, a source, a state and a
  reason for being there, and every transition the loop makes is an event. The request you
  previewed is the request that goes out.
* **Nothing is sacred.** That 17,000-token tool output can be excluded, and it goes. No
  "the agent has determined that this information is relevant".
* **Nothing is assumed.** No system prompt, no personality, no planning ritual, no mandatory
  subagents, no MCP, no default tools, no filesystem or network code, no permission table, no
  `/context` renderer, no background activity. There is not one line of prompt text in this
  crate.
* **Everything is an event.** The whole session is an append-only log of typed events, so a
  client is `subscribe()` + render, and changing the UI does not invalidate sessions.
* **Small.** Five dependencies (`async-trait`, `parking_lot`, `serde`, `serde_json`, `tokio`),
  no `unsafe`, and a codebase you can read in an afternoon.

---

### 🔁 the loop is a state machine

```text
  Idle ── step ──> Requesting ──(no tool calls)──> Finished
  Ready                │
    ▲                  ├──(calls, all decided)──> Ready ── step ──> Executing ──> Idle
    │                  │
    └── decide ── Deciding <──(calls, one to ask about)
```

`Kernel::step` performs exactly one of those transitions and returns the `State` it produced;
`Kernel::turn` repeats until the model ends its turn or somebody has to decide something.

* `Requesting` and `Executing` mean the loop is already being driven; a second `step` is
  `Error::Busy`, not a second request. If the future driving a transition is dropped, the kernel
  returns to `Idle` instead of wedging.
* Every other state is a resting state, and whatever you change while the kernel rests is what
  the next request will contain.
* `Ready` exists for exactly that reason: the model has said which tools it wants, nothing has
  run yet, and you can look first (`pending_calls`) - or refuse (`cancel_pending_calls`).

---

### 🚀 quick start

```rust
use std::sync::Arc;

use nachalnik::{Config, ContextItem, ContextState, Kernel, State};

let kernel = Kernel::new(Config::default());
kernel.set_provider(Arc::new(my_provider));  // you implement Provider
kernel.set_policy(Arc::new(my_policy));      // ... and decide what is allowed
kernel.add_tool(Arc::new(my_tool));          // ... and what can be done

// context is added on purpose, and every item can be named afterwards
let file = kernel.push(ContextItem::file("src/parser.rs", contents).pinned());
kernel.push(ContextItem::user("why is this failing?"));

// exactly what is about to be sent, before it is sent
let request = kernel.preview_request()?;
let payload = kernel.preview_payload()?;   // and the provider's own bytes, if it can show them

// the loop
match kernel.turn().await? {
    State::Finished { .. } => println!("{:?}", kernel.last_response()),
    State::Deciding { .. } => { /* ask a person, then kernel.decide(..) */ }
    State::Idle => { /* the turn's request budget ran out; your call */ }
    other => unreachable!("a turn does not end in {other:?}"),
}

// and the context remains yours
kernel.set_state([file], ContextState::Excluded, Some("too big".into()));
kernel.undo();
```

---

### 🧩 architecture

The kernel provides the loop and the bookkeeping; you provide the parts. Each of these is a
trait object you can set, swap at runtime, and inspect:

| trait | you provide | the kernel provides |
| --- | --- | --- |
| `Provider` | a model, however you reach it | the request, verbatim |
| `Tool` | what the model can do | the schema, the gating, the recording |
| `PermissionPolicy` | what is allowed | the question, and the refusal |
| `Projector` | the shape of a request | the context it is projected from |
| `TokenCounter` | how tokens are counted | every number it reports, and what each request really cost |
| `Compactor` | what to drop when it fills up | the veto on pinned items, and the report |

Each of them can also say what it is - `Provider` through `info()`, `Tool` through `spec()`, and
the other four through a `name()` whose default is the implementing type's own path. So
`kernel.policy().name()` is a thing a client can put on a screen, and "six replaceable parts" is
checkable rather than asserted:

```text
provider     gemini-3.5-flash via openai-compatible
tools        6 offered: edit, epoch__from_stamp, epoch__to_stamp, read, shell, write
policy       kamchatka::tools::Careful
projector    nachalnik::projection::LinearProjector
counter      nachalnik::tokens::Calibrating<nachalnik::tokens::BytesPerToken>
compactor    kamchatka::tools::Trim
```

Model parameters are an opaque `serde_json` map carried to the provider verbatim, so `thinking`,
`safety_settings` and `reasoning_effort` are exactly as first-class as `temperature` - and the
kernel cannot send anything you did not ask for.

The kernel has no wire format, so `preview_request` is as far as its own guarantee reaches. A
provider that implements `render` closes the rest of the gap: `preview_payload` then shows the
payload itself, and `Config::record_payloads` puts it in the log. Be precise about what that is
worth - it is the provider's account of itself, exactly like a tool's declared capabilities, and
the kernel has nothing to check it against. Render once and send what you rendered; a preview that
has quietly stopped matching is worse than none.

A reasoning model's own thinking is treated the same way. It is recorded on the turn that produced
it, counted like everything else, and offered back to the provider in `Message::reasoning` — some
APIs verify a signed thinking block against the turn it came from, and a runtime that dropped it
could not talk to them. It is never separated from its turn, and `LinearProjector::send_reasoning`
decides whether it goes back out. `ToolCall::extra` is the same idea per call: whatever a provider
attaches to one - Google's `thought_signature`, an encrypted reasoning item - is carried back
attached to that call, verbatim and uninterpreted. Gemini rejects the *next* request outright when
it goes missing, which is the sort of thing you only find out by asking a real API.

---

### 🔍 context as a data structure

Items are public data - an id, a kind, a source, a label, content, a size, a state, a note and
whatever metadata you attach - so a client can render `/context` however it likes. One method
covers every state change, and each call is one undoable operation:

```rust
kernel.set_state(ids, ContextState::Excluded, Some("an enormous test output".into()));
kernel.set_state(ids, ContextState::Pinned, None);
kernel.replace(id, "a shorter version")?;                 // new contents, same identifier
kernel.supersede(old, ContextItem::file(path, reread))?;  // this one replaces that one
kernel.annotate(id, json!({ "expendable": true }))?;      // a hint for your compactor
kernel.push_all(files);                                   // one operation, so one undo
kernel.undo();
kernel.redo();
```

`set_state` says what it did to each identifier - `changed`, `unchanged`, `unknown` - because
"there is no item 12" and "item 12 was already pruned" are different things to tell somebody.

Excluding an item removes it from the *projection*, not from the record. It keeps its identifier,
stays listed and inspectable, and comes back with another `set_state`, an `undo`, or a `redo`.
`Elided` is the third answer between in and out: the item stays in the request as a one-line
marker, so a tool result can stop costing what it holds without the call that asked for it having
to come down too. The default projector drops the other half of a call/result pair when one side
is gone, so pruning cannot produce a request the provider will reject - and says so in
`Projection::repairs` *and* in the session log, a request the kernel quietly adjusted being exactly
the one you want to be able to ask about afterwards.

**Nothing is destroyed, including by a limit.** A tool output over its limit is recorded twice:
the whole of it, archived, and the truncated copy the model is shown. Putting the whole thing back
in front of the model is a `set_state` like any other, rather than a re-run of the tool. Keeping
it costs a pointer rather than a copy: content, tool-call arguments and tool schemas are all
shared, so pruning a four-megabyte tool result moves a pointer, projecting it into a request moves
a pointer, and the event recording it points at the same bytes the context holds. Set
`Config::keep_truncated_output` to `false` when a tool can produce more than you are willing to go
on holding; the truncation is still reported either way.

One agent is one kernel, and a fleet of them shares nothing but whatever you hand to both, so
running sixteen at once needs no coordination at all. Within a single turn, the tools a model
asks for run one at a time in the order it asked - which is something you can build on, since two
edits to the same file then apply in sequence. `Config::parallel_tool_calls` gives that up for
speed, on purpose and never by default: nothing in the kernel can tell whether a model's calls are
independent, so the judgement is yours. Either way the results are recorded in the order the model
asked for them. Several threads on a *single* kernel are fine too - reading is cheap and every
mutation is atomic, so a client can render, prune and preview while a turn is in flight. The one
thing two threads cannot do is drive the loop at the same time, which is `Error::Busy` rather than
a second request.

Automatic management is allowed, invisible management is not. A `Compactor` gets the budget and
the items and returns a plan; the kernel refuses to remove anything pinned, applies the rest,
and broadcasts a report of exactly what it did - which the user can then disagree with.

---

### 🧵 a turn keeps the order it was produced in

Most runtimes record an assistant turn as three slots: a content string, a reasoning string, and a
flat list of tool calls. Real turns are not shaped like that. A reasoning model thinks, says a
sentence, asks for a tool, thinks again before the next one — and the order is *information*.
Flatten it on the way in and no projector can ever get it back.

So content can be an ordered sequence:

```rust
ContextItem::assistant(
    Content::blocks([
        Block::reasoning("the stack trace points at parse()"),
        Block::text("Checking the tests first."),
        Block::Call(read_tests),
        Block::text("and now the parser itself"),
        Block::Call(read_parser),
    ]),
    Vec::new(),
)
```

It is a variant of `Content` rather than a field on `Message` because content is the one type a
`ModelResponse`, a `ContextItem` and a `Message` all carry — so the order survives from the wire,
into the context where it is counted and pruned like anything else, and back out again.

A turn is recorded *either* that way *or* in the three conventional slots, never both, so nothing
can hold two accounts of it; `calls()` and `thinking()` read whichever is in use.
`LinearProjector::send_blocks` decides which shape goes out, and flattening reports what it cost
in `Projection::repairs` rather than doing it quietly.

---

### 🎯 a budget that corrects itself

Every token figure the kernel reports comes from a `TokenCounter`, and the estimate underneath the
default one - `bytes / 4` - is admittedly that. How wrong it is depends on the shape of what you
are sending: measured against a real API, about a third low on a short chat carrying four tool
definitions, and a steady 7% low once the conversation is a few thousand tokens. It cannot see
per-message framing and never sees the tokens a reasoning model spends thinking. Embedding a
tokenizer would mean embedding a model-specific assumption, which this crate will not do.

So it does the other thing. After every response, the provider has said what the request actually
cost, and the kernel knows what it estimated for the very same bytes - so it hands both numbers to
the counter, and a `Kernel::new` is already holding one that acts on them:

```rust
// what a kernel starts with; correcting by 1.0 until a provider has said otherwise
Calibrating::new(BytesPerToken::default())

// and the bare estimate, for a measurement that wants a counter which never changes its mind
kernel.set_counter(Arc::new(BytesPerToken::default()));
```

`Calibrating` converges on the first response worth learning from and settles there - measured over
a growing conversation, it took that steady 7% error to within 1%. What it learned is a number you
can look at (`calibration()`), not a fudge factor buried in the kernel. It ignores requests too
small to have a systematic error in them, because a percentage drawn from a handful of tokens is
noise. And it corrects what is counted *from then on*: figures already recorded on items do not
silently rewrite themselves, because that is exactly the sort of thing this crate does not do -
`Kernel::recount` rewrites them when you ask, and says so on the event stream.

The hook is `TokenCounter::observe`, whose default does nothing. As everywhere else, the kernel
supplies the facts and your code supplies the judgement.

---

### 🛑 stopping

`interrupt()` can be called from any thread, and stops the loop in three places, each needing a
little more cooperation than the last:

| where | what happens | who has to agree |
| --- | --- | --- |
| between transitions | the next `step` or `turn` spends one attempt acknowledging it and does nothing else | nobody |
| during a request | a provider that checks `DeltaSink::is_interrupted` stops reading and hands back what it has | the provider |
| during tool calls | the kernel does not start the serial calls that had not begun; a tool that checks `OutputSink::is_interrupted` can stop the one that had | the tool |

The kernel cannot reach into a `Provider` and stop it - it does not own the socket, the runtime or
the future - so it offers the fact and lets the provider decide. One that ignores it is not broken,
only slower to stop.

What stopping never does is discard work. A half-finished answer and a tool that returned early are
recorded as ordinary items, because the point of a context you can see is that *you* decide what to
do with them. The blunt instrument is still there - drop the future driving `step` and the request
is abandoned mid-flight, the kernel returns to `Idle` rather than wedging - but it costs you
whatever had been streamed.

---

### 🔓 what it does and does not protect you from

**The kernel executes nothing.** No filesystem code, no network code, no process spawning; every
side effect in a session happens inside a `Tool` you wrote and registered. So there is nothing here
to contain, and there will be no sandbox in this crate - containment belongs where the process is
actually spawned, which is your tool or the program around it. ([`kamchatka`][kamchatka] is the one
in this workspace that spawns things, so it is the one that confines them, with Landlock.)

What the runtime enforces is one thing: a call the `PermissionPolicy` refused is never handed to
`Tool::invoke`, and the refusal is recorded as an event and as a tool result the model is told
about. That is a decision point with a paper trail. The refusal says what *kind* it was, because
that is the only question a refused model can act on: a standing rule means the same call will meet
the same answer, and an answer to *this* call means a different approach may well be allowed. Which
of the two it was is the kernel's own knowledge - it resolved the grant. *Why* is not, so the
kernel asks: `PermissionPolicy::why` is defaulted to `None`, and whatever a policy returns goes
into the tool result beside the kernel's account of it.

Three things follow, and none of them is a bug:

* **A `Capability` is a declaration, not a verified property.** A tool that declares `Read` and
  opens a socket is lying, and the kernel has nothing to check it against. The defence is that you
  chose to register it.
* **`Capability::Shell` subsumes every other one.** A command can read, write and reach the
  network, so a policy that allows `Shell` has allowed all of it whatever it answers about the
  rest - unless something outside the runtime is confining the command.
* **Context can be hostile.** A fetched page, a file, an MCP server's output: anything in the
  context is something a model reads, and it can carry instructions. What this runtime offers
  against that is not a cleverer model but the two things it is built on - a policy that nothing
  in a model's output can reach except as a tool name and arguments, and a context you can *see*,
  item by item, before the next request goes out.

---

### 📡 everything is an event

```text
session.started    state.changed       model.changed      tool.requested
session.resumed    context.added       model.params       tool.unknown
session.finished   context.changed     model.requested    tool.repaired
turn.interrupted   context.replaced    model.delta        tool.started
tools.changed      context.undone      model.payload      tool.output
policy.changed     context.redone      model.finished     tool.finished
projector.changed  context.annotated   model.failed
counter.changed    context.recounted   step.failed        permission.requested
compactor.changed  context.compacted                      permission.decided
```

Every one of them carries what a client needs to render it without inferring anything. An undo
names the items it took back and the ones it reverted. A request names the items it left out, and
why. A seam being swapped names what went out and what came in — because "the projector was
replaced" leaves a reader unable to say what was projecting the requests on either side of that
line, and that is the question a log is for.

`Kernel::subscribe` is the live stream; `Kernel::history` is the complete, append-only session
log (both written under one lock, so their order agrees). Records are plain `serde` types, so
persisting a session is one line per event.

The log stays small by *naming* things rather than copying them: `model.requested` records the
context ids a request was projected from, not the messages. The one event that carries content is
`context.replaced`, and it follows the rule that makes the rest work - the log records what nothing
else can recover. An added item is still in the context; overwritten text is nowhere. The log is
unbounded on purpose (a capped append-only log is not one), and `drain_history` is how a
long-running session stays affordable: you take the records, you write them somewhere, the kernel
lets go. Nothing disappears behind your back.

---

### 💾 sessions outlive processes

The log and a snapshot answer different questions, and you want both. The log says what happened;
it stays small because an event *names* an item rather than carrying its contents - which is
exactly why it cannot rebuild a context. A `Snapshot` can:

```rust
let snapshot = kernel.snapshot();          // items, ids, states, notes, params, used call ids
std::fs::write("session.json", serde_json::to_vec(&snapshot)?)?;

// ... a process later
let kernel = Kernel::resume(Config::default(), snapshot);
```

Everything that is easy to lose comes back: a pin, the reason something was pruned, a turn's
reasoning, the signature attached to a tool call, and the identifiers already handed out - so a
resumed session cannot reuse one. A provider, a policy and the tools are yours to supply again,
because they were never the session's to remember. Naming the config resumes under a new name,
which is how a session gets forked rather than continued.

---

### 🎛️ features

Both are off by default, because neither is part of the runtime:

* `selectors` - a small language for naming context items (`17`, `tool:grep:latest`,
  `all:tool_results`, `state:elided`, `file:src/foo.rs`) that resolves to the identifiers a client
  then acts on.
* `test` - a scripted provider, dummy tools, off-the-shelf permission policies and a mechanical
  compactor, so an agent built on the kernel can be tested without a network.

---

### 📚 examples

Two offline, and API-key-free:

* **[transparency][ex-transparency]** - the whole philosophy in one run: what will be sent, a
  permission prompt, a tool that floods the context, and pruning it away. It also contains the
  permission policy and the `/context` renderer the library deliberately does not:
  `cargo run --example transparency --features selectors`
* **[compaction][ex-compaction]** - a compactor that summarizes what it drops, and the user
  putting it back anyway: `cargo run --example compaction`

Two that talk to a model:

* **[compare][ex-compare]** - the same prompt to several models at once, with proof that it *was*
  the same prompt. Every model gets a `Kernel` of its own, the same `ContextItem`s are pushed into
  each, and the fingerprint is of the serialized messages of `preview_request()`. Ask a follow-up
  and it goes on comparing, but stops claiming the requests are identical, because by then they are
  not. `EST` against `IN` is the other thing worth having: the kernel's estimate beside what the
  provider charged.
* **[panel][ex-panel]** - several models arguing about one question, in rounds, ending in a ruling
  with a tally behind it. Each round *supersedes* the last round's opinions rather than piling on
  top of them, so the context carries one item per peer however long the panel runs, and each
  panelist states its position through a tool - so the ending is arithmetic rather than a vibe.

```console
$ cargo run --example compare -- -m gemini-3.5-flash-lite -m gemini-3.5-flash \
    -s "answer in at most 40 words" "the biggest downside of Rust's orphan rule?"

INPUTS · what each model is about to be sent

  MODEL                           MSGS   ~TOKENS         LIMIT   REQUEST
  gemini-3.5-flash-lite              2        22     1,048,576   491ac859ea5e78d4
  gemini-3.5-flash                   2        22     1,048,576   491ac859ea5e78d4

  identical: every model is sent the same request, byte for byte.
```

The two networked ones share [`examples/common`][ex-common] - an OpenAI-compatible HTTP provider
and nothing else. They talk to anything that speaks that dialect, local models included:

```console
$ NACHALNIK_API_KEY=ollama NACHALNIK_BASE_URL=http://localhost:11434/v1 \
    cargo run --example compare -- -m llama3.2 -m granite4.2:3b "why the borrow checker?"
```

---

### 🧪 tests

`cargo test -p nachalnik` runs 152 offline tests - 147 unit and integration, 5 doc - covering the
context model, the selectors, the state machine, the loop, permissions, projection and tool-call
repair, token counting and calibration, compaction, and the session log. Three are worth naming:
the state machine is tested for refusing a second concurrent `step` and for a dropped one not
wedging the kernel, the log for reporting an item's states in the order they were applied (which
two threads changing one item is enough to break), and a replaced `Projector` gets a test of its
own, because a seam nothing has ever been swapped through is a claim rather than a seam.

There is also a live suite of 23, skipped when there is no key, which is the only way to check the
things a mock cannot - that the requests this crate builds are accepted by a real API, and that a
real model's answers survive the round trip through the context:

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --test-threads=1 --nocapture
```

Google AI Studio speaks the same dialect and has a free tier of its own:

```console
$ NACHALNIK_API_KEY=... NACHALNIK_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai \
  NACHALNIK_MODEL=gemini-3.5-flash cargo test --test live
```

It covers: a plain turn on the wire and the recorded payload being the one that went out; a call
whose result the model reads back, a refused call it is told about, and a truncated one; a *pruned*
tool exchange still producing a request the API accepts and an *elided* one still answering its
call; a step abandoned mid-request, a turn interrupted between requests, and an interrupt stopping
a stream already arriving; and, across a session, a paused-and-resumed permission decision, a
mid-session model swap, a whole session round-tripping through `serde`, and the calibrating counter
being told what a real request cost.

---

### 📦 the rest of the workspace

| crate | what it is |
| --- | --- |
| **[`kamchatka`][kamchatka]** | a terminal agent built on this - the thing you actually run, and the demonstration that the seams hold up under one. |
| **[`nachalnik-mcp`][nachalnik-mcp]** | a bridge to [MCP](https://modelcontextprotocol.io) servers, so that a tool somebody else wrote is a `Tool` like any other. |
| **[`nachalnik-eval`][nachalnik-eval]** | a benchmark for model introspection: the model commits to a claim about its own context, the harness moves the thing the claim was about on a forked copy, and the two are compared. |

None of the three needed a change to this crate to exist, which is the argument that its six seams
are real ones. See the [workspace readme][workspace].

---

### 🚧 status

Early, but complete for what it claims to cover: the state machine, the context model,
permissions, the event stream, sessions, and projection. Deliberately **not** included, and not
planned: MCP, subagents, an editor protocol, a daemon, a CLI, or a prompt library. Those belong on
top of it - which is the point, and which is what the rest of the workspace is for.

The crate follows [semver](https://semver.org/), and API breakage is to be expected before `1.0`.

---

### 🎸 the name

*Nachalnik Kamchatki* - "the boss of Kamchatka" - is a 1984 KINO album, named for the boiler room
where Viktor Tsoi shovelled coal while making it. A `nachalnik` is a boss, which is the joke: the
agent is not the boss, you are. `kamchatka` is the boiler room the work actually happens in.

---

### 📜 license

Licensed under the MIT License ([LICENSE-MIT][license]).

<!-- crates.io resolves a relative link against the directory the readme was published from -
     `nachalnik/` - rather than against the repository root, so every link into the tree is
     absolute. -->

[workspace]: https://github.com/ljedrz/nachalnik
[kamchatka]: https://github.com/ljedrz/nachalnik/tree/HEAD/kamchatka
[writeup]: https://ljedrz.github.io/nachalnik/
[nachalnik-mcp]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-mcp
[nachalnik-eval]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-eval
[ex-compare]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/compare.rs
[ex-panel]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/panel.rs
[ex-transparency]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/transparency.rs
[ex-compaction]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/compaction.rs
[ex-common]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/common/mod.rs
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT
