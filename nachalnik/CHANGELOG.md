# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### breaking

- **`Event::ContextAdded` carries `meta`, and `Event::ContextAnnotated` carries `was`.** The
  metadata an item was added with, and whatever an annotation replaced, were in no record - only in
  a snapshot taken before the change - and they are the hints a `Compactor` decides by. Both fields
  are `serde(default)`, so an older log reads as metadata that was null; a pattern that names the
  variants' fields needs the new one or `..`.

- **`Kernel::undo` and `Kernel::redo` return `Result<bool>`, and refuse while a turn holds
  calls.** From `Requesting` until the machine is resting with nothing to run - through
  `Deciding`, `Ready` and `Executing` - they answer `Error::Busy`. Undoing the turn that asked for
  a call did not stop the call: it ran, its result was recorded against a turn no longer there and
  never reached the model, and recording it took a checkpoint that made the undone turn
  unreachable by `redo`. A caller that wrote `kernel.undo()` as a statement handles the `Result`;
  `Ok(false)` is what `false` was.

- **`Snapshot` and `SelectorError` are `#[non_exhaustive]`.** Nothing outside the crate builds
  either - `Kernel::snapshot` makes the one and `Selector::parse` the other - which is the
  convention's test, and it makes the next field on a snapshot a patch. A struct literal of either
  written elsewhere no longer compiles; a `Snapshot` read back with `serde`, and a
  `SelectorError`'s `.0`, are unchanged.

### added

- **`Snapshot::problems`** says what is wrong with a snapshot, in words: an identifier two items
  share or none, a call the items name that `used_calls` does not list, and a number too near the
  top of a `u64` to count on from. It is what `Kernel::resume` would have to repair or could not,
  for a caller that would rather refuse a snapshot read back from a file than resume a repaired
  one.

### changed

- **`cancel_pending_calls` passes through `State::Executing`.** It refuses the calls and claims
  the machine without letting go of the lock, records the results, and only then goes `Idle` - the
  shape `decide` refusing each call and a `step` running them would give. It went straight to
  `Idle` and recorded afterwards, so a `step` in between could build a request carrying calls with
  no results, and the log said `idle` before it said the calls were refused. A client drawing
  states sees `executing` for the length of the recording.

- **A policy is asked `why` only about a refusal its own verdict made.** The kernel called it for
  every refused call and then used the answer for policy-sourced refusals alone, so a policy that
  answered `Ask` and had its question refused by a person was asked to explain a decision it did
  not make, and the explanation was computed and dropped. The wording the model reads is
  unchanged - a call somebody refused still says it was an answer to that call rather than a
  standing rule - and `PermissionPolicy::why` now says which refusals it is asked about.

- Three doc notes that said what the code does not. `Blob::wire_len` said `meta` never reaches a
  provider, where the field's own note says one reads `name` out of it and
  `nachalnik-providers` does. `Session::last_seq` said `0` when there is no record, where a
  drained log has none and it answers the last number handed out - which is what makes it a
  cursor. And `Capability::of` now says that neither half may hold a colon and that nothing
  enforces it: a capability built with one serializes to text `Capability::parse` refuses, so the
  log record carrying it cannot be read back.

- The docs say the policy is asked about what a call needs (`Tool::needs`) rather than everything
  the tool declared; that `Budget::context_tokens` is counted over the projected messages; that a
  step can also end in `Idle` or spend itself on an interrupt; and that `tool_result:N` is the item
  numbered `N`, whatever its kind.

- The docs say that `ContextChanged` also reports a new note on an unchanged state, so `from` and
  `to` can be equal; that the session log leaves out tool output as well as deltas unless
  `Config::record_progress` is on; that an `Overrun` can come from the kernel's own estimate as
  well as from a refusal; and that `Snapshot::used_calls` is what a resumed session checks a
  provider's identifiers against.

### fixed

- **A resumed snapshot is checked rather than trusted.** `resume` reserved only the calls
  `used_calls` listed, so a snapshot that left one out - merged, or written by hand - let a
  provider hand it back unrepaired, and a request carried one `tool_call_id` twice; it reserves
  every call the items name now, and `Kernel::snapshot` lists them all, since a client can push a
  turn whose calls it never reserved. Two items sharing an identifier, or one with `0`, resumed as
  they were, and a lookup found one of the two at random; each is given the next free identifier.
  And a number at the top of a `u64` - an item's, `next_item`, `last_seq` - panicked in a debug
  build and wrapped round in a release one; the counting saturates, and `Snapshot::problems` names
  the snapshot that would need it.

- **What the counter learned comes back exactly.** The scale was read back from JSON as written,
  and about one ratio in six does not survive that trip, so a resumed session counted on a scale a
  digit off the one it was saved with. It is worked out again from the two totals, as `observe`
  works it out.

- **A batch of tool results is one undo whatever lands between two of them.** Each result joined
  the checkpoint the first took, assuming nothing else took one in between - and a tool that
  changes the context while it runs, or a client that does, did. One undo then took back that
  change and the results after it, and kept the ones before: a turn half answered, repaired out of
  the next request. A checkpoint taken while a batch runs is folded into the batch's own now, for
  the calls of a turn, their cancellation, and the answer to a call nobody can run.

