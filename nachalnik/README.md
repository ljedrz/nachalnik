# nachalnik

[![crates.io](https://img.shields.io/crates/v/nachalnik.svg)](https://crates.io/crates/nachalnik)
[![docs.rs](https://docs.rs/nachalnik/badge.svg)](https://docs.rs/nachalnik)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**An agent runtime in which the context, the tools, the permissions and the requests are explicit
state — state you can read, change and put back.** Not decisions taken inside a framework and
reported to you afterwards.

> The agent is not the boss. You are.

It is a library, not a program: it has no UI, no editor, no model, no tools and no prompt. What it
has is the loop, the context, and a record of everything that happened. The rest of the
[workspace][workspace] — a terminal agent, an MCP bridge, an introspection benchmark — is what
gets built on top.

**Looking for an agent to run rather than one to build?** That is [`kamchatka`][kamchatka], a
terminal agent on this runtime that shows you everything in its context and lets you change it,
and the [transcripts][writeup] are what its sessions look like.

---

### ⏱️ in thirty seconds

**See the exact request before it goes out** — not a trace of it afterwards:

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
kernel.undo()?;                            // and it is back as it was, under the same identifier
```

**Stop at any step of the loop.** In `Ready`, the model has said which tools it wants and none of
them have run:

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
  whole previewed request showing that the only variable was the model — and the tokenizers
  disagreeing with each other about identical bytes, which you can see rather than assume.
  (`cargo run --example compare_models`, `--example panel`)
* **Editor and IDE integration.** A `/context` view, a permission prompt and an undo that you
  render yourself, over a loop that pauses at each step instead of acting first and reporting
  afterwards.
* **Anything that has to be auditable or reproducible.** An append-only log of typed events, plus
  a snapshot that resumes the same session in another process — and an assistant turn recorded in
  the order the model produced it, thinking and tool calls interleaved, rather than rearranged
  into whichever shape the wire format wanted.
* **Agents that read and manage their own context.** Everything here is public API a `Tool` can
  call, so the same view and the same controls can be handed to the model. `kamchatka` does, and
  the [write-ups][writeup] are sessions of it.

Reach for something else if you want **an agent today**. This crate ships no provider, no tools,
no prompt and no UI, so you have to assemble a working agent yourself; [`kamchatka`][kamchatka] in
this workspace shows how much work that is, and most of it is tools and rendering. If you want batteries
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
* **Small.** A handful of dependencies, no `unsafe`, and a codebase you can read in an afternoon.

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
  run yet, and you can look first (`pending_calls`) — or refuse (`cancel_pending_calls`).

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
    State::Idle => { /* the turn's request budget ran out, or an interrupt stopped it */ }
    State::Ready { .. } => { /* an interrupt stopped it before the calls ran */ }
    other => unreachable!("a turn does not end in {other:?}"),
}

// and the context remains yours
kernel.set_state([file], ContextState::Excluded, Some("too big".into()));
kernel.undo()?;
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
| `Compactor` | what to drop when it fills up | the veto on pinned items, the report, and saying when nothing more can go |

Each of them can also say what it is, so a client can put the parts a session is running with on a
screen.

Model parameters are an opaque `serde_json` map carried to the provider verbatim, so `thinking`,
`safety_settings` and `reasoning_effort` are exactly as first-class as `temperature` — and the
kernel cannot send anything you did not ask for.

The kernel has no wire format, so its own guarantee ends at `preview_request`. A provider that
implements `render` covers the rest: `preview_payload` then shows the actual payload, and
`Config::record_payloads` puts it in the log. That payload is the provider's own claim, like a
tool's declared capabilities, and the kernel can't verify it. So providers should render once and
send exactly what they rendered; a preview that no longer matches what is sent is worse than none.

A reasoning model's thinking is kept too. It is recorded on the turn that produced it, counted
like everything else, and passed back to the provider in `Message::reasoning`, since some APIs
check a signed thinking block against the turn it came from and reject requests without it. It
always stays with its turn, and `LinearProjector::send_reasoning` decides whether it is sent back.
`ToolCall::extra` does the same for individual calls: whatever a provider attaches to a call is
sent back with that call, unchanged and uninterpreted.

---

### 🔓 what it does and does not protect you from

**The kernel executes nothing.** No filesystem code, no network code, no process spawning; every
side effect in a session happens inside a `Tool` you wrote and registered. So there is nothing here
to contain, and there will be no sandbox in this crate — containment belongs where the process is
actually spawned, which is your tool or the program around it. ([`kamchatka`][kamchatka] is the one
in this workspace that runs the commands a model asks for, so it is the one that confines them:
Landlock for the filesystem, and a seccomp filter that holds every attempt to reach the network
until somebody answers it.)

The runtime enforces one thing: a call the `PermissionPolicy` refused is never passed to
`Tool::invoke`, and the refusal is recorded as an event and as a tool result the model sees. It is
a checkpoint with a record, not a security boundary. The refusal says which *kind* it was, since
that's what a model needs to decide what to do next: a standing rule means the same call will be
refused again, while a refusal of *this* call means a different approach might be allowed. The
kernel knows which kind it was, because it resolved the decision. It doesn't know *why*, so it
asks: `PermissionPolicy::why` returns `None` by default, and whatever a policy returns goes into the
tool result next to the kernel's own explanation.

What it does not protect you from, by design:

* **A `Capability` is a declaration, not a verified property.** A tool that declares `fs:read` and
  opens a socket is lying, and the kernel has nothing to check it against. The defence is that you
  chose to register it.
* **`exec:run` subsumes every other one.** A command can read, write and reach the network, so a
  policy that allows it has allowed all of it whatever it answers about the rest — unless something
  outside the runtime is confining the command.
* **Context can be hostile.** A fetched page, a file, an MCP server's output: anything in the
  context is something a model reads, and it can carry instructions. What this runtime offers
  against that is not a cleverer model but the two things it is built on — a policy that nothing
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

* `selectors` — a small language for naming context items (`17`, `tool:grep:latest`,
  `all:tool_results`, `state:elided`, `file:src/foo.rs`) that resolves to the identifiers a client
  then acts on.
* `test` — a scripted provider, dummy tools, off-the-shelf permission policies and a mechanical
  compactor, so an agent built on the kernel can be tested without a network.

---

### 📚 examples

Three offline, and API-key-free:

* **[transparency][ex-transparency]** — the whole philosophy in one run: what will be sent, a
  permission prompt, a tool that floods the context, and pruning it away. It also contains the
  permission policy and the `/context` renderer the library deliberately does not:
  `cargo run --example transparency --features selectors`
* **[compaction][ex-compaction]** — a compactor that summarizes what it drops, and the user
  putting it back anyway: `cargo run --example compaction`
* **[pricing_a_picture][ex-pricing]** — one context counted three ways, and what `Blob::meta` and
  `TokenCounter::uncounted` are for: a counter that knows a vendor's tiling formula, and the same
  counter handed a payload nobody measured. `cargo run --example pricing_a_picture`

Two that talk to a model:

* **[compare_models][ex-compare]** — the same prompt to several models at once, with proof that it
  *was* the same prompt. Every model gets a `Kernel` of its own, the same `ContextItem`s are
  pushed into each, and the fingerprint is of the whole serialized `preview_request()`. Ask
  a follow-up and it goes on comparing, but stops claiming the requests are identical, because by
  then they are not. `EST` against `IN` puts the kernel's estimate beside what the provider
  charged.
* **[panel][ex-panel]** — several models arguing about one question, in rounds, ending in a ruling
  with a tally behind it. Each round *supersedes* the last round's opinions rather than piling on
  top of them, so the context carries one item per peer however long the panel runs, and each
  panelist states its position through a tool — so the ending is arithmetic rather than a vibe.

Both talk through [`nachalnik-providers`][nachalnik-providers] to anything that speaks the OpenAI
dialect, local models included, configured by `NACHALNIK_API_KEY`, `NACHALNIK_BASE_URL` and the
models named on the command line:

```console
$ cargo run --example compare_models -- -m <model> -m <another model> "the biggest downside of Rust's orphan rule?"
```

---

### 🧪 tests

`cargo test -p nachalnik` runs the offline suite. The live suite, skipped without a key, is the
only way to check what a mock cannot — that a real API accepts the requests this crate builds, and
that a real model's answers survive the round trip through the context:

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --test-threads=1 --nocapture
```

`NACHALNIK_BASE_URL` and `NACHALNIK_API_KEY` point it at any other OpenAI-compatible endpoint.

---

### 📦 the rest of the workspace

| crate | what it is |
| --- | --- |
| **[`kamchatka`][kamchatka]** | a terminal agent built on this, for Linux — the thing you actually run, with a confined shell, a permission policy in front of every call, and sessions that can be served and rejoined. |
| **[`nachalnik-mcp`][nachalnik-mcp]** | a bridge to [MCP](https://modelcontextprotocol.io) servers, so that a tool somebody else wrote is a `Tool` like any other. |
| **[`nachalnik-eval`][nachalnik-eval]** | a benchmark for model introspection: the model commits to a claim about its own context, the harness moves the thing the claim was about on a forked copy, and the two are compared. |
| **[`nachalnik-providers`][nachalnik-providers]** | the three APIs this workspace talks — OpenAI chat-completions (and its Responses API), Google's `generateContent` and Anthropic's Messages API — as `Provider`s, streamed, retried and interruptible. |

None of them needed any change to this crate, which shows that its six traits really are
replaceable. See the [workspace readme][workspace].

---

### 📌 status

Complete for what it claims to cover — the state machine, the context model, permissions, the event
stream, sessions, and projection — with every invariant it states held by a test. Deliberately
**not** included, and not planned for the core: MCP, subagents, an editor protocol, a daemon, a CLI,
or a prompt library. Those belong on top of it, and that is what the rest of the workspace is for.

The crate follows [semver](https://semver.org/), and API breakage is to be expected before `1.0`.

---

### 🎸 the name

*Nachalnik Kamchatki* — "the boss of Kamchatka" — is a 1984 KINO album, named for the boiler room
where Viktor Tsoi shovelled coal while making it. A `nachalnik` is a boss, which is the joke: the
agent is not the boss, you are. `kamchatka` is the boiler room the work actually happens in.

---

### 📜 licence

Licensed under the MIT License ([LICENSE-MIT][license]).

<!-- crates.io resolves relative links against the directory this README was published from
     (`nachalnik/`), not the repository root, so links into the tree are absolute. -->

[workspace]: https://github.com/ljedrz/nachalnik
[kamchatka]: https://github.com/ljedrz/nachalnik/tree/HEAD/kamchatka
[writeup]: https://ljedrz.github.io/nachalnik/
[nachalnik-mcp]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-mcp
[nachalnik-eval]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-eval
[nachalnik-providers]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-providers
[ex-compare]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/compare_models.rs
[ex-panel]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/panel.rs
[ex-transparency]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/transparency.rs
[ex-compaction]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/compaction.rs
[ex-pricing]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/pricing_a_picture.rs
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT

[concepts]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/CONCEPTS.md
