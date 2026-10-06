# invariants

Break one of these and something in `tests/` should go red. If it does not, the missing test is
part of the change - and **measure** that rather than assuming it: see *a test's worth is measured*
in [CONTRIBUTING.md](CONTRIBUTING.md), because it is cheap to get wrong in both directions.

Each one carries the reasoning that put it there: a rule you can restate and cannot justify is
one you will trade away the first time it is inconvenient. [AGENTS.md](AGENTS.md) lists them
without it.

---

- **Nothing is destroyed.** Removal is a state change. An excluded or elided item keeps its
  identifier, is still listed and inspectable, and comes back with a `set_state`, an `undo` or a
  `redo`. This holds for the output limit too: the whole of a truncated tool result is excluded
  beside the truncated copy the model is shown (`Config::keep_truncated_output`).
- **The previewed request is the request.** There is no step between `preview_request()` and the
  wire where the kernel adds anything of its own. The one thing that may still intervene is a
  `Compactor`, and it reports exactly what it did.
- **Identifiers are never reused**, including by items that `undo` took away, and including tool
  call identifiers, record numbers and permission identifiers across a resumed session
  (`Snapshot::used_calls`, `repair_call_ids`, `Snapshot::last_seq`, `Snapshot::next_permission`).
- **Every state change is an `Event`**, and the log and the broadcast are written under one lock
  so their order agrees - with each other, and with the order the changes were actually applied in.
  No logging a user cannot see.
- **The log names things, it does not copy them.** `model.requested` records context ids, not
  messages. `context.replaced` is the one event carrying content, because overwritten text is the
  one thing nothing else can recover; the rendered request (`model.payload`) and the streamed
  fragments are kept only when `Config::record_payloads` and `Config::record_progress` ask for
  them. An item's metadata is copied too - into `context.added`, and what an annotation replaced
  into `context.annotated` - because it is a hint a compactor decides by, and nothing else keeps
  the one it replaced.
- **A pin is a promise**: the kernel refuses a `Compactor`'s attempt to remove a pinned item and
  says so in `CompactionReport::refused` - and the same for a removal of the call or the result a
  pinned item is paired with, because the two go out together or not at all.
- **One operation is one undo.** `push_all`, `set_state` over eight ids, `supersede`, a recorded
  turn - one checkpoint each. An operation that changes nothing takes no checkpoint, and one that
  is about to fail takes none either.
- **A failing `Tool` is not a kernel error.** It becomes an error tool result the model is shown.
  `Error` is only for conditions that stop the loop. A tool that panics has failed too: the panic
  is caught where the kernel polls the tool, answered the same way, and named by `tool.panicked`.
- **Nothing in a model's output reaches the policy** except the tool name and the arguments, both
  as data. A model insisting it already has permission has no effect.
- **`Content` is shared, not copied.** Every variant is behind an `Arc`; pruning a four-megabyte
  tool result moves a pointer. Do not introduce a path that clones the bytes.
- **The counter is honest about being an estimate.** No tokenizer goes into this crate - that
  would be a model-specific assumption. `Calibrating` corrects from what providers charge, from
  then on, and never silently rewrites figures already recorded (`Kernel::recount` does, loudly).
- **A count that cannot reach something says so rather than returning `0`.**
  `TokenCounter::uncounted` is how, and it rides up to `Budget::uncounted` and
  `ContextItem::uncounted`, so "measured, and free" and "nothing priced this" are never the same
  figure. A request carrying anything unpriced does not reach `TokenCounter::observe`, because
  `Calibrating` corrects with one multiplier and would spread a gap it cannot see over the bytes it
  can. What a counter would need to price a payload goes in `Blob::meta`, which the kernel never
  reads.

  No vendor formula goes into this workspace: a price is a per-model fact that changes whenever a
  vendor ships a model, and a formula carried here would be wrong silently, which is what the
  abstention exists to end.

- **A media type is a claim, and nothing in here guesses one.** Everything that acts on it -
  which part a dialect sends a blob as, whether a provider takes it at all - would be wrong if it
  were inferred, and sniffing bytes is inference: an uncompressed PDF is valid UTF-8 for pages at a
  time. `kamchatka` names the types it is sure of and treats everything else that is valid text as
  text, and refuses the rest. A wire shape for a media type is trusted once a live test has sent
  it to a real endpoint.
