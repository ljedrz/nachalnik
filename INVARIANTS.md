# invariants

Break one of these and something in `tests/` should go red. If it does not, the missing test is
part of the change - and **measure** that rather than assuming it: see *a test's worth is measured*
in [CONTRIBUTING.md](CONTRIBUTING.md), because it is cheap to get wrong in both directions.

Each one carries the reasoning that put it there, because for these the argument is the part
worth reading: a rule you can restate and cannot justify is one you will trade away the first
time it is inconvenient. [AGENTS.md](AGENTS.md) lists them without it.

---

- **Nothing is destroyed.** Removal is a state change. An excluded, archived or superseded item
  keeps its identifier, is still listed and inspectable, and comes back with a `set_state`, an
  `undo` or a `redo`. This holds for the output limit too: the whole of a truncated tool result is
  archived beside the truncated copy the model is shown (`Config::keep_truncated_output`).
- **The previewed request is the request.** There is no step between `preview_request()` and the
  wire where the kernel adds anything of its own. The one thing that may still intervene is a
  `Compactor`, and it reports exactly what it did.
- **Identifiers are never reused**, including by items that `undo` took away, and including tool
  call identifiers across a resumed session (`Snapshot::used_calls`, `repair_call_ids`).
- **Every state change is an `Event`**, and the log and the broadcast are written under one lock
  so their order agrees - with each other, and with the order the changes were actually applied in.
  No logging a user cannot see.
- **The log names things, it does not copy them.** `model.requested` records context ids, not
  messages. `context.replaced` is the one event carrying content, because overwritten text is the
  one thing nothing else can recover.
- **A pin is a promise**: the kernel refuses a `Compactor`'s attempt to remove a pinned item and
  says so in `CompactionReport::refused`.
- **One operation is one undo.** `push_all`, `set_state` over eight ids, `supersede`, a recorded
  turn - one checkpoint each. An operation that changes nothing takes no checkpoint, and one that
  is about to fail takes none either.
- **A failing `Tool` is not a kernel error.** It becomes an error tool result the model is shown.
  `Error` is only for conditions that stop the loop.
- **Nothing in a model's output reaches the policy** except the tool name and the arguments, both
  as data. A model insisting it already has permission has no effect.
- **`Content` is shared, not copied.** Every variant is behind an `Arc`; pruning a four-megabyte
  tool result moves a pointer. Do not introduce a path that clones the bytes.
- **The counter is honest about being an estimate.** No tokenizer goes into this crate - that
  would be a model-specific assumption. `Calibrating` corrects from what providers charge, from
  then on, and never silently rewrites figures already recorded (`Kernel::recount` does, loudly).
- **A count that cannot reach something says so rather than returning `0`.**
  `TokenCounter::uncounted` is how, and it rides up to `Budget::uncounted` and
  `ContextItem::uncounted`, so
  "measured, and free" and "there is a picture here and nothing priced it" are never the same
  figure. Two rules follow and both are load-bearing. A request carrying anything unpriced does
  not reach `TokenCounter::observe`: `Calibrating` corrects with a single multiplier, so a gap it
  cannot see gets spread over the bytes it can, and prose beside one screenshot ends up reading
  50% high while the screenshot still reads nothing. And whatever a counter *would* need in order
  to price a payload goes in `Blob::meta`, which the kernel never reads - on the blob rather than
  the item, because the budget is counted over projected messages and a `Message` carries a
  `Content` and nothing else a counter can see.

  This crate carries no vendor formula and is not going to. A dialect is a shape that changes
  over years and a price list is a per-model fact that changes whenever a vendor ships a model,
  so putting the formulas in `nachalnik-providers` would turn "we speak two dialects" into a
  subscription - and be wrong silently, which is what the abstention exists to end. The three
  formula *shapes* stay as prose on `BytesPerToken::count`. Two near misses, so nobody
  re-proposes them: a typed `dimensions` on `Blob` covers pictures and leaves a PDF's pages and a
  recording's seconds nowhere to go, and a `tokens: Option<usize>` on `Blob` is a per-model
  figure on a model-agnostic type, wrong the moment the model changes.

- **A media type is a claim, and nothing in here guesses one.** Three places act on it and all
  three would be wrong if it were inferred. `kamchatka`'s `attach::TYPES` maps ten extensions and
  refuses anything else that is not valid text, rather than sniffing the bytes - an uncompressed
  PDF is valid UTF-8 for pages at a time, so "is this text?" answers yes and the model is sent
  PDF source. The OpenAI dialect then reads the media type to pick between `image_url` and
  `file`, because in that dialect `image_url` means an image and a PDF sent through it is a 400;
  Google's `inline_data` needs no such split. And `Blob::meta["name"]` is what fills that `file`
  part's required filename - a convention between a caller and a provider, *not* a key the kernel
  knows, which is the whole point of `meta` being free-form. A derived `file.pdf` is the fallback
  because the part is refused without one.

  `nachalnik-providers` deliberately does not implement this dialect's third payload shape,
  `input_audio`. Nothing in the workspace produces a recording, so it would be a shape written
  from a specification and pinned by no test - which is exactly what the `file` part was until
  `a_document_goes_out_as_a_document` in `nachalnik/tests/live.rs` sent one at a real endpoint.
  That test is the reason to trust the shape; there is no offline equivalent.