- **A resumed session numbers its records and its permission questions on from the one it
  carries on from.** Both started again at 1, so a client keeping a `history_since` cursor across
  `Kernel::resume` read nothing until the new log passed the old number, and a log kept across the
  resume had two records under each number and two questions under one `PermissionId`.
  `Snapshot::last_seq` and `Snapshot::next_permission` carry them, and `session.resumed` is the
  record after the last one. `last_seq` is read under the lock the items are, so the records up to
  it are exactly those whose changes the items show, which is what lets a log written beside a
  snapshot agree with it. Both are `serde(default)`, so a snapshot written before them resumes
  and numbers from 1, as it did.

- **`interrupt` sets and announces the flag under the machine lock**, where every step reads and
  spends it. Outside it, a step could spend an interrupt between its setting and its announcement,
  and the log placed `turn.interrupted` after the step that had acted on it.

- **`snapshot` reads the context before the used call identifiers.** A turn reserves an
  identifier before it records the call, and none is given back, so identifiers read afterwards
  cover every call read before. The other order let a turn recorded between the two reads leave a
  call whose identifier `used_calls` lacked, for a resumed session to hand out again.

- **`push_all` and `supersede` are one operation under one lock.** Both took the context lock
  once per change, so another thread's work could land between them: a turn recorded halfway
  through a `push_all` went into its checkpoint, and one `undo` took back the turn and the tail of
  the files and left their head; an `undo` between `supersede`'s check and its change answered `Ok`
  having superseded nothing.

- **The calibrating counter and `Usage::settled` saturate rather than overflow.** Both add figures
  that came from outside the process - a snapshot, a provider's report - which panicked in a debug
  build on a hostile one and wrapped in a release build.

- **The results of a turn's calls are one undo.** Every result a batch produced took a checkpoint
  of its own, so one `undo` after a turn that ran three tools took back the last result and left
  the model looking at a turn where two calls were answered and one never mentioned -
  `cancel_pending_calls` already refused that shape, and running the calls is the same thing
  happening. The first result takes the checkpoint and the rest join it, and the kernel's answer
  to a call naming no tool joins the turn it answers, which is recorded in the same step. A model
  asking for sixteen tools no longer spends the whole undo history.

- **An interrupt no longer outlives a request that failed.** Only reaching `Finished` put the flag
  down, so a provider that met the stop with an error, or a step future dropped after it, left it
  set - and the next `turn` was spent acknowledging it, sending nothing, and returning the state it
  started in. The flag is cleared wherever the machine is put back after a transition that did
  not finish.

- **`Kernel::annotate` discards the redo stack.** It takes no checkpoint, on purpose, but it is
  new work, and a redo that reached across it restored the old metadata over the new.

- **A compaction plan cannot take a pinned item out of the request through the item beside it.**
  The kernel refused a plan's removal of a pinned item and nothing else, but a call and its result
  go out together or not at all - so removing the turn a pinned result answers dropped the result
  as an orphan, and removing the only result a pinned turn's call asked for dropped the turn, with
  both still reading `Pinned` and `CompactionReport::refused` empty. A removal of either half of a
  pinned pair is refused now. Elision is untouched, since an elided item keeps its place in the
  pair.

- **An item a compaction plan names twice is moved once and reported once.** The report took every
  entry as it came, so `remove: [5, 5]` listed item 5 twice and counted its tokens twice, and an
  item in both `remove` and `elide` was excluded, then elided, and reported as both. Removal wins
  where the two lists overlap, and the report lists only what actually moved.

- **A step refused as `Error::Busy` no longer swallows the interrupt.** `step_once` cleared the
  flag before it took the machine lock, so a second thread calling `step` while a request was in
  flight got `Ok` where the state table promises `Busy`, and took the stop away from the request
  it was meant for - a `Provider` watching `DeltaSink::is_interrupted` saw it go false mid-stream,
  and the turn carried on. Busy is decided first now, and acts on nothing, so it spends nothing.

- **A tool result nothing in the request asks for keeps the place it had.** `LinearProjector`'s
  wire-ordering pass decided whether to defer a result by asking whether its identifier was in the
  map of results - which it had just built out of every result's own identifier, so the answer was
  yes for all of them. The branch meant to leave an unasked-for result where it was never ran, and
  with `repair_orphans` off the result was moved to the end of the request instead, past turns it
  came before, with nothing in `Projection::reordered` saying so.

- **`Kernel::replace` with what the item already says takes no checkpoint and announces nothing.**
  It checked that the item existed and then checkpointed unconditionally, so a replacement that
  changed no byte spent an undo: the next `undo` put back a state identical to the one it was
  asked from, and the operation somebody wanted reverted needed a second one. `set_state` has had
  the rule since it was written; this is the same rule, in the other place that needed it.

- **The component setters announce themselves under the lock that made the change.**
  `set_provider`, `clear_provider`, `add_tool`, `remove_tool`, `set_policy`, `set_projector`,
  `set_counter`, `set_compactor`, `set_params` and `reserve_calls` each took their lock as a
  temporary and emitted after it had been released - the shape the note on `Kernel::emit` calls
  out as wrong. Two clients swapping the same component could apply in one order and be logged in
  the other, leaving the log's last word on the provider naming the one that is not installed.

  What this asks of a component is that `Provider::info`, and the `name` of a policy, projector,
  counter or compactor, do not call back into the kernel: each is asked what was replaced while
  the lock holding it is held.

