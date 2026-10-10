# the parts that are not obvious

What this runtime does differently from a simple agent loop, and why. [The README](README.md) says
what the crate is and how to start; read this before building something that depends on it.

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

`set_state` reports what it did to each identifier (`changed`, `unchanged`, `unknown`), because
"there is no item 12" and "item 12 was already pruned" are different answers.

`annotate` is the exception: it doesn't create its own undo step. Metadata belongs to the operation
it describes, so `undo` removes an annotation together with the operation before it. It still
counts as new work, though, so it clears anything `redo` could have restored.

Excluding an item removes it from the *projection* (the request), not from the record. It keeps its
identifier, stays listed and inspectable, and comes back with another `set_state`, an `undo`, or a
`redo`. `Elided` is in between: the item stays in the request as a one-line placeholder, so a large
tool result stops costing tokens without its tool call having to be removed too. The default
projector removes the other half of a call/result pair when one half is gone, so pruning can't
produce a request the provider will reject. It reports this in `Projection::repairs` *and* in the
session log, because any request the kernel adjusted is one you'll want to be able to look into
afterwards.

**Nothing is destroyed, not even by a limit.** A tool output over its limit is recorded twice: the
full output, excluded, and the truncated copy the model sees. Showing the model the full output is
a `set_state` like any other, not a re-run of the tool. Keeping it is cheap: content, tool-call
arguments and tool schemas are all shared, so pruning a four-megabyte tool result, putting it into
a request, and recording it in an event all just pass a pointer to the same bytes. Set
`Config::keep_truncated_output` to `false` when a tool can produce more than you are willing to go
on holding; the truncation is still reported either way.

One agent is one kernel, and kernels share nothing unless you give them something in common, so
running sixteen at once needs no coordination. Within a single turn, the tools a model
asks for run one at a time in the order it asked — which is something you can build on, since two
edits to the same file then apply in sequence. `Config::parallel_tool_calls` gives that up for
speed, on purpose and never by default: nothing in the kernel can tell whether a model's calls are
independent, so the judgement is yours. Either way the results are recorded in the order the model
asked for them. Several threads on a *single* kernel are fine too — reading is cheap and every
mutation is atomic, so a client can render, prune and preview while a turn is in flight. The one
thing two threads cannot do is drive the loop at the same time, which is `Error::Busy` rather than
a second request.

Automatic management is allowed; invisible management is not. A `Compactor` gets the budget and the
items and returns a plan; the kernel refuses to remove anything pinned, applies the rest, and
broadcasts a report of exactly what it did, which the user can then undo.

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
can hold two accounts of it; `calls()` and `thinking()` on an item or a response read whichever is
in use. `LinearProjector::send_blocks` decides which shape goes out, and flattening reports what it
cost in `Projection::repairs` rather than doing it quietly.

---

## 🎯 a budget that corrects itself, and admits what it cannot reach

Every token figure the kernel reports comes from a `TokenCounter`. The default one estimates text
as `bytes / 4`, which is only an estimate, and how far off it is depends on what you send: short
requests with many tool definitions come out well below the real count, long conversations a few
percent below. It can't see per-message overhead or the tokens a reasoning model spends thinking.
Embedding a tokenizer would tie the crate to particular models, which it won't do.

Instead, it learns from the provider. A provider that reports usage says, after each response,
what the request actually cost, and the kernel knows what it estimated for the same request, so it
gives both numbers to the counter. A new `Kernel` already has a counter that uses them:

```rust
// what a kernel starts with; correcting by 1.0 until a provider has said otherwise
Calibrating::new(BytesPerToken::default())

// and the bare estimate, for a measurement that wants a counter which never changes its mind
kernel.set_counter(Arc::new(BytesPerToken::default()));
```

`Calibrating` keeps one ratio across all the responses it learns from, so it settles instead of
jumping with each request. The ratio is a number you can read (`calibration()`), not a hidden
fudge factor. It ignores requests too small to show a systematic error, because a percentage from a
handful of tokens is noise. And it corrects what is counted *from
then on*: figures already recorded on items do not silently rewrite themselves. `Kernel::recount`
rewrites them when you ask, and says so on the event stream.

The hook is `TokenCounter::observe`, which does nothing by default. As everywhere else, the kernel
provides the facts and your code decides what to do with them.

When a counter can't price something at all, it says so instead of returning `0`. That's a different
problem from being a few percent off, and calibration can't fix it: `Content::Blob` holds base64,
and base64 length divided by four says nothing about tokens: divided that way, a 400 KB screenshot
would count as a hundred thousand tokens and trigger compaction of a context that is nowhere near
full. So the default counter leaves every blob out of its figure and counts it as unpriced instead.
An image's real cost is a formula based on its *dimensions*, and every vendor publishes a different
one, so this crate includes none of them.

