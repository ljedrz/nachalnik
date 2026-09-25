# the parts that are not obvious

Six things this runtime does differently from a loop somebody would write in an afternoon,
each with the reasoning that put it there. [The readme](README.md) says what the crate is and
how to start; this is what to read before building something that leans on it.

---

## 🔍 context as a data structure

Items are public data — an id, a kind, a source, a label, content, a size, a state, a note and
whatever metadata you attach — so a client can render `/context` however it likes. One method
covers every state change, and each call is one undoable operation:

```rust
kernel.set_state(ids, ContextState::Excluded, Some("an enormous test output".into()));
kernel.set_state(ids, ContextState::Pinned, None);
kernel.replace(id, "a shorter version")?;                 // new contents, same identifier
kernel.supersede(old, ContextItem::file(path, reread))?;  // this one replaces that one
kernel.annotate(id, json!({ "expendable": true }))?;      // a hint for your compactor
kernel.push_all(files);                                   // one operation, so one undo
kernel.undo()?;                                           // refused while a turn holds calls
kernel.redo()?;
```

`set_state` says what it did to each identifier — `changed`, `unchanged`, `unknown` — because
"there is no item 12" and "item 12 was already pruned" are different things to tell somebody.

`annotate` is the exception, and takes no undo of its own: metadata rides with the operation it
describes, so an `undo` takes an annotation back with the operation before it. It is new work all
the same, so it leaves nothing for a `redo` to put back.

Excluding an item removes it from the *projection*, not from the record. It keeps its identifier,
stays listed and inspectable, and comes back with another `set_state`, an `undo`, or a `redo`.
`Elided` is the third answer between in and out: the item stays in the request as a one-line
marker, so a tool result can stop costing what it holds without the call that asked for it having
to come down too. The default projector drops the other half of a call/result pair when one side
is gone, so pruning cannot produce a request the provider will reject. It says so in
`Projection::repairs` *and* in the session log, because a request the kernel quietly adjusted is
exactly the one you want to be able to ask about afterwards.

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
asks for run one at a time in the order it asked — which is something you can build on, since two
edits to the same file then apply in sequence. `Config::parallel_tool_calls` gives that up for
speed, on purpose and never by default: nothing in the kernel can tell whether a model's calls are
independent, so the judgement is yours. Either way the results are recorded in the order the model
asked for them. Several threads on a *single* kernel are fine too — reading is cheap and every
mutation is atomic, so a client can render, prune and preview while a turn is in flight. The one
thing two threads cannot do is drive the loop at the same time, which is `Error::Busy` rather than
a second request.

Automatic management is allowed, invisible management is not. A `Compactor` gets the budget and
the items and returns a plan; the kernel refuses to remove anything pinned, applies the rest,
and broadcasts a report of exactly what it did — which the user can then disagree with.

---

## 🧵 a turn keeps the order it was produced in

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

## 🎯 a budget that corrects itself, and admits what it cannot reach

Every token figure the kernel reports comes from a `TokenCounter`, and the estimate underneath the
default one — `bytes / 4` — is admittedly that. How wrong it is depends on the shape of what you
are sending: measured against a real API, about a third low on a short chat carrying four tool
definitions, and a steady 7% low once the conversation is a few thousand tokens. It cannot see
per-message framing and never sees the tokens a reasoning model spends thinking. Embedding a
tokenizer would mean embedding a model-specific assumption, which this crate will not do.

So it does the other thing. After every response, the provider has said what the request actually
cost, and the kernel knows what it estimated for the very same bytes — so it hands both numbers to
the counter, and a `Kernel::new` is already holding one that acts on them:

```rust
// what a kernel starts with; correcting by 1.0 until a provider has said otherwise
Calibrating::new(BytesPerToken::default())

// and the bare estimate, for a measurement that wants a counter which never changes its mind
kernel.set_counter(Arc::new(BytesPerToken::default()));
```

`Calibrating` converges on the first response worth learning from and settles there — measured over
a growing conversation, it took that steady 7% error to within 1%. What it learned is a number you
can look at (`calibration()`), not a fudge factor buried in the kernel. It ignores requests too
small to have a systematic error in them, because a percentage drawn from a handful of tokens is
noise. And it corrects what is counted *from then on*: figures already recorded on items do not
silently rewrite themselves. `Kernel::recount` rewrites them when you ask, and says so on the
event stream.