- **`Kernel::set_params` with the parameters already in force announces nothing.** It replaced and
  emitted unconditionally, so setting a key to the value it already holds, or loading a snapshot
  back into the session it was taken from, wrote `model.params` into the log over a request that
  goes out byte for byte the same. `Params` is a `Map`, which makes this the one component setter
  that can tell: the rest hold an `Arc<dyn Trait>`, where two that would behave alike are not
  comparable. The rule is the one `replace` and `set_state` follow.

## [0.6.2] - 2026-09-21

### fixed

- `tests/live.rs`'s `a_result_recorded_after_a_later_turn_still_reaches_the_api` reads the move
  it asserts on from `Projection::reordered`. It went on reading `repairs` after 0.6.0 split the
  two, and a move takes nothing out of the request, so `repairs` answers `[]` to it for ever: the
  test failed on every endpoint and every model before a request was sent, and only for whoever
  had a key, because a keyless run compiles the suite and skips it.

## [0.6.1] - 2026-09-19

### fixed

- `tests/crash.rs`'s `a_resumed_session_refuses_to_reuse_the_identifier` checks the refusal it is
  named for. It asserted that the snapshot carries `used_calls` and stopped there, never resuming -
  so it was a test of the precondition, and it would have passed unchanged if `Kernel::resume` had
  stopped extending `seen_calls` or `repair_call_ids` had stopped consulting them, which is the
  regression it exists to catch. It now resumes, lets the model ask for the spent identifier again,
  and asserts the rename, the `tool.repaired` reason, and that the key the application had already
  reconciled was not handed out a second time.

- `LinearProjector` orders a turn's results by the pairing it made rather than by identifier, so
  two calls that share a `ToolCallId` each keep their own answer. `repair_orphans` says the pairing
  is "one for one, in order, rather than by set membership" and names a hand-assembled or restored
  context as the case that makes it matter; the ordering pass then keyed results by identifier and
  handed both answers to the first call, leaving the second to reach the wire with nothing after
  it. Two `tool` messages in a row and a trailing unanswered call - the two shapes the dialect
  refuses a whole request over - and nothing in `repairs` or `skipped`, because nothing had been
  dropped. The property suite could not catch it: its generator mints a fresh identifier for every
  call, so the shape the adjacency property would fail on is one it never builds.

- `Kernel::cancel_pending_calls` takes one undo checkpoint for the batch rather than one per call,
  which is what "one operation is one undo" says and what `push_all` exists to do. Dropping a
  turn's calls is one thing somebody did; a checkpoint each made a single `undo` take back one
  refusal and leave the others, so the model was left looking at a turn where some of its calls
  were answered and one was never mentioned - and a model asking for sixteen tools spent the whole
  of the default undo depth on one keystroke. The two tests that cancelled anything cancelled one
  call, where the two behaviours are identical.

- `Calibrating::recalibrate` holds the scale it is handed to the same bounds the one it works out
  for itself goes through. `observe` clamped its ratio and this door had nothing on it, which
  matters because it is the door a `Snapshot` comes through: `Calibration` is `serde` with public
  fields, so what a file says a scale is has been derived by nobody. A `0.0` there is the value
  `Calibration::default` is hand-written to avoid, arriving by the other route - every figure in
  the session reported as nothing, a compactor that never fires and an oversized request never
  refused, and then `observe` dividing by it, which comes out as an infinity that saturates to
  `u64::MAX` on its way into a running total. A NaN is named separately rather than left to the
  clamp, which answers one with a NaN.
- `Kernel::recalibrate` decides whether to recount from what the counter says afterwards rather
  than from what it was handed, so a correction a counter declines to apply is not announced as a
  change to every stored figure.

## [0.6.0] - 2026-09-17

### added

- `TooLong` and `Overrun`: a request refused for being longer than the model takes, as the two
  numbers rather than as a sentence. A `Provider` returns it in place of a plain error where it
  recognises the refusal - reading a vendor's wording is a dialect's job, and there is none in this
  crate - and the kernel looks for it in whatever it was handed, `TooLong::of` walking the source
  chain. `Event::ModelFailed` gained an `overrun` field carrying it, so a client can say how much
  has to go rather than only that something went wrong. It needs both figures in the sentence, and
  not every endpoint gives both: one answers `You exceeded the maximum context length for this
  model of 260000` and never says what the request came to, which arrives as the prose it is.
  Filling the missing half from this crate's own estimate would put a guess on the wire under the
  server's name, and the estimate is what `refuse_oversized_requests` already acts on one step
  earlier.
- `Usage::settled`, which reads `cached_input_tokens > input_tokens` as an endpoint reporting the
  cache miss under the name of the whole prompt and adds the two. The one repair that is certain;
  what it must not become is a guess from the estimate at which convention an endpoint speaks,
  which is circular and resolves in favour of the error the counter already has.
- `test::TooLongProvider`, which refuses every request the way a model out of room does.
- `Config::refuse_oversized_requests`, on by default: a request the kernel can already see is
  longer than the model will read is refused here rather than sent, as `Error::TooLong`, and
  `Event::StepFailed` carries the same `Overrun` `ModelFailed` does - so a client says how much
  has to go without telling the two kinds of refusal apart. The round trip bought the endpoint's
  own account of a figure that was already on the screen, and it bought it after reading the whole
  request. It is a knob rather than a rule because it acts on an estimate: a counter that reads
  high for one model would refuse a request that endpoint would have taken, and the way out of
  that cannot be editing a context until an estimate is happy. Nothing is refused where the
  provider does not say what the model holds, and the comparison is `>` the limit rather than a
  fraction of it - a margin here would be the kernel having a policy about how full a context may
  be.
