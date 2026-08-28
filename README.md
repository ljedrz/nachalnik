# nachalnik

[![crates.io](https://img.shields.io/crates/v/nachalnik.svg)](https://crates.io/crates/nachalnik)
[![docs.rs](https://docs.rs/nachalnik/badge.svg)](https://docs.rs/nachalnik)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**An agent runtime in which the context, the tools, the permissions and the requests are explicit
state you can read, change and put back - rather than decisions taken inside a framework and
reported to you afterwards.**

> The agent is not the boss. You are.

It is a library, not a program: it owns no UI, no editor, no model, no tools and no prompt. What
it owns is the loop, the context, and the paper trail.

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
kernel.set_state(ids, ContextState::Excluded, Some("13k tokens of nothing".into()));
assert_eq!(kernel.budget().used(), 126);   // it was 13,173
kernel.undo();                             // and nothing was ever destroyed
```

**Prove two models were asked exactly the same thing**, which is the difference between a
comparison and an anecdote:

```rust
// one set of items, pushed into two kernels; the digest is of what goes on the wire
let a = serde_json::to_vec(&fast.preview_request()?.messages)?;
let b = serde_json::to_vec(&smart.preview_request()?.messages)?;
assert_eq!(a, b, "byte for byte");
```

None of that is a hook, a callback or a tracing integration bolted on afterwards. It is the state
the loop runs on, and there is no step between it and the wire where the kernel adds something of
its own.

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
  a snapshot that resumes the same session in another process.

Reach for something else if you want **an agent today**. This crate ships no provider, no tools,
no prompt and no UI, so a working agent is yours to assemble; `kamchatka` in this workspace is
what that costs, and most of it is tools and rendering. If you want batteries included, `goose` and `codex`
are good and also Rust. `nachalnik` is what you build a harness *out of*.

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
  run yet, and you can look first (`pending_calls`) - or refuse
  (`cancel_pending_calls`).

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

### 🖥️ the agent you can run

`nachalnik` ships no UI, but the workspace does. **[`kamchatka`](kamchatka/)** is a terminal agent
built on it, and built to show what it is for:

```console
$ cargo run -p kamchatka -- -f src/lib.rs "what does this crate do?"
```

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│  id  label                      kind                tokens  what it says, or why it is not being sent        │
│  1 ▪ src/kernel.rs              reference            1,045  pub struct Kernel;                               │
│  2 · user                       user_message             6  what does the kernel do?                         │
│  3 · assistant                  assistant_message        7  asked for read                                   │
│  4 - read                       tool_result             15  excluded: pruned at the terminal                 │
│  5 · assistant                  assistant_message        7  asked for shell                                  │
│  6 · shell                      tool_result             10  test result: ok. 175 passed; 0 failed            │
│  7 · assistant                  assistant_message       62  The kernel is a state machine with five states. …│
│                                                                                                              │
└──────────────────────────────────────────────────────────────────────────────────────── 7 items, 1 not going ┘
┌ you ─────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ask for something, or /help                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 done · gpt-4o-mini · ~1,168 tokens, 0.9% (128k) · 1,102 really · 15 held back · F1 for the keys
```

Four tabs, each of which gets the whole window. The first is the conversation, which every
terminal agent has. The one above is the second, and it is this runtime: every item the context
holds, what it costs, whether it is going into the next request, and what the model will actually
read of it - with the ones that are *not* going saying why, on their own row, in the projector's
words. `space` takes an item out and puts it back, `p` pins it, `e` changes what it says, `enter`
reads the whole of it, `u` undoes. The third is every event the runtime emits, as it happens, in
the same names the session log is made of. The fourth is the permission policy - every capability,
what the policy will answer about it, and which tools that covers - decided in advance and changed
where it is read, rather than one prompt at a time at the moment it is least convenient.

`ctrl+p` heads that request with everything the projector left out and why - `excluded: pruned by
\`tool:shell:latest\``, `archived: the whole output; the model was shown a shortened copy`,
`dropped the call ... its result is not in the projection` - because "why is that not in there?"
is the question this runtime exists to answer, and the JSON alone can only answer the other one.

The `~` on the status line is not decoration either: that figure is an estimate from a counter
with no tokenizer, and beside it is what the provider actually charged. `/budget` reconciles them
and shows the correction the counter drew from the difference - over a real session it went from
13% low to within 0.3%.

And because the loop is a state machine rather than a function that runs to completion, `/step`
performs exactly *one* transition of it. That is the only way to stand in `Ready` - the moment the
model has said what it wants to do and none of it has run yet:

```text
· step → ready: 1 call(s) decided, none of them run yet
      shell {"cmd":"wc -l ledger.py"}
```

The command is decided, permitted, and not running. Every other agent's only checkpoint is
"approve this command?"; this one is the state machine's own, and from it you can read the call,
prune the context it would run against, drop it, or take the next transition.

It is a few hundred lines of ordinary user code on top of the crate: a provider, four tools, a
policy, a compactor and the drawing. None of it needed anything the runtime does not already hand
out - `step`, `supersede`, `cancel_pending_calls`, `remove_tool` and the seam accessors are all
public API, used from the outside like anything else.

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

That is `/seams` in `kamchatka`, and every line of it is an accessor on the kernel.

Model parameters are an opaque `serde_json` map carried to the provider verbatim, so `thinking`,
`safety_settings` and `reasoning_effort` are exactly as first-class as `temperature` - and the
kernel cannot send anything you did not ask for.

The kernel has no wire format, so `preview_request` is as far as its own guarantee reaches. A
provider that implements `render` closes the rest of the gap: `preview_payload` then shows the
payload itself, and `Config::record_payloads` puts it in the log. Be precise about what that is
worth - it is the provider's account of itself, exactly like a tool's declared capabilities, and
the kernel has nothing to check it against. Render once and send what you rendered; a preview that
has quietly stopped matching is worse than none.

A reasoning model's own thinking is treated the same way: it is recorded on the turn that
produced it, counted like everything else, and offered back to the provider in
`Message::reasoning`, because some APIs verify a signed thinking block against the turn it came
from and a runtime that dropped it could not talk to them. It is never separated from its turn,
and `LinearProjector::send_reasoning` decides whether it goes back out.

A projector decides which items become which messages, and it is worth being exact about how far
that reaches. Tool results inside a user turn, thinking-only turns kept rather than dropped, the
whole conversation flattened into one string - each of those is a projector away. What is not is a
dialect whose assistant turn is an *ordered* list of typed blocks, with thinking interleaved
between tool calls: `Message` has one content slot, so the order is not something a projector has
anywhere to put. A provider for such an API reassembles a conventional order and is right until
the model interleaves. Lifting that means blocks in `Content`, which changes what the context
holds rather than how it is projected, and it is not done.

`ToolCall::extra` is the same idea at the level of a single call: whatever a provider attaches to
a call - Google's `thought_signature`, an encrypted reasoning item - is carried back attached to
that call, verbatim and uninterpreted. Gemini rejects the *next* request outright when it goes
missing, which is the sort of thing you only find out by asking a real API.

---

### 🔍 context as a data structure

Items are public data - an id, a kind, a source, a label, content, a size, a state, a note and
whatever metadata you attach - so a client can render `/context` however it likes:

```text
INSTRUCTIONS                           9
  └─ [1] AGENTS.md                     9
SELECTIONS                            12
  └─ [2] src/parser.rs:12-14          12
USER                                   5
  └─ [4] user                          5
TOOL RESULTS                      13,058
  ├─ [6] read_file                    18
  └─ [8] shell                    13,040

TOOLS                                 62
TOTAL                             13,173
LIMIT                            128,000
```

One method covers every state change, and each call is one undoable operation:

```rust
kernel.set_state(ids, ContextState::Excluded, Some("an enormous test output".into()));
kernel.set_state(ids, ContextState::Pinned, None);
kernel.replace(id, "a shorter version")?;          // new contents, same identifier
kernel.supersede(old, ContextItem::file(path, reread))?;  // this one replaces that one
kernel.annotate(id, json!({ "expendable": true }))?;      // a hint for your compactor
kernel.push_all(files);                            // one operation, so one undo
kernel.undo();
kernel.redo();
```

`set_state` says what it did to each identifier - `changed`, `unchanged`, `unknown` - because
"there is no item 12" and "item 12 was already pruned" are different things to tell somebody.

Excluding an item removes it from the *projection*, not from the record: it keeps its identifier,
is still listed and inspectable, and comes back with another `set_state`, an `undo`, or a `redo`.
The default projector also drops the other half of a tool call/result pair when one side is gone,
so pruning cannot produce a request the provider will reject - and it says so in
`Projection::repairs` *and* in the session log, because a request the kernel quietly adjusted is
exactly the one you want to be able to ask about afterwards.

**Nothing is destroyed, including by a limit.** A tool output over its limit is recorded twice:
the whole of it, archived, and the shortened copy the model is shown. Putting the whole thing back
in front of the model is a `set_state` like any other, rather than a re-run of the tool. Keeping
it costs a pointer rather than a copy: content, tool-call arguments and tool schemas are all
shared, so pruning a four-megabyte tool result moves a pointer, projecting it into a request moves
a pointer, and the event recording it points at the same bytes the context holds rather than a
second copy of them. Set
`Config::keep_truncated_output` to `false` when a tool can produce more than you are willing to go
on holding; the truncation is still reported either way.

One agent is one kernel, and a fleet of them shares nothing but whatever you hand to both, so
running sixteen at once needs no coordination at all. Within a single turn, the tools a model
asks for run one at a time in the order it asked - which is something you can build on, since two
edits to the same file then apply in sequence. `Config::parallel_tool_calls` gives that up for
speed, on purpose and never by default: nothing in the kernel can tell whether a model's calls are
independent, so the judgement is yours. Either way the results are recorded in the order the model
asked for them, so the context does not depend on which tool happened to be quick. Several threads on a *single* kernel are
fine too - reading is cheap and every mutation is atomic, so a client can render, prune and
preview while a turn is in flight. The one thing two threads cannot do is drive the loop at the
same time, which is `Error::Busy` rather than a second request.

Automatic management is allowed, invisible management is not. A `Compactor` gets the budget and
the items and returns a plan; the kernel refuses to remove anything pinned, applies the rest,
and broadcasts a report of exactly what it did - which the user can then disagree with.

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
noise, and a counter that chased it would be wrong for every request that matters. It corrects what
is counted *from then on*: figures already recorded on items do not silently rewrite themselves,
because that is exactly the sort of thing this crate does not do - `Kernel::recount` rewrites them
when you ask, and says so on the event stream.

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

### 📡 everything is an event

```text
session.started    state.changed       model.changed      tool.requested
session.resumed    context.added       model.params       tool.unknown
session.finished   context.changed     model.requested    tool.repaired
turn.interrupted   context.replaced    model.delta        tool.started
tools.changed      context.undone      model.payload      tool.output
policy.changed     context.redone      model.finished     tool.finished
                   context.annotated   model.failed
                   context.recounted   step.failed        permission.requested
                   context.compacted                      permission.decided
```

Every one of them carries what a client needs to render it without inferring anything: an undo
names the items it took back and the ones it reverted, and a request names the items it left out
and why.

`Kernel::subscribe` is the live stream; `Kernel::history` is the complete, append-only session
log (both written under one lock, so their order agrees). Records are plain `serde` types, so
persisting a session is one line per event.

The log stays small by *naming* things rather than copying them: `model.requested` records the
context ids a request was projected from, not the messages, so twelve requests over one
conversation cost twelve short lists of integers instead of twelve copies of it. The one event
that carries content is `context.replaced`, and it follows the rule that makes the rest work - the
log records what nothing else can recover. An added item is still in the context; overwritten text
is nowhere. Without it, a request replayed from its item ids would reconstruct the wrong bytes.

That matters in memory, where the log is a live structure. On disk it matters much less than it
looks: a log whose entries repeat each other is the best case an ordinary compressor gets, and
twenty-four turns come to 71 KB raw and 3.5 KB under `xz` - or 1.3 MB and 4.8 KB with
`record_payloads` on. So keep the *log* lean for RAM, use `drain_history` to hand records off on a
schedule, and let the filesystem worry about the rest. The log is unbounded on purpose - a capped
append-only log is not one - and `drain_history` is how a long-running session stays affordable:
you take the records, you write them somewhere, the kernel lets go. Nothing disappears behind
your back.

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
  `all:tool_results`, `file:src/foo.rs`) that resolves to the identifiers a client then acts on.
* `test` - a scripted provider, dummy tools, off-the-shelf permission policies and a mechanical
  compactor, so an agent built on the kernel can be tested without a network.

---

### 📚 examples

**[compare](nachalnik/examples/compare.rs)** - the same prompt to several models at once, with proof that
it *was* the same prompt:

```console
$ cargo run --example compare -- -m gemini-3.5-flash-lite -m gemini-3.5-flash \
    -s "answer in at most 40 words" "the biggest downside of Rust's orphan rule?"

INPUTS · what each model is about to be sent

  MODEL                           MSGS   ~TOKENS         LIMIT   REQUEST
  gemini-3.5-flash-lite              2        22     1,048,576   491ac859ea5e78d4
  gemini-3.5-flash                   2        22     1,048,576   491ac859ea5e78d4

  identical: every model is sent the same request, byte for byte.

ANSWERS

  MODEL                             TIME      EST       IN      OUT    THINK   STOP
  gemini-3.5-flash-lite            1.06s       22       23       42        -   end_turn
  gemini-3.5-flash                 9.77s       22       23       37        -   end_turn
```

A comparison means something only if the model was the only thing that differed, and that is
normally taken on trust: the harness assembles a prompt somewhere inside itself, once per model,
and hands you the answers. Here every model has a `Kernel` of its own, the same `ContextItem`s
are pushed into each, and the fingerprint is of the serialized messages of `preview_request()` -
the projection the request is actually built from. Ask a follow-up and it goes on comparing, but
it stops claiming the requests are identical, because by then they are not:

```text
  diverged: each context now also holds that model's own answers. What the user
  put there is still identical in all of them (6656ed5bd5e496ba).
```

`EST` against `IN` is the other thing worth having: the kernel's own estimate beside what the
provider charged for. `--payload` prints the exact bytes each provider will send, `--save DIR`
writes every session as a log and a snapshot, and each one is resumable with `kamchatka -r`.

**[panel](nachalnik/examples/panel.rs)** - several models arguing about one question, in rounds, ending in
a ruling with a tally behind it:

```console
$ cargo run --example panel -- -m gemini-3.5-flash-lite -m gemini-3.5-flash \
    "should a library expose anyhow::Error in its public API, or an error enum?"

ROUND 2 · 2 peer opinions circulated, superseding the last ones

  gemini-3.5-flash-lite        1.00s   Hand-written error enum (100%)
  gemini-3.5-flash             8.32s   Hand-written error enum (100%)

CONTEXTS · what each panelist actually read

  gemini-3.5-flash-lite — 14 items, 1 superseded, ~1,043 tokens
    [1]  instruction panel rules                          86
    [2]  user        user                                 43
    [3]  said        assistant                            98
    [4]  panel       gemini-3.5-flash · round 1          104  (superseded by item 8)
    [5]  user        user                                 32
    [6]  ballot      assistant                           185
    [7]  recorded    state_position                        5
    [8]  panel       gemini-3.5-flash · round 2          108
```

Round one is independent - nobody has read anybody. From then on each panelist receives the
others' opinions as context items of its own, attributed and counted, and every round
*supersedes* the last round's rather than piling on top of it, so the context carries one item
per peer however long the panel runs. The superseded ones are still listed, still sized, still
restorable; they are simply not in the next request.

Each panelist also states its position through a tool, so the ending is arithmetic rather than a
vibe: who moved, who held, who never stated a position at all. Nothing is inferred on a model's
behalf - a panelist that ignored the ballot is reported as having ignored it, and an abstention
is never counted as agreement.

Two more, offline and API-key-free:

* **[transparency](nachalnik/examples/transparency.rs)** - the whole philosophy in one run: what will be
  sent, a permission prompt, a tool that floods the context, and pruning it away. It also
  contains the permission policy and the `/context` renderer the library deliberately does not:
  `cargo run --example transparency`
* **[compaction](nachalnik/examples/compaction.rs)** - a compactor that summarizes what it drops, and the
  user putting it back anyway: `cargo run --example compaction`

The three networked ones share [`examples/common`](nachalnik/examples/common/mod.rs) - an OpenAI-compatible
HTTP provider and nothing else, because a server-sent-event parser is not what any of them is
about. They talk to anything that speaks that dialect, local models included:

```console
$ NACHALNIK_API_KEY=ollama NACHALNIK_BASE_URL=http://localhost:11434/v1 \
    cargo run --example compare -- -m llama3.2 -m granite4.2:3b "why the borrow checker?"
```

---

### 🧪 tests

`cargo test -p nachalnik` runs 113 offline tests: the context model, the selectors, the state
machine (including that a second concurrent `step` is refused and that a dropped one does not
wedge the kernel), the loop, permissions, projection and tool-call repair, token counting and
calibration, compaction, and the session log. A replaced `Projector` gets its own test, because a
seam nothing has ever been swapped through is a claim rather than a seam.

`cargo test --workspace` runs 203 in all: those, the bridge's 23 - which stand a real MCP server
up rather than mocking one - and `kamchatka`'s 43, which draw its screen and read the characters
back, plus one that puts a socket in front of it that answers and then goes silent.

Every count and every percentage in this file was measured at the commit it was written for,
against a real API where it says so. They are here because a claim with a number in it can be
checked and a claim without one cannot - but the current answer is always `cargo test --workspace`
and `/budget`, not this page.

There is also a live suite, which is the only way to check the things a mock cannot - that the
requests this crate builds are accepted by a real API, and that a real model's answers survive
the round trip through the context:

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --test-threads=1 --nocapture
```

Google AI Studio speaks the same dialect and has a free tier of its own:

```console
$ NACHALNIK_API_KEY=... \
  NACHALNIK_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai \
  NACHALNIK_TEST_MODEL=gemini-3.5-flash-lite \
  cargo test --test live -- --test-threads=1 --nocapture
```

It skips itself when there is no key (and reads only `OPENROUTER_API_KEY` /
`NACHALNIK_API_KEY`, never a stray `OPENAI_API_KEY`), defaults to a small free model, costs
about thirty requests per run, waits out momentary upstream rate limits, and skips rather than
fails when a free-tier key has spent its daily allowance. Nineteen tests cover: a plain turn,
the provider's own context limit, a system instruction, a labelled reference, streamed fragments
adding up to the answer, a tool call whose result the model reads back, a paused-and-resumed
permission decision, a refused call the model is told about, a *pruned* tool exchange still
producing a request the API accepts, a truncated tool result, compaction before a request, an
opaque parameter reaching the model, a mid-session model swap, a whole session round-tripping
through `serde`, the recorded payload being the one that actually went out, a step abandoned
mid-request leaving the kernel usable, a turn interrupted between requests, an interrupt stopping
a stream that is already arriving, and the calibrating counter being told what a real request
cost.

---

### 📦 the workspace

| crate | what it is |
| --- | --- |
| **[`nachalnik`](nachalnik/)** | the runtime. Five dependencies, no `unsafe`, no network, no prompt. This is the part that matters, and it is meant to stay boring. |
| **[`nachalnik-mcp`](nachalnik-mcp/)** | a bridge to [MCP](https://modelcontextprotocol.io) servers, so that a tool somebody else wrote is a `Tool` like any other. |
| **[`kamchatka`](kamchatka/)** | a terminal agent built on the runtime - the thing you actually run, and the demonstration that the seams hold up under one. |
| `nachalnik-utils` | never published, permanently `0.0.0`. The OpenAI-compatible provider this crate's own examples and live tests talk through, so that scaffolding is written once rather than three times. A *dev*-dependency, which is the whole trick: cargo strips those from a published manifest, so a crate only ever dev-depended on never has to exist on the registry. |

The bridge is deliberately *not* in the core. Speaking MCP means spawning processes, opening
sockets and reading notifications in the background, and the runtime promises to do none of those;
it would also tie a context library's version to a protocol that revises faster than the library
should.

It is also the answer to the obvious question about a crate this abstract - whether the seams are
real. Writing it needed **no change to the runtime at all**: an MCP tool is a `Tool` that forwards
to a server, tools arriving and leaving are `add_tool` and `remove_tool`, a structured result is
`Content::Json`. And it pushed back on one thing worth knowing about: MCP tool annotations are
*hints*, the specification says a client should never make tool-use decisions on hints from an
untrusted server, so the bridge believes none of them by default. Its tests include a server
offering a tool called `delete_everything` that claims to be read-only.

---

### 🚧 status

Early, but complete for what it claims to cover: the state machine, the context model,
permissions, the event stream, sessions, and projection. Deliberately **not** included, and not
planned for the core: MCP, subagents, an editor protocol, a daemon, a CLI, or a prompt library.
Those belong on top of it - which is the point, and which is what the rest of the workspace is
for.

The crate follows [semver](https://semver.org/), and API breakage is to be expected before
`1.0`.

---

### 🎸 the name

*Nachalnik Kamchatki* - "the boss of Kamchatka" - is a 1984 KINO album, named for the boiler room
where Viktor Tsoi shovelled coal while making it. A `nachalnik` is a boss, which is the joke: the
agent is not the boss, you are. `kamchatka` is the boiler room the work actually happens in.

---

### 📜 license

Licensed under the MIT License ([LICENSE-MIT](LICENSE-MIT)).
