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
  (`cargo run --example compare_models`, `--example panel`)
* **Editor and IDE integration.** A `/context` view, a permission prompt and an undo that are
  yours to render, over a loop that stops between transitions instead of acting and reporting.
* **Anything that has to be auditable or reproducible.** An append-only log of typed events, plus
  a snapshot that resumes the same session in another process — and an assistant turn recorded in
  the order the model produced it, thinking and tool calls interleaved, rather than rearranged
  into whichever shape the wire format wanted.
* **Agents that read and manage their own context.** Everything here is public API a `Tool` can
  call, so the same view and the same controls can be handed to the model. `kamchatka` does; see
  the [write-ups][writeup] - five sessions where an agent found a false note in its own context and
  rewrote it, took back a hallucination of its own the same way, ran an ablation on itself rather
  than answer from theory, and - in the two where I am the one editing - carried on from words I
  put in its mouth and retracted a true statement after I hid the evidence for it.

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

### 📖 the parts that are not obvious

Six of them, in [CONCEPTS.md][concepts], each with the reasoning that put it there: the **context
as a data structure** rather than a list of strings, why **a turn keeps the order it was produced
in** and what that costs a dialect that cannot, **a budget that corrects itself** from what
providers actually charge and says out loud what it could not price, what **stopping** means when
a request is already in flight, why **everything is an event** and the log names things rather
than copying them, and how **sessions outlive processes** through a snapshot that is deliberately
not the log.

### 🎛️ features

Both are off by default, because neither is part of the runtime:

* `selectors` - a small language for naming context items (`17`, `tool:grep:latest`,
  `all:tool_results`, `state:elided`, `file:src/foo.rs`) that resolves to the identifiers a client
  then acts on.
* `test` - a scripted provider, dummy tools, off-the-shelf permission policies and a mechanical
  compactor, so an agent built on the kernel can be tested without a network.

---

### 📚 examples

Three offline, and API-key-free:

* **[transparency][ex-transparency]** - the whole philosophy in one run: what will be sent, a
  permission prompt, a tool that floods the context, and pruning it away. It also contains the
  permission policy and the `/context` renderer the library deliberately does not:
  `cargo run --example transparency --features selectors`
* **[compaction][ex-compaction]** - a compactor that summarizes what it drops, and the user
  putting it back anyway: `cargo run --example compaction`
* **[pricing_a_picture][ex-pricing]** - one context counted three ways, and what `Blob::meta` and
  `TokenCounter::uncounted` are for: a counter that knows a vendor's tiling formula, and the same
  counter handed a payload nobody measured. `cargo run --example pricing_a_picture`

Two that talk to a model:

* **[compare_models][ex-compare]** - the same prompt to several models at once, with proof that it *was*
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
$ cargo run --example compare_models -- -m gemini-3.5-flash-lite -m gemini-3.5-flash \
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
    cargo run --example compare_models -- -m llama3.2 -m granite4.2:3b "why the borrow checker?"
```

---

### 🧪 tests

`cargo test -p nachalnik` runs the offline suite, covering the context model, the selectors, the
state machine, the loop, permissions, projection and tool-call repair, token counting and
calibration, compaction, and the session log. Three are worth naming:
the state machine is tested for refusing a second concurrent `step` and for a dropped one not
wedging the kernel, the log for reporting an item's states in the order they were applied (which
two threads changing one item is enough to break), and a replaced `Projector` gets a test of its
own, because a seam nothing has ever been swapped through is a claim rather than a seam.

There is also a live suite, skipped when there is no key, which is the only way to check the
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
[ex-compare]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/compare_models.rs
[ex-panel]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/panel.rs
[ex-transparency]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/transparency.rs
[ex-compaction]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/compaction.rs
[ex-pricing]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/pricing_a_picture.rs
[ex-common]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/common/mod.rs
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT

[concepts]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/CONCEPTS.md
