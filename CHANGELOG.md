# 0.1.0 (unreleased)

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
  `TokenCounter`, `Compactor`.
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
- Five dependencies, no `unsafe`, no system prompt, no default tools, no HTTP client.