- `Tool::needs(call)`, which says which of the capabilities a tool declared *this* call is. A tool
  that does one thing declares it once on its spec and never implements this; a tool whose `action`
  picks between reading a file and writing one is the only thing that can tell those apart, because
  it is the only thing that knows its own arguments. It replaces the kernel reading the `action`
  argument for itself, which meant guessing from a string's shape whether `<name>:<name>` was an
  operation or a tool that happened to have a colon in its name. The default is the whole of
  `ToolSpec::capabilities`, which is safe for anything that declines to narrow: the strictest of
  everything consulted wins, so a tool that says nothing is judged against all of it.
- `Tool::limit(call)`, the same question about how much of a call's output the model is shown. A
  tool that reads a file and also searches a directory has two natural answer sizes and one
  `ToolSpec::output_limit` to put them in. What it decides is what the model is shown and never
  what is kept: the kernel still archives the whole of anything it shortens, under
  `Config::keep_truncated_output`.

### changed

- A call to a tool nobody registered is told which tools there are. A model told only that its word
  was wrong does not stop, it guesses again — one that spells every call `<tool>.<operation>` can
  read the definitions throughout without seeing which part of what it wrote is the wrong part. The
  registered ids are named rather than the nearest one to what was asked, because a suggestion is a
  guess about what was meant and this crate does not know.
- The counter learns from a refusal for length, not only from an answered request. `TokenCounter::observe`
  is now told what the model said the request came to, on the same condition as a reported usage:
  a request carrying anything the counter disowned still teaches it nothing. A usage figure is a
  bill, and an aggregator in front of a model may quote it in some other tokenizer's units; the
  number in a refusal is the model's own count of the same bytes, and the only one taken in the
  units the limit is enforced in. Without it a session that had run out of room corrected nothing,
  because every request from then on failed and no failure was a lesson.
- `Event::ContextRecounted` and `Event::SessionResumed` no longer call their figures the projected
  total. Both carry `Context::tokens()` - the sum over the items sending their content - and *the
  projected total* means something else here: what the projected messages cost, which is the figure
  `Budget::context_tokens` carries and `projection_cost` is the one definition of. They differ by
  whatever the projector does on the way out, a reference's label and an elided item's marker
  included. Documentation only; the numbers are the ones they always were.
- The readme's Google AI Studio invocation named `NACHALNIK_MODEL`, which nothing reads. The live
  suite reads `NACHALNIK_TEST_MODEL`, and getting it wrong is quiet: the suite falls back to its
  default model, that endpoint refuses a name it does not serve, and the failures are about tool
  calls rather than about the variable.
- `Content::truncate_to` starts its search at the end of the text rather than at the limit. What
  has to fit is `byte_len` and what is cut is `to_text`, and for a blob or a turn of blocks those
  are two different strings - a 4 MB picture names itself in nineteen characters - so the search
  walked down one character boundary at a time from an index the text never reaches. Same result,
  and `pricing_a_picture` is the third keyless example CI runs now, so the blob path has a run
  behind it rather than only a unit test.

### breaking

- A capability is a domain and an operation. `Capability` was an enum of six - `Read`, `Write`,
  `Edit`, `Shell`, `Network` and `Custom(String)` - and is now a struct over a `Domain` of its own:

  ```rust
  pub enum Domain { Fs, Exec, Net, Other(String) }
  pub struct Capability { pub domain: Domain, pub op: String }
  ```

  This crate ships no tools, so it can vouch for the domains every agent has - a filesystem, a
  process, a socket - and cannot know that a client calls one of its own `context`. Those arrive
  as `Domain::Other`, named by whoever brought them. `Capability::{fs, exec, net}` build one in a
  named domain and `of` builds one in any; `parse` reads `domain:op` back, which is the inverse of
  `Display`, so a rule spelled on a command line and a rule read off a screen are the same rule.

  What the old shape could not express is a rule narrower than a tool. A capability called `read`
  was declared by a tool called `read`, so a client showing what a rule covered had a tautology to
  show, and the granularity a rule could have was whatever granularity of tool somebody happened
  to register. A domain is a thing that can be acted on and an operation is one act on it, so a
  rule is either about the thing or about one act and there is no third question to ask.

  The recorded spelling changed with it. A capability serializes as `fs:read` where it was `read`,
  so a `permission.requested` event written by 0.5.2 no longer deserializes - it comes back as
  `` `read` names no operation; it should be `domain:op` ``. Nothing that resumes a session reads
  those: a `Snapshot` carries items, parameters and what the counter learned, and none of the
  three holds a `Capability`. What is affected is reading a session log back as `Record`.
- `#[non_exhaustive]` on `Selector` and `Which`, which name a syntax that grows, and on
  `StateChange`, `Record`, `Removed` and `CompactionReport`, which this crate answers with and
  nothing outside it builds - so the next variant or field on any of them is a patch. It is here
  rather than under `changed` because of what it costs the caller who was already doing either
  thing: a match over one of the two enums needs a wildcard arm, and the four structs can no
  longer be built with a struct literal outside this crate. Nothing in the workspace needed
  changing for it. Deliberately not marked: `Grant` and `Verdict`, which are the whole of what a
  decision can be; and `Budget`, `Usage`, `ModelInfo`, `ToolOutput`, `CompactionPlan`, `Projection`
  and `Skipped`, each of which looks like an answer and is built by somebody implementing one of
  the six traits.