Instead, it gives you two things to supply your own. `TokenCounter::uncounted` is a *count*
of the pieces a counter declined to price, and it rides up to `Budget::uncounted` and
`ContextItem::uncounted`, so `budget.fully_counted()` tells a complete figure from a minimum, and
the item holding the image can be marked as unpriced. `Blob::meta` is a free-form value the kernel
never reads, for whatever your counter needs: `{"w": 1024, "h": 768}` for an image,
`{"pages": 12}` for a document. Whoever encoded the payload had it decoded just before, so they
know these values.

```console
$ cargo run --example pricing_a_picture
```

That counts one context three ways: with the default counter, with one that applies a vendor's
formula using `meta`, and with that same formula given a blob without dimensions. A formula is no
use without the dimensions, so the third reports the image as unpriced, like the default counter.

One rule follows: a request containing anything unpriced is never passed to `observe`.
`Calibrating` uses a single multiplier, so it would spread the unpriced image's cost over the text
it can see, making the text look too expensive while the image still counted as nothing.

---

## 🛑 stopping

`interrupt()` can be called from any thread, and stops the loop in three places, each needing a
little more cooperation than the last:

| where | what happens | who has to agree |
| --- | --- | --- |
| between transitions | the next `step` or `turn` spends one attempt acknowledging it and does nothing else | nobody |
| during a request | a provider that checks `DeltaSink::is_interrupted` stops reading and hands back what it has | the provider |
| during tool calls | the kernel does not start the serial calls that had not begun; a tool that checks `OutputSink::is_interrupted` can stop the one that had | the tool |

The kernel can't stop a `Provider` itself, since it doesn't own the socket, the runtime or the
future, so it signals the interrupt and lets the provider act on it. A provider that ignores it
isn't broken, just slower to stop.

Stopping never throws work away. A half-finished answer and a tool that returned early are recorded
as ordinary items, so *you* decide what to do with them. There is also a blunter option: drop the
future driving `step`, and the request is abandoned and the kernel returns to `Idle` instead of
getting stuck, but you lose whatever had been streamed.

---

## 📡 everything is an event

```text
session.started    state.changed       model.changed      tool.requested
session.resumed    context.added       model.params       tool.unknown
session.finished   context.changed     model.requested    tool.repaired
turn.interrupted   context.replaced    model.delta        tool.reserved
turn.paused        context.undone      model.payload      tool.started
turn.unfinished    context.redone      model.finished     tool.output
tools.changed      context.annotated   model.failed       tool.finished
policy.changed     context.recounted   step.failed        permission.requested
projector.changed  context.compacted                      tool.panicked
counter.changed    context.full                           permission.decided
compactor.changed                                         policy.ruled
```

Every event carries what a client needs to display it without guessing. An undo names the items it
removed and the ones it reverted. A request names the items it left out, and why. Swapping a
component names both the old one and the new one, because "the projector was replaced" doesn't
tell you which projector built the requests before and after, which is exactly what a log is for.

`Kernel::subscribe` is the live stream; `Kernel::history` is the append-only session log (both
written under one lock, so their order agrees), which keeps `model.delta` and `tool.output` only
when `Config::record_progress` is on. Records are plain `serde` types, so persisting a session is
one line per event. The record a session begins with carries `FORMAT`, and so does a snapshot. An
event a reader's version does not know reads as `Event::Unknown` rather than failing the log. And
`tests/records/` holds one of every event and a snapshot, per format, as the shapes to read against.

The log stays small by *referring* to things rather than copying them: `model.requested` records
the context ids a request was built from, not the messages. The only event that carries content is
`context.replaced`, because the log records what can't be recovered any other way: an added item is
still in the context, but overwritten text is gone. Item metadata is copied too, into
`context.added` and `context.annotated`, because compactors make decisions based on it and nothing
else keeps the value an annotation replaced. `model.payload` holds the rendered request, and is only
written when `Config::record_payloads` is on. The log is deliberately unbounded (a capped log isn't
append-only), and `drain_history` keeps long sessions manageable: you take the records, write them
somewhere, and the kernel releases them. Nothing disappears without you doing it.

---

## 💾 sessions outlive processes

The log and a snapshot serve different purposes, and you want both. The log says what happened and
stays small, because events refer to items instead of carrying their contents, which is also why
it can't rebuild a context. A `Snapshot` can:

```rust
let snapshot = kernel.snapshot(); // items, ids, states, notes, params, used call ids, calibration
std::fs::write("session.json", serde_json::to_vec(&snapshot)?)?;

// ... a process later
let kernel = Kernel::resume(Config::default(), snapshot);
```

Everything that is easy to lose comes back: pins, the reasons items were pruned, a turn's reasoning,
the signature attached to a tool call, what the token counter had learned about the real cost of a
request, and the identifiers already used, so a resumed session can't reuse one. You supply the
provider, policy and tools again, because they aren't part of the session. Setting
`Config::session_name` resumes under a new name, which is how a session gets forked rather than
continued.
