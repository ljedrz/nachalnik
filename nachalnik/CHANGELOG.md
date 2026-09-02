# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### fixed

- `LinearProjector` keeps a tool result next to the call it answers. An item pushed into the
  context while a turn was still collecting its results - a note a tool writes on the model's
  behalf, mid-turn - was projected in the position it arrived in, between the assistant message
  and the results. Every OpenAI-compatible API refuses that request outright, naming the
  `tool_call_id` that went unanswered, and the whole session dies. It is provider-dependent, so it
  was silent: Google's API accepts the sequence and so does at least one OpenRouter upstream,
  which is why three published transcripts never showed it. Five of seven live runs died on it,
  each immediately after the model had written down ten correct findings. Whatever arrives
  mid-turn is now held until the turn has been answered and listed in `Projection::repairs`.

## [0.2.1] - 2026-09-01

### added

- `PermissionPolicy::why`, defaulted to `None`: the kernel asks a policy that is about to refuse
  a call whether it has anything to say, and puts the answer into the tool result the model reads.
  The reason is emphatically not the kernel's - it is made of a policy's own vocabulary, which
  capability or which path rule actually did it, and a kernel that invented one would be guessing
  at somebody else's decision. A downstream policy that knew exactly why had no way to say so, and
  a policy needing a core change to do an ordinary thing is a seam that is not finished.

### changed

- The truncation marker no longer names this crate. `[... 943 bytes truncated by nachalnik ...]`
  was addressed to a reader who has never heard of it; what the model can use is that something
  was cut, how much is missing, and that a limit rather than the tool did it - all of which say
  "ask for less next time". It is `[... 943 bytes truncated by an output limit ...]`.
- A refused call is told which *kind* of refusal it was. `the call was not permitted` is true and
  leaves open the only question a refused model can act on: a standing rule means the same call
  will meet the same answer, and an answer to *this* call means a different approach may well be
  allowed. Which of the two happened is the kernel's own knowledge, since it resolved the grant,
  so it says so - and a model that cannot tell them apart rephrases at a rule that will never
  move, or abandons an approach that was refused once.

## [0.2.0] - 2026-08-30

An assistant turn can be the ordered sequence the model produced it in, rather than a content
slot, a reasoning slot and a flat list of calls.

**Breaking:** `LinearProjector` has a new public field, so a struct literal that names every field
no longer compiles; add `..Default::default()`. `Content` and `Block` are `#[non_exhaustive]`, so
the new variants are additive.

### added

- `Content::Blocks`, and the `Block` enum it holds: an assistant turn as the *ordered* sequence
  the model produced it in - thinking, a sentence, a tool call, another sentence after it. Some
  APIs make that order part of the message, and a turn with one content slot, one reasoning slot
  and a flat list of calls cannot express it however cleverly it is projected. It is a variant of
  `Content` rather than a field on `Message` because content is the one thing a `ModelResponse`, a
  `ContextItem` and a `Message` all carry, so the order survives the whole way from the wire, into
  the context where it can be counted and pruned, and back out again; a field on `Message` would
  have been a shape the context could not hold, and a projector cannot recover an order that was
  never recorded.
- `Message::calls`, `ModelResponse::calls` and `ContextItem::calls`: the tool calls a turn asked
  for, wherever they are recorded. A turn is recorded *either* the conventional way *or* as
  blocks, never both, so nothing can disagree - and these are what the kernel, the projector and a
  provider should read. A provider reading the `tool_calls` field directly would send the words of
  an ordered turn with none of the calls in it, which most APIs reject and which is very hard to
  see afterwards.
- `LinearProjector::send_blocks`, off by default: whether an assistant turn is projected as
  blocks or flattened into the three slots this projector's dialect has. On, every assistant turn
  goes out as blocks - a conventional one assembled into the conventional order - so a context
  holding some of each projects to one shape rather than two. Off, a turn recorded as blocks is
  flattened, and where that loses something (two thinking blocks joined into one, a sentence that
  came after a call arriving before it, a signature that has nowhere to go) it is reported in
  `Projection::repairs` instead of being done quietly. That is the honest version of the reassembly a provider used to have to do for
  itself, and for a signed thinking block it is not good enough, which is why the flag exists.
- `Part`, which is what the two block variants that are not calls hold: a `Content` and an
  `extra`. It is `ToolCall::extra` for the rest of a turn, and it exists because some APIs sign
  each piece of one rather than the whole - Gemini's `thoughtSignature` rides on a text part as
  readily as on a call, and `generateContent` puts it on the text part of a turn that called
  nothing. Bound to the block rather than kept beside it, so that whatever removes the block
  removes the signature of the thing that is no longer there; eliding a turn is the case that
  makes it matter, since the marker replacing the words must not go out signed as if it were them.
- `ContextItem::thinking`, the counterpart of `ContextItem::calls`: the model's thinking wherever
  it is recorded, in order. An ordered turn keeps it in the content, where `ContextItem::reasoning`
  cannot see it, so a client that only knew about the conventional slot would show a reasoning
  model as having done no reasoning at all.
- `Block::name` / `call` / `said` / `thought` / `part` / `extra` / `byte_len`, the `Block::text`
  and `Block::reasoning` constructors, `Content::blocks` / `as_blocks`, `Message::blocks`,
  `ModelResponse::blocks`, and `ToolCall::byte_len` - the last one says once what a call costs,
  which `TokenCounter::count_item` has always added on top of the content.

### changed