The hook is `TokenCounter::observe`, whose default does nothing. As everywhere else, the kernel
supplies the facts and your code supplies the judgement.

And where a counter cannot reach something at all, it says so instead of returning `0`. That is a
different problem from being a few percent out, and no amount of calibration touches it:
`Content::Blob` holds base64, and base64 over four is a number about an encoding rather than about
a model — a 400 KB screenshot would arrive as a hundred thousand tokens and send a compactor after
a context that is nowhere near full. What a picture really costs is a formula over its
*dimensions*, every vendor publishes one, and each publishes a different one, so this crate
carries none of them.

What it carries is the two halves that let you supply one. `TokenCounter::uncounted` is a *count*
of the pieces a counter declined to price, and it rides up to `Budget::uncounted` and
`ContextItem::uncounted` — so `budget.fully_counted()` is the difference between a figure that is
complete and a figure that is a floor, and the row holding the picture can be marked as the one
nobody priced. `Blob::meta` is a free-form value the kernel never reads, for whatever that counter
would need: `{"w": 1024, "h": 768}` for a picture, `{"pages": 12}` for a document. Whoever encoded
the payload had it decoded a moment earlier, so they are the one who knows.

```console
$ cargo run --example pricing_a_picture
```

That counts one context three ways — the default counter, one applying a vendor's tiling formula
from `meta`, and that same formula handed a blob nobody measured. Knowing a formula does not help
if the payload has no dimensions on it, so the third abstains exactly as the default one does.

One rule follows: a request carrying anything unpriced never reaches
`observe`. `Calibrating` corrects with a single multiplier, so a gap it cannot see would be spread
over the bytes it can — prose beside one screenshot ends up reading 50% high while the screenshot
still reads nothing.

---

## 🛑 stopping

`interrupt()` can be called from any thread, and stops the loop in three places, each needing a
little more cooperation than the last:

| where | what happens | who has to agree |
| --- | --- | --- |
| between transitions | the next `step` or `turn` spends one attempt acknowledging it and does nothing else | nobody |
| during a request | a provider that checks `DeltaSink::is_interrupted` stops reading and hands back what it has | the provider |
| during tool calls | the kernel does not start the serial calls that had not begun; a tool that checks `OutputSink::is_interrupted` can stop the one that had | the tool |

The kernel cannot reach into a `Provider` and stop it — it does not own the socket, the runtime or
the future — so it offers the fact and lets the provider decide. One that ignores it is not broken,
only slower to stop.

What stopping never does is discard work. A half-finished answer and a tool that returned early are
recorded as ordinary items, because the point of a context you can see is that *you* decide what to
do with them. The blunt instrument is still there — drop the future driving `step` and the request
is abandoned mid-flight and the kernel returns to `Idle` rather than wedging — but it costs you
whatever had been streamed.

---

## 📡 everything is an event

```text
session.started    state.changed       model.changed      tool.requested
session.resumed    context.added       model.params       tool.unknown
session.finished   context.changed     model.requested    tool.repaired
turn.interrupted   context.replaced    model.delta        tool.reserved
tools.changed      context.undone      model.payload      tool.started
policy.changed     context.redone      model.finished     tool.output
projector.changed  context.annotated   model.failed       tool.finished
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
`context.replaced`, and it follows the rule that makes the rest work — the log records what nothing
else can recover. An added item is still in the context; overwritten text is nowhere. The log is
unbounded on purpose (a capped append-only log is not one), and `drain_history` is how a
long-running session stays affordable: you take the records, you write them somewhere, the kernel
lets go. Nothing disappears behind your back.

---

## 💾 sessions outlive processes

The log and a snapshot answer different questions, and you want both. The log says what happened
and stays small, because an event *names* an item rather than carrying its contents — which is
also why it cannot rebuild a context. A `Snapshot` can:

```rust
let snapshot = kernel.snapshot();          // items, ids, states, notes, params, used call ids
std::fs::write("session.json", serde_json::to_vec(&snapshot)?)?;

// ... a process later
let kernel = Kernel::resume(Config::default(), snapshot);
```

Everything that is easy to lose comes back: a pin, the reason something was pruned, a turn's
reasoning, the signature attached to a tool call, and the identifiers already handed out — so a
resumed session cannot reuse one. A provider, a policy and the tools are yours to supply again,
because they were never the session's to remember. Setting `Config::session_name` resumes under a
new name, which is how a session gets forked rather than continued.
