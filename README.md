# nachalnik

[![crates.io](https://img.shields.io/crates/v/nachalnik.svg)](https://crates.io/crates/nachalnik)
[![docs.rs](https://docs.rs/nachalnik/badge.svg)](https://docs.rs/nachalnik)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**An agent runtime in which the context, the tools, the permissions and the requests are explicit
state — state you can read, change and put back.** Not decisions taken inside a framework and
reported to you afterwards.

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

**Hand the agent the same controls.** A `Tool` is ordinary code and the kernel it belongs to is
ordinary public API, so an agent that reads its own budget and drops what it is finished with is a
tool somebody wrote — not a runtime feature, and not a promise the runtime has to keep:

```rust
let budget = kernel.budget();                  // what the next request costs, and against what
let worst = kernel.items().into_iter().max_by_key(|item| item.tokens);
kernel.set_state(worst.map(|i| i.id), ContextState::Elided, Some("done with it".into()));
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
  a snapshot that resumes the same session in another process — and an assistant turn recorded in
  the order the model produced it, thinking and tool calls interleaved, rather than rearranged
  into whichever shape the wire format wanted.
* **Agents that read and manage their own context.** Everything above is public API a `Tool` can
  call, so the same view and the same controls can be handed to the model. `kamchatka` does; see
  [below](#-the-same-handles-given-to-the-agent), or the [write-ups][writeup] of sessions where an
  agent found a false note in its own context and rewrote it, and where one took back a
  hallucination of its own the same way.

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

`nachalnik` ships no UI, but the workspace does. **[`kamchatka`][kamchatka]** is a terminal agent
built on it, and built to show what it is for:

```console
$ cargo run -p kamchatka -- -f src/lib.rs "what does this crate do?"
```

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│  id  label         kind               sending   held  what it says, or why it is not being sent            │
│  1 ▪ src/kernel.rs reference            1,045         pub struct Kernel;                                    │
│  2 · user          user_message             6         what does the kernel do?                              │
│  3 · assistant     assistant_message        7         asked for read                                        │
│  4 - read          tool_result              0     15  excluded: pruned at the terminal                      │
│  5 · assistant     assistant_message        7         asked for shell                                       │
│  6 … shell         tool_result             11  9,004  compaction: compacted to make room                    │
│  7 · assistant     assistant_message       62         The kernel is a state machine with five states. …     │
│                                                                                                              │
└────────────────────────────────────────────────────────────────────────────── 7 items, 2 not going, 1 elided ┘
┌ you ─────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ask for something, or /help                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 done · gpt-4o-mini @ openrouter.ai · ~1,168 tokens, 0.9% (128k) · 1,102 really · 15 held back · F1 for the keys
```

Four tabs, each of which gets the whole window. The first is the conversation, which every
terminal agent has. The one above is the second, and it is this runtime: every item the context
holds, what it costs, whether it is going into the next request, and what the model will actually
read of it - with the ones that are *not* going saying why, on their own row, in the projector's
words. Item 6 is `…`, elided: it is *in* the request, as a one-line marker saying it was compacted
away, so the call on row 5 still has an answer and the model is not left reading a conversation in
which it never asked for anything. What it holds is counted as held back rather than spent, and
`space` spends it again on the way round. That key cycles how much of an item the model gets - all
of it, a marker, nothing, all of it - and `p` pins it, `e` changes what it says, `u` undoes. `enter` reads
the whole of it, in pages `←` and `→` move between: what the request will actually contain for it,
what the item itself holds, and what it said before something rewrote it. The third is every event the runtime emits, as it happens, in
the same names the session log is made of. The fourth is the permission policy - every
answer somebody has actually given, what it covers, and how many things are still a question -
changed where it is read rather than one prompt at a time at the moment it is least convenient.

`ctrl+p` heads that request with everything the projector left out and why - `excluded: pruned by
\`tool:shell:latest\``, `archived: the whole output; the model was shown a truncated copy`,
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

`--gemini` swaps the wire format for Google's own, so a turn keeps the order it was produced in
(see [below](#-a-turn-keeps-the-order-it-was-produced-in)); `--introspect` hands the model the same
context controls the keys give you ([below](#-the-same-handles-given-to-the-agent)).

It is a couple of thousand lines of ordinary user code on top of the crate: two providers, six
tools, a policy, a compactor and the drawing. None of it needed anything the runtime does not
already hand out - `step`, `supersede`, `cancel_pending_calls`, `remove_tool`, `snapshot`,
`resume`, `budget`, `set_state` and the seam accessors are all public API, used from the outside
like anything else.

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

A reasoning model's own thinking is treated the same way. It is recorded on the turn that produced
it, counted like everything else, and offered back to the provider in `Message::reasoning` — some
APIs verify a signed thinking block against the turn it came from, and a runtime that dropped it
could not talk to them. It is never separated from its turn, and `LinearProjector::send_reasoning`
decides whether it goes back out.

A projector decides which items become which messages, and — for an assistant turn — which of two
shapes it goes out in. Tool results inside a user turn, thinking-only turns kept rather than
dropped, the whole conversation flattened into one string, or an ordered list of typed blocks:
each of those is a projector away, the last of them through `LinearProjector::send_blocks`. What a
projector cannot do is recover an order that was never recorded, which is why the order lives in
the *content* — see [a turn keeps the order it was produced in](#-a-turn-keeps-the-order-it-was-produced-in).

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

Excluding an item removes it from the *projection*, not from the record. It keeps its identifier,
stays listed and inspectable, and comes back with another `set_state`, an `undo`, or a `redo`.

The default projector also drops the other half of a tool call/result pair when one side is gone,
so pruning cannot produce a request the provider will reject. It says so in `Projection::repairs`
*and* in the session log — a request the kernel quietly adjusted being exactly the one you want to
be able to ask about afterwards.

**Nothing is destroyed, including by a limit.** A tool output over its limit is recorded twice:
the whole of it, archived, and the truncated copy the model is shown. Putting the whole thing back
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

`kamchatka --gemini` is what produces one. Google's `generateContent` reports `content.parts[]`
and maps onto `Block` one for one; the OpenAI-compatible shim in front of the *same model*
flattens it, because the dialect it imitates has nowhere to put an order. It also answers
`400 Function call is missing a thought_signature` to a request that hands a turn back altered,
which is what `Part::extra` is for: what a provider attached to a piece of a turn rides back out
on the piece it arrived on, unread.

---

### 🪞 the same handles, given to the agent

Everything above is public API. A `Tool` can call all of it — so `kamchatka --introspect` offers
the model two more tools, and neither of them needed a line added to the runtime.

**`introspect` reads.** `look` lists what is being carried, and reads any of it back block by
block. `budget` reports what the next request costs against the limit, and which items are the
expensive ones. `request` shows what is about to be sent. `draft` and `fork` answer on a throwaway
copy of the context, so an answer can be read before it is given.

**`amend` changes.** `prune` elides, excludes or pins items by id or by selector. `revise`
rewrites one. `note` writes something down where compaction cannot reach it. And `undo` walks back
the changes *it* made.

Given a 10,000-token limit, no compactor, and a mundane question about a repository, one model's
first move was to look before it moved:

```text
─── request 1: 2 msgs, ~1358 tokens
    → introspect({"action": "budget"})
```

*"I'm realizing that 10,000 tokens is a tight budget!"* — and every shell command it wrote from
then on ended in `| sort -n | tail -n 10`. Eight requests later:

```text
─── request 9: 18 msgs, ~6356 tokens
    → amend({"action":"prune","select":"all:tool_results","state":"elide",
             "reason":"pruning tool results to save context budget"})
      [4] active → elided   [6] active → elided   [8] active → elided   … 8 items
─── request 10: 20 msgs, ~4354 tokens
```

One call, eight items, two thousand tokens back — and nothing destroyed: every one of them is
still listed, still inspectable, and one `set_state` from coming back.

A second run, told to write its findings down, is the argument for pinning them. It overran badly
— ~17,700 tokens against a 10,000 limit — then excluded every assistant turn it had, which took
their calls down and the results with them:

```text
─── request 22: 48 msgs, ~11962 tokens,  0 items left out
─── request 23:  8 msgs,  ~1971 tokens, 42 items left out, 21 repairs
```

83% of its own context, gone in one move. What survived: the pinned brief, the question, the turn
it was speaking in — and the four notes it had pinned, which is the only reason the answer it then
gave was still right.

None of that is the runtime being clever. The runtime's contribution is that the context is a list
of ordinary values with identities, states and sizes, and that a tool is allowed to call the same
functions a user interface calls. What decides which of it a *model* may do is the tool: a pinned
item, a system instruction, and the assistant turn the call is speaking in are refused, and the
refusal is handed back to the model. The agent is not the boss.

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

### 🔓 what it does and does not protect you from

**The kernel executes nothing.** No filesystem code, no network code, no process spawning; every
side effect in a session happens inside a `Tool` you wrote and registered. So there is nothing here
to contain, and there will be no sandbox in this crate - containment belongs where the process is
actually spawned, which is your tool or the program around it. What the runtime enforces is one
thing: a call the `PermissionPolicy` refused is never handed to `Tool::invoke`, and the refusal is
recorded as an event and as a tool result the model is told about. That is a decision point with a
paper trail.

The refusal says what kind it was, because that is the only question a refused model can act on:
a standing rule means the same call will meet the same answer, and an answer to *this* call means
a different approach may well be allowed. Which of the two it was is the kernel's own knowledge —
it resolved the grant. *Why* is not, so the kernel asks: `PermissionPolicy::why` is defaulted to
`None`, and whatever a policy returns goes into the tool result beside the kernel's account of it.
A reason is built of a policy's own vocabulary — which capability, which path rule, which of
several subjects actually did it — and a kernel that invented one would be guessing at somebody
else's decision.

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

Third-party tools get their own line. `nachalnik-mcp` speaks to servers somebody else wrote, and
the specification says a client should never make tool-use decisions on hints from a server it does
not trust - so `Trust` believes none of them by default, and the bridge's tests include a server
offering a `delete_everything` that claims to be read-only.

#### the sandbox is `kamchatka`'s, because that is where the process is

`kamchatka` is the program that actually spawns things, so it is the one that can confine them, and
it does - with [Landlock](https://landlock.io), a Linux LSM a process applies to *itself*. No
privileges, no setuid helper, no container, no daemon. The `shell` tool re-executes `kamchatka` in
a mode that restricts itself and then *becomes* the command - the domain is inherited across the
`exec`, so nothing is given up by leaving, and the process the tool holds is the command itself
rather than a helper standing in front of it. So:

```text
network: deny  ->  connect() is refused by the kernel
write:   deny  ->  the working directory is read-only
               ->  nothing outside the working directory is readable or writable at all
```

Which is the difference that matters. A policy that refuses a command because it contains the word
`curl` is a heuristic somebody walks around: asked for a page with the network refused, a live model
had its `curl` refused and reached the same page with `python3 -c "import urllib.request"` on the
very next call. Under Landlock it tried `curl`, then a raw `socket.connect`, and got
`Permission denied` from the kernel both times. The command-reading check is still there, because
refusing up front with a reason is kinder than letting something run and fail - but it is no longer
the thing standing between the model and the network.

Within that boundary the file tools are finer than a capability. Everything starts at `ask`, and
`Careful` also holds a short list of patterns - `.env*`, `*.pem`, `id_rsa*`, `.ssh/` - where the
strictest of everything consulted wins. Answer `always` to an ordinary read and the *capability*
goes to `allow` while those do not, so reading `src/main.rs` goes silent from then on and reading
`.env` is still a question that names the rule that raised it. They bind `read`, `write` and `edit`, and deliberately
not `shell`: a command names its files inside a string, and `cat .env`, `sed -n 1p .env` and
`python -c "open('.env')"` are the same act written three ways. What binds a command is the kernel,
and what the kernel can express is a directory. So `cat .env` works where `read .env` asks, which is
the honest shape of it.

The three file tools cannot be confined that way - they run on the terminal's own threads, and a
ruleset applied there would confine the terminal - so they are held to the same boundary by their
own code, which resolves `..` and symlinks before comparing. That is weaker in kind, and the
permissions tab says which kind you have: `shell: confined`, or `shell: a command can do any of
these` where Landlock is unavailable, because a sandbox that quietly did nothing would be the worst
thing in this workspace.

`--sandbox-allow PATH` opens up more, `--no-sandbox` turns it off, and both are visible on that
tab. It is a demonstration rather than a hardened agent: it is one LSM, not a container, and a
temporary directory of its own is the only thing outside the working directory a command can write.

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

**[compare][ex-compare]** - the same prompt to several models at once, with proof that it *was*
the same prompt:

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

**[panel][ex-panel]** - several models arguing about one question, in rounds, ending in a ruling
with a tally behind it:

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

* **[transparency][ex-transparency]** - the whole philosophy in one run: what will be sent, a
  permission prompt, a tool that floods the context, and pruning it away. It also contains the
  permission policy and the `/context` renderer the library deliberately does not:
  `cargo run --example transparency`
* **[compaction][ex-compaction]** - a compactor that summarizes what it drops, and the user
  putting it back anyway: `cargo run --example compaction`

The three networked ones share [`examples/common`][ex-common] - an OpenAI-compatible HTTP
provider and nothing else, because a server-sent-event parser is not what any of them is about.
They talk to anything that speaks that dialect, local models included:

```console
$ NACHALNIK_API_KEY=ollama NACHALNIK_BASE_URL=http://localhost:11434/v1 \
    cargo run --example compare -- -m llama3.2 -m granite4.2:3b "why the borrow checker?"
```

---

### 🧪 tests

`cargo test -p nachalnik` runs 126 offline tests, covering the context model, the selectors, the
state machine, the loop, permissions, projection and tool-call repair, token counting and
calibration, compaction, and the session log.

Two of those are worth naming. The state machine is tested for refusing a second concurrent
`step`, and for a dropped one not wedging the kernel. The log is tested for reporting an item's
states in the order they were applied — which two threads changing one item is enough to break.
A replaced `Projector` gets a test of its own, because a seam nothing has ever been swapped
through is a claim rather than a seam.

`cargo test --workspace` runs 269 in all — those 126, plus:

* **25** in the two live suites below, which skip themselves when there is no key.
* **23** in the MCP bridge, which stand a real server up rather than mocking one.
* **95** in `kamchatka`: seventy draw its screen and read the characters back, thirteen drive it
  at every window size with every key, nine try to escape its sandbox and report what the kernel
  refused, two cover streaming, and one puts a socket in front of it that answers and then goes
  silent.

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

It skips itself when there is no key. It reads only `OPENROUTER_API_KEY` / `NACHALNIK_API_KEY`,
never a stray `OPENAI_API_KEY`; it defaults to a small free model; it costs about thirty requests
a run; it waits out momentary upstream rate limits; and it skips rather than fails when a
free-tier key has spent its daily allowance.

Twenty tests, in five groups:

* **On the wire.** A plain turn, the provider's own context limit, a system instruction, a
  labelled reference, streamed fragments adding up to the answer, an opaque parameter reaching
  the model, and the recorded payload being the one that actually went out.
* **Tools.** A call whose result the model reads back, a refused call the model is told about,
  and a truncated result.
* **A context somebody has edited.** A *pruned* tool exchange still producing a request the API
  accepts, an *elided* one still answering the call that asked for it, and compaction before a
  request.
* **Interruption.** A step abandoned mid-request leaving the kernel usable, a turn interrupted
  between requests, and an interrupt stopping a stream that is already arriving.
* **Across a session.** A paused-and-resumed permission decision, a mid-session model swap, a
  whole session round-tripping through `serde`, and the calibrating counter being told what a
  real request cost.

`kamchatka` has a live suite of its own - five tests, the same key, `KAMCHATKA_` rather than
`NACHALNIK_` - because the keys are what build those requests and drawing the screen can only say
what was drawn. It walks the context tab's state cycle against a real endpoint (all of an item,
then a marker where it was, then nothing, then all of it again), checks each step is a request
the API accepts, and asks the endpoint what models it serves.

---

### 📦 the workspace

| crate | what it is |
| --- | --- |
| **[`nachalnik`][nachalnik]** | the runtime. Five dependencies, no `unsafe`, no network, no prompt. This is the part that matters, and it is meant to stay boring. |
| **[`nachalnik-mcp`][nachalnik-mcp]** | a bridge to [MCP](https://modelcontextprotocol.io) servers, so that a tool somebody else wrote is a `Tool` like any other. |
| **[`kamchatka`][kamchatka]** | a terminal agent built on the runtime - the thing you actually run, and the demonstration that the seams hold up under one. |
| `nachalnik-utils` | never published, permanently `0.0.0`. The OpenAI-compatible provider this crate's own examples and live tests talk through, so that scaffolding is written once rather than three times. A *dev*-dependency, which is the whole trick: cargo strips those from a published manifest, so a crate only ever dev-depended on never has to exist on the registry. |

The bridge is deliberately *not* in the core. Speaking MCP means spawning processes, opening
sockets and reading notifications in the background, and the runtime promises to do none of those;
it would also tie a context library's version to a protocol that revises faster than the library
should.

It is also the answer to the obvious question about a crate this abstract - whether the seams are
real. Writing it needed **no change to the runtime at all**: an MCP tool is a `Tool` that forwards
to a server, tools arriving and leaving are `add_tool` and `remove_tool`, a structured result is
`Content::Json`. The introspection tools above are the second instance of the same test, and a
harder one: forking a context is `snapshot` and `resume`, previewing a request is
`preview_request`, pruning is `set_state`, and reading the budget is `budget` - all of it already
public, none of it added for the purpose. And it pushed back on one thing worth knowing about: MCP tool annotations are
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

Licensed under the MIT License ([LICENSE-MIT][license]).

<!-- This file is `nachalnik`'s readme as well as the workspace's, and crates.io resolves a
     relative link against the directory the readme was published from - `nachalnik/` - rather
     than against the repository root. Every link into the tree is therefore absolute. -->

[kamchatka]: https://github.com/ljedrz/nachalnik/tree/HEAD/kamchatka
[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[writeup]: https://ljedrz.github.io/nachalnik/
[nachalnik-mcp]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-mcp
[ex-compare]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/compare.rs
[ex-panel]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/panel.rs
[ex-transparency]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/transparency.rs
[ex-compaction]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/compaction.rs
[ex-common]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/common/mod.rs
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT
