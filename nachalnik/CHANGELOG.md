# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- **Replacing any of the six seams is an event, and the event names what was replaced.** Setting
  the projector, the counter or the compactor emitted nothing at all, and `policy.changed` said
  only that the policy had been swapped - so a session log could not answer "what was projecting
  these requests?" or "was anything dropping items here?" for a session where somebody had changed
  it half way, and those are the questions a log is for. `projector.changed`, `counter.changed` and
  `compactor.changed` join `policy.changed`, all four carrying `from` and `to` as the seams' own
  `name()`; a compactor removed is `to: None`, since "nothing will ever be dropped from now on" is
  the change that matters most and was the one least visible. `policy.changed` gaining those fields
  is a breaking change to a variant nothing has released yet.
- `Kernel::with_history`, the mirror of `with_context`: a question about the log - a count, a
  search - answered without copying it. `history` says on itself that it copies the whole thing.
- `TokenCounter::calibration` and `recalibrate`, both defaulted, and `Snapshot::calibration`
  (`serde(default)`) that carries what one hands out to the other. A resumed session no longer
  spends its first requests relearning what it had already been told, and - because resuming
  recounts - comes back with the corrected figures rather than the stale ones.
- `Calibration` now derives `Serialize`/`Deserialize` and has a hand-written `Default` (scale
  `1.0`, not `f64::default()`).

### changed

- The crate documentation has a `What it does not protect you from` section: there is no sandbox,
  the kernel executes nothing, and what it enforces is that a refused call never reaches
  `Tool::invoke` - a decision point with a paper trail rather than a boundary.
- `Capability`'s documentation says that `Shell` subsumes every other capability, so a policy that
  allows it has allowed all of them - and that what closes the gap is `PermissionRequest::args`,
  which a policy is handed and a capability list cannot see.

### fixed

- Context events are recorded while the context lock is still held, so the log's account of an
  item's states matches the order they were applied in. Two threads changing one item could apply
  in one order and be announced in the other, leaving the log's last word on that item
  contradicting the item itself.

### changed

- The counter a `Kernel::new` starts with is `Calibrating<BytesPerToken>` rather than a bare
  `BytesPerToken`. It corrects by `1.0` until a provider has reported what a request cost, so it is
  the same counter until there is something better to be - and the low estimate is no longer what
  everybody who did not read the documentation was left with. `kernel.counter().name()` says both
  halves now; `set_counter(Arc::new(BytesPerToken::default()))` gets the bare one back.

### 0.1.0

The first release: an agent loop as a state machine, with the context, the tools, the permissions
and the requests as explicit state.

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
- `Snapshot` / `resume`, because a log of events that name their items cannot rebuild the items.
- `interrupt`, which stops the loop between transitions, and - through
  `DeltaSink::is_interrupted` and `OutputSink::is_interrupted` - lets a provider or a tool stop
  what is already in flight without losing what it had. Plus `Config::max_requests_per_turn` and
  `cancel_pending_calls`.
- `Calibrating`, a token counter that corrects another against what providers actually charge,
  via `TokenCounter::observe` - the kernel reports what a request was estimated at and what it
  cost, and the counter decides what to make of it.
- `Config::parallel_tool_calls`, off by default: the one place the kernel spawns tasks.
- Features: `selectors` (a small language for naming context items) and `test` (a scripted
  provider, dummy tools, off-the-shelf policies and a mechanical compactor).
- Five dependencies, no `unsafe`, no system prompt, no default tools, no HTTP client. The
  OpenAI-compatible provider the examples and the live suite talk through lives in
  `nachalnik-utils`, an unpublished `0.0.0` workspace member that is a dev-dependency and nothing
  else - so none of it reaches anybody who depends on this crate.