- `Projection` has a `reordered` list beside `repairs`, and a move is in the new one. They were one
  list and they are two pieces of news: a repair is content the model would have had and will not -
  a call whose result is gone, a result whose call is, an ordered turn a flat shape cannot carry -
  and a move takes nothing out at all. A tool result has to reach the wire immediately after the
  call it answers, so an item pushed between the two sends it down the list and the projector puts
  it back, which is the layout rule working. `context: note` does that on every single call, the
  item being written while the call that writes it is still in flight - so a client honestly
  reporting `repairs` told the person the request had been repaired every time a model wrote
  anything down, and it stood for the rest of the session and every session resumed from it.

  `Event::ModelRequested` deliberately carries only `repairs`: a client's use of that field is to
  put it where somebody will see it, and the moves are the one thing nobody has to act on.

  A projector written outside this crate will not compile until it fills the new field; that is
  what `Projection` not being `#[non_exhaustive]` is for.

### fixed

- What an endpoint charges for its own framing is no longer read as a bias to correct for.
  `Calibrating` learns only from requests big enough to have a systematic error in them, and it was
  asking that of the *reported* figure - the wrong side of the ratio. An endpoint's preamble and the
  scaffolding round a tool call are a fixed cost, so on a small request they are the whole of the
  difference: `mercury-2.5` reported **933** tokens for a request estimated at 31, a ratio of 30,
  held at the bounds to 10, and from then on every figure in the session read ten times what it
  was. The second request of that session was counted at 53,210 tokens and refused for its length
  before it was sent. Both sides of the pair have to be a real request now.
- The whole of a shortened tool output carries the same label as the copy the model was shown. An
  output limit records two items for one call, and the label is the only name either row carries -
  the column beside it is the kind, which reads `tool_result` for both. Only the short copy was
  getting the tool and the operation, so a context listed `fs` above the `fs:read` it holds the
  rest of: a second call, apparently, by a tool that would not say what it did, on precisely the
  row somebody opens to read the part that was cut.

## [0.5.2] - 2026-09-14

### changed

- `session.rs` no longer claims the log carries no content. `Event::ContextReplaced` does: a
  replacement is the only operation that overwrites something, so once the change falls out of the
  undo window the old text exists nowhere else. The note names the exception and the other two ways
  content reaches a log - `Config::record_payloads` and `Config::record_progress`, both off by
  default. Documentation only.
- `Session` says how a caller reaches one: `Kernel::with_history`, the mirror of `with_context`.
  Grepping the kernel for "session" finds only `session_name`.

## [0.5.1] - 2026-09-12

### changed

- `Kernel::interrupt` documents that what it can stop depends on how the calls are run. Run
  together, `invoke_together` spawns every call before the first answers, so the only thing an
  interrupt reaches is a `Tool` checking `OutputSink::is_interrupted` itself. A tool that blocks
  without looking at the sink cannot be stopped in either mode. What the kernel does in every mode
  is record: every call gets an output, even when the output is that it never ran.
- `ToolSpec::schema` documents that the kernel does not validate arguments against it. The schema is
  sent to the model and counted for what it costs to send. Enforcing would mean choosing a JSON
  Schema dialect and a validator for every tool author, and checking in the tool makes a mismatch an
  ordinary `ToolOutput::error` the model can correct. A caller who wants it everywhere wraps `Tool`.

### added

- A section on surviving a crash, with tests. Whether a side effect happened is not the kernel's to
  know; what it provides is a durable *name* for the attempt - the call's own `ToolCallId`, which
  outlives the process - so an external operation keyed on it can be asked about afterwards.

  Checkpointing is two writes and the order decides what a crash costs: copy, write, drop, then
  snapshot. `history_since` hands back clones; `drain_history` hands back the only copy, so draining
  before writing opens a window in which the records exist nowhere. The snapshot goes last, since a
  snapshot ahead of the log is a state nothing accounts for.

## [0.5.0] - 2026-09-11

### breaking

- `PermissionPolicy::why` is asked with the `PermissionRequest` rather than a `ToolCallId`. A policy
  overriding it reads `request.call` where it read the argument. `evaluate` was handed everything
  known about a call and `why` only its name, so a policy whose reason depends on the arguments had
  to remember it in a map keyed by call - which wants a bound, past which the reason is gone.

### fixed

- A tool result reaches the wire immediately after the call it answers. `LinearProjector` kept a
  *count* of what the current turn was waiting for and reset it on the next turn, so a result
  belonging to an older turn went out wherever the context held it - putting a `tool` message four
  messages from its call, which an OpenAI-compatible API refuses. A count also let any result
  decrement it. The order is a pass over the whole list now; the messages a valid context projected
  to are unchanged. A client reads `moved item 5 up behind the call \`c0\` it answers`, one repair
  per result that moved.

### added

- A live test that a tool result recorded after a *later* turn still reaches a real endpoint beside
  the call it answers, with identifiers claimed through `Kernel::reserve_calls`. Measured: against
  the pre-fix projector the misordered request is **accepted** by Inception Labs' `mercury-2.5`, so
  "a real API accepted it" was never evidence the order was right. The test asserts the *position*.
- A property that a snapshot resumes into the session it was taken from, asserting that the resumed
  session projects to the same *request* rather than merely holding the same items, through serde.