- `Content::to_text` on blocks is what the turn *said* - the text blocks, joined with a newline -
  and not what it costs: thinking is not something the model uttered, and a provider putting it in
  a `content` field would be sending the model its own reasoning back as if it had. `byte_len`
  counts all of it, calls included, and `truncate_to` measures against that, so a turn whose words
  fit but whose calls do not is over the limit and the number it reports is everything that went.
- `Kernel::repair_call_ids` repairs a call wherever it lives, rewriting the sequence when one is
  inside a turn's blocks - and only when something actually needed repairing.
- The OpenAI-compatible providers in `nachalnik-utils` and `kamchatka` render a message's calls
  through `Message::calls`, so an ordered turn projected at them still goes out with its calls.

## [0.1.0] - 2026-08-29

The first release: an agent loop as a state machine, with the context, the tools, the permissions
and the requests as explicit state.

### added

- `Kernel`, the loop: `step` performs exactly one transition and returns the `State` it produced,
  `turn` repeats until the model ends its turn or somebody has to decide something. `Requesting`
  and `Executing` are refused rather than duplicated, and a dropped step returns the kernel to
  `Idle` rather than wedging it.
- `Context`, a list of identified items. Removal is a state change, so a removed item can still be
  listed, inspected and restored - and that holds for an output limit too, which archives the
  whole of what a tool said beside the shortened copy the model is shown.
- `undo` / `redo` / `supersede` / `replace` / `annotate` / `push_all`, each one operation.
- `ContextState::Elided`, the third answer between in and out: the item stays in the request as a
  short marker - its own note, in brackets - instead of its content. It is what a `Compactor`
  should reach for, through `CompactionPlan::elide`. Excluding a tool result forces the projector
  to drop the call that asked for it, since a call with no result is a request most providers
  reject, so the model ends up reading a history in which it never asked for anything, directly
  under a summary saying the results were dropped; an elided result still answers its call, and
  those two accounts stop disagreeing. `ContextState::sends_content` is the predicate the token
  figures are built on, and an elided item's own size is `tokens_withheld` rather than spent.
- `TokenCounter::count_message`, defaulted, and with it a budget counted over the messages the
  projector produced rather than over the items that went in. The two are not the same figure and
  never were: a reference is labelled on its way out, so `src/parser.rs:\n` was going on the wire
  without appearing on the bill. It also means the budget answers with what the counter knows now
  rather than what it knew when each item was pushed, so a `Calibrating` correction shows up
  immediately instead of at the next `recount`.
- Six seams, all replaceable at runtime: `Provider`, `Tool`, `PermissionPolicy`, `Projector`,
  `TokenCounter`, `Compactor`. Each of the four that had no other way to identify itself carries a
  `name()` whose default is the implementing type's own path, so `Kernel::policy`, `projector`,
  `counter` and `compactor` hand back something a client can actually show somebody.
- `preview_request` and `preview_payload`: the exact request, and the provider's own bytes for it,
  before anything is sent.
- `Event`, an append-only session log of typed events covering every transition, broadcast live.
  Replacing any of the seams is an event too - `policy.changed`, `projector.changed`,
  `counter.changed`, `compactor.changed`, each carrying `from` and `to` as the seams' own `name()`
  - so a log can answer "what was projecting these requests?" for a session where somebody changed
  it half way. A compactor removed is `to: None`, "nothing will ever be dropped from now on" being
  the change that matters most and the one least visible. Context events are recorded while the
  context lock is still held, so the log's account of an item's states is in the order they were
  applied rather than the order two threads happened to announce them.
- `Snapshot` / `resume`, because a log of events that name their items cannot rebuild the items.
  A snapshot carries `calibration`, so a resumed session does not spend its first requests
  relearning what it had already been told.
- `interrupt`, which stops the loop between transitions, and - through
  `DeltaSink::is_interrupted` and `OutputSink::is_interrupted` - lets a provider or a tool stop
  what is already in flight without losing what it had. Plus `Config::max_requests_per_turn` and
  `cancel_pending_calls`.
- `Calibrating`, a token counter that corrects another against what providers actually charge,
  via `TokenCounter::observe` - the kernel reports what a request was estimated at and what it
  cost, and the counter decides what to make of it. It is what `Kernel::new` starts with, wrapped
  around `BytesPerToken`, correcting by `1.0` until a provider has said something; the bare
  estimate is `set_counter(Arc::new(BytesPerToken::default()))` for anybody who wants it back.
  `TokenCounter::calibration` and `recalibrate` are how one counter hands that over to another.
- `Kernel::with_context` and `with_history`: a question about the context or the log - a count, a
  search - answered without copying either. `context()` and `history()` say on themselves that
  they copy the whole thing.
- `Config::parallel_tool_calls`, off by default: the one place the kernel spawns tasks.
- Features: `selectors` (a small language for naming context items) and `test` (a scripted
  provider, dummy tools, off-the-shelf policies and a mechanical compactor).
- Five dependencies, no `unsafe`, no system prompt, no default tools, no HTTP client. The
  OpenAI-compatible provider the examples and the live suite talk through lives in
  `nachalnik-utils`, an unpublished `0.0.0` workspace member that is a dev-dependency and nothing
  else - so none of it reaches anybody who depends on this crate.
- The crate documentation says what it does *not* protect you from: there is no sandbox and the
  kernel executes nothing, so what it enforces is that a refused call never reaches `Tool::invoke`
  - a decision point with a paper trail rather than a boundary. `Capability`'s own documentation
  says that `Shell` subsumes every other capability, so a policy that allows it has allowed all of
  them, and that what closes the gap is `PermissionRequest::args`, which a policy is handed and a
  capability list cannot see.
