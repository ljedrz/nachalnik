# invariants

Breaking one of these should make something in `tests/` fail. If nothing fails, the missing test
is part of the change. Check that rather than assuming it (see *check that tests catch something*
in [CONTRIBUTING.md](CONTRIBUTING.md)); it is easy to get wrong either way.

Each rule comes with its reason, because a rule nobody can justify gets dropped the first time it's
inconvenient. [AGENTS.md](AGENTS.md) lists them without the reasons.

---

- **Nothing is destroyed.** Removing something is a state change. An excluded or elided item keeps
  its identifier, is still listed and can be inspected, and comes back with `set_state`, `undo` or
  `redo`. This applies to the output limit too: when a tool result is truncated, the full result is
  kept as an excluded item next to the truncated copy the model sees
  (`Config::keep_truncated_output`).
- **The previewed request is the request.** The kernel adds nothing of its own between
  `preview_request()` and sending. What can still change it is the start of the next step: a
  `Compactor`, which reports exactly what it did, and the caller's full notice
  (`Kernel::set_full_notice`), placed or retired as the compactor finds the context full or not,
  each time an ordinary change to the context with its own event.
- **Identifiers are never reused**, including those of items removed by `undo`, and including tool
  call ids, record numbers and permission ids across a resumed session (`Snapshot::used_calls`,
  `repair_call_ids`, `Snapshot::last_seq`, `Snapshot::next_permission`).
- **Every state change is an `Event`**, and the log and the broadcast are written under one lock,
  so they are in the same order as each other and as the changes themselves. No logging that the
  user can't see.
- **The log refers to things, it doesn't copy them.** `model.requested` records context ids, not
  messages. `context.replaced` is the only event that carries content, because overwritten text
  can't be recovered any other way. The rendered request (`model.payload`) and streamed fragments
  are only kept if `Config::record_payloads` and `Config::record_progress` are set. Item metadata is
  copied too (into `context.added`, and the replaced metadata into `context.annotated`), because
  compactors make decisions based on it and nothing else keeps the old value.
- **A pin is a promise**: the kernel refuses a `Compactor`'s attempt to remove a pinned item and
  reports it in `CompactionReport::refused`. The same goes for removing the tool call or result
  that a pinned item is paired with, because the two are always sent together or not at all.
- **One operation is one undo step.** `push_all`, `set_state` over eight ids, `supersede`, a
  recorded turn: one checkpoint each. An operation that changes nothing creates no checkpoint, and
  neither does one that is about to fail. `annotate` is the one exception: it takes no checkpoint
  of its own, so an undo takes an annotation back with the operation before it.
- **A failing `Tool` is not a kernel error.** It becomes an error result that the model sees.
  `Error` is only for conditions that stop the loop. A panicking tool counts as failed: the panic is
  caught where the kernel polls the tool, handled the same way, and reported as `tool.panicked`.
- **Nothing in a model's output reaches the permission policy** except the tool name and the
  arguments, both as data. A model claiming it already has permission changes nothing.
- **`Content` is shared, not copied.** Every variant is behind an `Arc`, so pruning a four-megabyte
  tool result only moves a pointer. Don't add code paths that clone the bytes.
- **The token counter openly says it's an estimate.** No tokenizer goes into this crate, since
  that would assume a particular model. `Calibrating` adjusts its estimates based on what providers
  actually charge, from then on, and never silently rewrites figures already recorded
  (`Kernel::recount` does, and says so).
- **A count that can't see something says so rather than returning `0`.**
  `TokenCounter::uncounted` reports it, and it is passed up to `Budget::uncounted` and
  `ContextItem::uncounted`, so "measured and free" and "not priced" are never the same number. A
  request containing anything unpriced is not passed to `TokenCounter::observe`, because
  `Calibrating` corrects with a single multiplier and would spread the unknown cost over the bytes
  it can see. Anything a counter needs to price a payload goes in `Blob::meta`, which the kernel
  never reads.

  No vendor pricing formulas go into a crate: prices are per model and change whenever a vendor
  ships one, and a formula here would silently become wrong. An example may write one out, saying
  whose it is, as `pricing_a_picture` does.

- **A media type is the caller's claim; nothing here guesses one.** Everything that depends on it
  (how an API sends a blob, whether a provider accepts it at all) would be wrong if it were guessed,
  and inspecting the bytes is guessing: an uncompressed PDF can be valid UTF-8 for pages at a time.
  `kamchatka` labels the types it is sure of, treats anything else that is valid text as text, and
  refuses the rest. How a media type is sent is only trusted once a live test has sent it to a real
  endpoint.