- `the_generators_reach_what_the_properties_are_about`, which counts the states the properties have
  a branch for and fails if a run produces none of one. Four properties here were measurably weaker
  than they read - a name strategy topping out below the length limit it tested, an alphabet with
  nothing unpriced in it, one where a result never preceded its call.
- `tests/invariants.rs`: what holds of a context and its projection after every operation of a
  generated sequence. Invariants rather than a reference model, since a model faithful enough to
  compare against is a second implementation to keep honest. The alphabet is the whole of context
  control, sequences run past the default undo depth, and checks run after every operation so a
  counterexample is the shortest prefix. Measured against ten mutations: most ground is already held
  by existing tests, and what nothing else holds is a `resume` that forgets which identifiers are
  spent, plus the projection bug above, which lived in a suite of 597 passing tests.
- A test that a refusal can name the argument that earned it - the case the old `why` signature
  could not reach.

### changed

- The live interrupt test no longer asserts the endpoint took the continuation. The step after an
  interrupt sends a request *ending with a model turn*, which Google's OpenAI-compatible shim
  refuses with `400` where OpenAI's own API and OpenRouter continue from it.

## [0.4.0] - 2026-09-10

### breaking

- `Budget`, `ContextItem` and `Blob` each grew a public field and none is `#[non_exhaustive]`, so a
  struct literal no longer compiles. The fix is `..` or the constructor.
- `Blob` no longer derives `Eq`, because `serde_json::Value` does not. It is still `PartialEq`.

### added

- `Content::Blob` and the `Blob` it holds: bytes that are not text. The runtime does not look inside
  one - it carries, measures and hands it to a `Provider` - and *names* it where it must become
  text: `to_text` answers `[image/png, 12.05kB]`, and `as_blob` is how anything wanting the payload
  asks. The payload is held **already base64**, the form both dialects put it on the wire in, so
  nothing is encoded on the way out, `byte_len` is the size as sent, a log is the base64 string, and
  a base64 codec stays out of the dependency list. `Content` is `#[non_exhaustive]`.
- `Budget::uncounted` and `ContextItem::uncounted`: how many pieces of content the active
  `TokenCounter` would not price, so a count can abstain out loud. `Budget::fully_counted` is the
  question worth asking before believing `used()`. The budget's figure is counted over the projected
  messages, so an elided picture is not a hole in a request that no longer carries one.
- `TokenCounter::uncounted`, `uncounted_item` and `uncounted_message`, mirroring `count`,
  `count_item` and `count_message`. All default to `0`, so implementing them is optional.
- `Blob::meta`, an `Arc<Value>` the kernel never reads: somewhere to put what a counter needs to
  price a payload, which a byte length cannot reach - `{"w": 1024, "h": 768}`, `{"pages": 12}`.
  Every vendor's formula differs, so this crate carries the place to put the inputs instead. On the
  blob rather than the item, because a `Message` carries a `Content` and nothing else a counter
  could read. A `Provider` may read it too: `nachalnik-providers` reads `name` for the attachment
  part's required filename.
- `Content::blobs`, collecting every `Blob` including those nested in a `Content::Blocks` turn, and
  `Blob::wire_len`, the payload plus the media type naming it.
- `examples/pricing_a_picture.rs`: one context counted three ways - the default counter, a vendor
  tiling formula over `Blob::meta`, and that formula handed a blob nobody measured, which abstains.
- A test that a payload survives a snapshot, nested where a client attaching a file puts one.

### fixed

- `BytesPerToken` no longer counts a picture at four bytes a token when it arrives inside a turn.
  The abstention matched on `Content::Blob` and everything else fell through to `Content::byte_len`,
  which sums nested blobs - so one 400 KB screenshot counted `0` alone and **100,005 tokens** in the
  sentence-and-a-screenshot turn both dialects actually send.
- A request carrying something the counter would not price no longer teaches `Calibrating`. It
  corrects with a single multiplier, so an unpriced screenshot was spread over the bytes the counter
  could see: the prose then reads high while the picture still reads nothing, cumulatively, so
  deleting the picture did not undo it.

### changed

- A blob names its size as `[image/png, 12.05kB]` rather than `12048 bytes`. Two decimals, since
  these figures get compared; thousands rather than 1024, since what they get compared against is an
  API's documented limit. Under a thousand it stays an exact count.
- `BytesPerToken` returns `0` for a `Content::Blob` rather than dividing base64 by four, which is a
  number about an encoding. The figure is a **floor** and no longer a silent one:
  `BytesPerToken::uncounted` answers `1` per blob.

## [0.3.3] - 2026-09-09

### fixed

- An interrupt no longer outlives the turn it stopped. A `Provider` watching
  `DeltaSink::is_interrupted` honours the interrupt itself, so the turn ends in `Finished` with the
  flag still up and the *next* turn is spent clearing it - which from a client is a stop that eats
  the next message. Reaching `Finished` now puts the flag down; the resting states an interrupt is
  for are untouched.
- `Usage::output_tokens` says whether the reasoning is inside it, and `Usage::reasoning_tokens` that
  it is part of that number rather than a second one beside it. OpenAI's `completion_tokens`
  contains the reasoning, Google reports `candidatesTokenCount` and `thoughtsTokenCount` side by
  side, and a Google endpoint speaking the OpenAI dialect sends neither.

## [0.3.2] - 2026-09-08

