# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

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