### added

- `ModelResponse::thinking`, reading what the model thought wherever it is recorded - the
  `reasoning` field or a `Block::Reasoning` among ordered blocks. An iterator of `Content`.

### changed

- The `compare` example is `compare_models`. `nachalnik-eval` ships one called `compare` too, and
  two example targets with one name collide at `target/debug/examples/compare`. It is a `cargo`
  warning rather than a `rustc` one, which is why `RUSTFLAGS: -D warnings` never caught it.

### fixed

- A compaction pass that moved nothing leaves nothing behind. The summary went in regardless and the
  checkpoint condition counted `summary.is_some()` as something done - so the pass was asked again
  before the next request, the context was no smaller, and it said yes again.
- A pass may only take what the request is carrying, and the projection is what knows. The guard
  asked `is_projected`, a question about the state, so an item the projector had repaired away
  passed it. `apply_compaction` reads `included` from the projection it already takes.
- A `CompactionReport`'s two totals are the projected ones its documentation said they were. They
  summed the items sending content, which drops an elided item and never charges for its marker - so
  a pass eliding one 4,000-byte result reported 5 tokens remaining where a request cost 27. Both
  paths share `projection_tokens`. `Event::ContextRecounted` keeps its item sums.
- What a shortened tool result *is* outlives every state it passes through. The pair an output limit
  leaves behind kept "which item holds the whole of it" in `note`, which is replaced whenever the
  state changes. It goes in `included_because` now. Both fields document which facts belong in
  which: `included_because` for anything that must survive a state change, `note` for the state.
- A second result for one call is reported as `already has a result` rather than as a missing call.
  It is what restoring the whole of a truncated output produces.
- A superseded item's note reads `replaced by item 8` rather than `superseded by item 8`, which
  composed as `superseded: superseded by item 8`.

## [0.3.1] - 2026-09-06

### added

- `Kernel::recalibrate`, the front door for `TokenCounter::recalibrate`, which recounts. Applying a
  correction to an already-counted context otherwise leaves stored figures on the old scale while
  everything projected is on the new one.
- `Kernel::reserve_calls`, and `Event::ToolCallsReserved`. A client merging a snapshot *into* a
  running session keeps its kernel, so the turns it pushes carry identifiers that kernel never
  issued - and the next response is free to hand one back, with the request carrying the same
  `tool_call_id` twice.

### fixed

- A compaction pass that moves nothing takes no checkpoint. It checkpointed before knowing whether
  the plan amounted to anything, so a pass whose every candidate was pinned spent one of sixteen
  undos - and `checkpoint` discards the redo stack, so a compactor in that state took the redo away
  on every request for the rest of the session.
- `LinearProjector` holds a mid-turn item back under `send_blocks` too. The blocks branch skipped
  the bookkeeping that counts a turn's outstanding calls and flushes held items, so with that flag
  on the 0.3.0 repair never fired.
- Eliding an assistant turn takes its thinking with its words. A turn whose content had become a
  marker went on costing every token the model had thought, so eliding it freed nothing. Under
  `send_blocks` a signed thinking block went out beside a marker it was not signed over.
- `Calibrating` scales a wrapped counter's `count_message` instead of discarding it - the method the
  budget is counted over, and the one a real tokenizer overrides to charge for per-message framing.
- `Selector` knows `state:elided`. The state a `Compactor` is told to prefer was a parse error, and
  a `Selector::State` holding it printed as a string that would not parse back.
- `Kernel::turn` cannot swallow an interrupt. Both it and the `step` it called cleared the flag, so
  one landing between the two checks was spent on a step that transitioned nothing. The step is the
  only reader now and reports whether it acted.

## [0.3.0] - 2026-09-05

### added

- `ModelInfo::parameters`: the names of the `Params` a model accepts, where the provider publishes
  them. Empty means "not published", never "takes none".

### fixed

- `LinearProjector` keeps a tool result next to the call it answers. An item pushed into the context
  while a turn was still collecting its results was projected where it arrived, between the
  assistant message and the results - a request every OpenAI-compatible API refuses outright.
  Provider-dependent, so it was silent: Google's API accepts the sequence and so does at least one
  OpenRouter upstream. Whatever arrives mid-turn is held until the turn has been answered, and
  listed in `Projection::repairs`.

## [0.2.1] - 2026-09-01

### added

- `PermissionPolicy::why`, defaulted to `None`: the kernel asks a policy about to refuse a call
  whether it has anything to say, and puts the answer into the tool result. The reason is made of a
  policy's own vocabulary, and a kernel inventing one would be guessing at somebody else's decision.

### changed

- The truncation marker reads `[... 943 bytes truncated by an output limit ...]` rather than naming
  this crate. What the model can use is that something was cut, how much, and that a limit did it.
- A refused call is told which *kind* of refusal it was. A standing rule means the same call will
  meet the same answer; an answer to *this* call means a different approach may be allowed.

## [0.2.0] - 2026-08-30

An assistant turn can be the ordered sequence the model produced it in, rather than a content slot,
a reasoning slot and a flat list of calls.

**Breaking:** `LinearProjector` has a new public field, so a struct literal naming every field no
longer compiles; add `..Default::default()`. `Content` and `Block` are `#[non_exhaustive]`.

### added

- `Content::Blocks` and the `Block` enum it holds: a turn as the ordered sequence it was produced in
  - thinking, a sentence, a tool call, another sentence. A variant of `Content` rather than a field
  on `Message`, because content is the one thing a `ModelResponse`, a `ContextItem` and a `Message`
  all carry, so the order survives from the wire into the context and back out.
- `Message::calls`, `ModelResponse::calls` and `ContextItem::calls`: the tool calls a turn asked
  for, wherever recorded. A turn is recorded either the conventional way or as blocks, never both. A
  provider reading the `tool_calls` field directly would send an ordered turn with no calls in it.
- `LinearProjector::send_blocks`, off by default. Off, a turn recorded as blocks is flattened, and
  where that loses something - two thinking blocks joined, a sentence that came after a call
  arriving before it, a signature with nowhere to go - it is reported in `Projection::repairs`.
- `Part`, what the two non-call block variants hold: a `Content` and an `extra`. Some APIs sign each
  piece of a turn rather than the whole; Gemini's `thoughtSignature` rides on a text part as readily
  as on a call. Bound to the block, so removing the block removes the signature with it.
- `ContextItem::thinking`, the counterpart of `ContextItem::calls`. An ordered turn keeps the
  thinking in the content, where `ContextItem::reasoning` cannot see it.
- `Block::name` / `call` / `said` / `thought` / `part` / `extra` / `byte_len`, the `Block::text` and
  `Block::reasoning` constructors, `Content::blocks` / `as_blocks`, `Message::blocks`,
  `ModelResponse::blocks`, and `ToolCall::byte_len`.

### changed

- `Content::to_text` on blocks is what the turn *said* - the text blocks joined - and not what it
  costs: a provider putting thinking in a `content` field would send the model its own reasoning
  back as if it had uttered it. `byte_len` counts all of it, and `truncate_to` measures against
  that.
- `Kernel::repair_call_ids` repairs a call wherever it lives, rewriting the sequence when one is
  inside a turn's blocks, and only when something needed repairing.

## [0.1.0] - 2026-08-29

The first release: an agent loop as a state machine, with the context, the tools, the permissions
and the requests as explicit state.

### added

- `Kernel`, the loop: `step` performs exactly one transition and returns the `State` it produced,
  `turn` repeats until the model ends its turn or somebody has to decide something. `Requesting` and
  `Executing` are refused rather than duplicated, and a dropped step returns to `Idle`.
- `Context`, a list of identified items. Removal is a state change, so a removed item can still be
  listed, inspected and restored - including for an output limit, which archives the whole of what a
  tool said beside the shortened copy the model is shown.
- `undo` / `redo` / `supersede` / `replace` / `annotate` / `push_all`, each one operation.
- `ContextState::Elided`, the third answer between in and out: the item stays in the request as a
  short marker instead of its content, which is what a `Compactor` should reach for through
  `CompactionPlan::elide`. Excluding a tool result forces the projector to drop the call that asked
  for it, so the model reads a history in which it never asked; an elided result still answers its
  call. `ContextState::sends_content` is the predicate the token figures are built on.
- `TokenCounter::count_message`, defaulted, and with it a budget counted over the messages the
  projector produced rather than the items that went in. A reference is labelled on its way out, so
  `src/parser.rs:\n` went on the wire without appearing on the bill. It also means the budget
  answers with what the counter knows now rather than when each item was pushed.
- Six seams, all replaceable at runtime: `Provider`, `Tool`, `PermissionPolicy`, `Projector`,
  `TokenCounter`, `Compactor`. Each of the four with no other way to identify itself carries a
  `name()` defaulting to the implementing type's path.
- `preview_request` and `preview_payload`: the exact request, and the provider's own bytes for it,
  before anything is sent.
- `Event`, an append-only session log of typed events covering every transition, broadcast live.
  Replacing any seam is an event carrying `from` and `to`; a compactor removed is `to: None`.
  Context events are recorded while the context lock is held, so the log's account of an item's
  states is in the order they were applied.
- `Snapshot` / `resume`, because a log of events that name their items cannot rebuild the items. A
  snapshot carries `calibration`, so a resumed session does not relearn what it had been told.
- `interrupt`, which stops the loop between transitions, and - through `DeltaSink::is_interrupted`
  and `OutputSink::is_interrupted` - lets a provider or tool stop what is in flight without losing
  what it had. Plus `Config::max_requests_per_turn` and `cancel_pending_calls`.
- `Calibrating`, a token counter that corrects another against what providers charge, via
  `TokenCounter::observe`. `Kernel::new` starts with it wrapped around `BytesPerToken`, correcting
  by `1.0` until a provider has said something; the bare estimate is
  `set_counter(Arc::new(BytesPerToken::default()))`.
- `Kernel::with_context` and `with_history`, answering a question about the context or the log
  without copying either. `context()` and `history()` say on themselves that they copy.
- `Config::parallel_tool_calls`, off by default: the one place the kernel spawns tasks.
- Features: `selectors` (a small language for naming context items) and `test` (a scripted provider,
  dummy tools, off-the-shelf policies and a mechanical compactor).
- Five dependencies, no `unsafe`, no system prompt, no default tools, no HTTP client.
- The crate documentation says what it does *not* protect you from: there is no sandbox and the
  kernel executes nothing, so what it enforces is that a refused call never reaches `Tool::invoke`.
  `Capability`'s documentation says `Shell` subsumes every other capability, and that what closes
  the gap is `PermissionRequest::args`, which a policy is handed and a capability list cannot see.
