# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- `tests/invariants.rs`: what holds of a context and its projection after every operation of a
  generated sequence. Invariants rather than a reference model, because a model faithful enough to
  compare against is a second implementation of the context that has to be kept honest, and a
  wrong model reads exactly like a broken kernel. What is checked instead are sentences this
  crate had already written down - `set_state`'s note that an operation changing nothing takes no
  checkpoint, `undo`'s that the granularity is one operation and not one item, `Projection`'s that
  a repair is named rather than done quietly - and the family where two things must agree about
  one request, which is where every bug worth fixing in the recent releases lived.

  The alphabet is the whole of context control: `push`, `push_all`, the five moves over one
  identifier or several, `replace`, `supersede`, `undo`, `redo`. Sequences run past the default
  undo depth, because an undo stack is a bounded thing and the interesting arithmetic is at its
  bound. The checks run after *every* operation rather than at the end, so a counterexample is the
  shortest prefix that breaks something.

  Measured, mutation by mutation: a `set_state` that spends a checkpoint on a no-op, a projector
  that stops repairing orphaned calls, one that holds an item back without saying so, a
  `push_all` that takes a checkpoint per item, and a budget that reports every request as fully
  counted. Each is caught, and each by the assertion it was aimed at.

  Two of those needed the suite strengthened first, and that is the part worth reading. The
  granularity `undo` documents is invisible to a round trip - undoing everything and redoing
  everything restores the context whether a checkpoint was spent per operation or per item - so it
  took an assertion of its own. And the invariant about `uncounted` could not fail at all until
  the alphabet could push a `Content::Blob`: with nothing unpriced in any generated context, a
  kernel changed to report every request as fully counted broke nothing. An invariant that cannot
  fire reads exactly like one that holds.

  It also found something, which is written down in the file rather than fixed here: a tool result
  is projected out of position when another assistant turn stands between it and the call it
  answers. `projection.rs` tracks what a turn is waiting for as a count and a new turn resets it,
  so a result belonging to an older turn has nothing anchoring it - and the request goes out with
  a `tool` message that does not follow its call, which is the shape the conventional dialect
  refuses. The suite asserts the weaker property that holds today; the stronger one is what a fix
  would let it say.

- A test that a refusal can name the argument that earned it: two calls to one tool, the same
  capability twice, one of the two paths ending in `.env`, and a policy that holds nothing at all.
  It is the case the old signature could not reach - the identifiers are the kernel's to hand out
  and the tool and the capability are the same one twice, so the arguments are the only thing that
  tells the pair apart.

### breaking

- `PermissionPolicy::why` is asked with the `PermissionRequest`, where it was asked with a
  `ToolCallId`. A policy that overrides it - one that has something to say, which is the only
  reason the method exists - changes the signature and reads `request.call` wherever it read the
  argument.

  The two halves of the seam were being asked different questions about the same call. `evaluate`
  is handed everything known about it; `why` was handed its name. So a policy whose reason is a
  function of the arguments - *the rule for `**/.env` refused `deploy/.env`* - had to write that
  sentence down at the moment it decided and find it again when it was asked, which is a map keyed
  by call. A map wants a bound, and a bound is a number of refusals in one step past which the
  reason is simply gone; `kamchatka`'s remembers sixty-four. None of that is a policy's problem to
  have.

  The kernel was holding the request the whole time. `PreparedCall` has carried it since
  permission stopped being decided inside execution, so this hands over a field that was already
  there rather than building one to answer with, and what a policy sees in `why` is the identical
  value it saw in `evaluate`.

  What the model reads is unchanged, and no policy in this workspace answers differently:
  `kamchatka`'s `Careful` keeps its own copy either way, because its other reader is a screen with
  nothing but an identifier - `Event::PermissionDecided` carries no arguments. This moves the seam
  rather than the program, which is the honest summary of a release with one signature in it.

## [0.4.0] - 2026-09-10

### added

- `examples/pricing_a_picture.rs`, which is what `Blob::meta` and
  `TokenCounter::uncounted` are *for*. It counts one context three ways - with the default
  counter, with one that applies a vendor's tiling formula to `{"w": .., "h": ..}`, and with
  the same formula handed a blob nobody measured - so that "put a real tokenizer behind
  `Kernel::set_counter`" stops being advice and becomes forty lines somebody can copy. The
  third case is the one worth reading: a counter that knows a formula still abstains on a
  payload with no dimensions on it, exactly as the default one does.

- `Content::Blob`, and the `Blob` it holds: bytes that are not text - an image, a document, a
  recording. The runtime does not look inside one. It carries it, measures it and hands it to a
  `Provider`, the same as everything else, and *names* it wherever it has to become text, because
  a gap where a picture was is worse than a sentence saying there was one:
  `[image/png, 12.05kB]` is what `to_text` answers, and `as_blob` is how anything that wants
  the payload asks for it.

  The payload is held **already base64**, which is deliberate. It is the form both dialects put it
  on the wire in - a `data:` URI in one, `inline_data` in the other - so nothing is encoded on the
  way out; `byte_len` really is the size in the form it would be sent in rather than three
  quarters of it; a session log is the base64 string rather than a JSON array of six hundred
  thousand numbers; and a base64 codec stays out of a crate with five dependencies and a rule
  about growing a sixth. Whoever reads a PNG off a disk encodes it, where a base64 crate is free
  to be.

  `Content` is `#[non_exhaustive]`, so this breaks no `match` - and `nachalnik-providers` renders
  it in both dialects as of its first release.

- `Budget::uncounted` and `ContextItem::uncounted`: how many pieces of content the active
  `TokenCounter` would not put a number on, so that a count can **abstain out loud**. A counter
  returning `0` because it measured something and found it free and a counter returning `0`
  because there is a picture in front of it and nothing priced one were the same figure until
  there was a second one beside them - and the difference is the difference between a floor
  somebody can act on and a fiction. `Budget::fully_counted` is the question worth asking before
  believing `used()` or `fraction_used()`.

  The budget's figure is counted over the *projected messages*, like its tokens, so a picture
  that has been elided is not reported as a hole in a request that no longer carries one. The
  item's figure is about the item, and stays.

- `TokenCounter::uncounted`, `uncounted_item` and `uncounted_message`: the trio behind those,
  mirroring `count`, `count_item` and `count_message`. All three are defaulted to `0`, which is
  the honest answer for a real tokenizer and is why implementing them is optional.

- `Blob::meta`, an `Arc<Value>` the kernel never reads. The same bargain `ContextItem::meta`
  strikes: somewhere to put a fact the runtime has no business having an opinion about. The fact
  that matters here is whatever a counter would need to price a payload, because a byte length
  cannot reach it - `{"w": 1024, "h": 768}` for a picture, `{"pages": 12}` for a document,
  `{"seconds": 184, "fps": 30}` for a recording. Every vendor's formula is over figures like
  those and each vendor's is different, so this crate carries none of them and carries the place
  to put the inputs instead. Whoever produced the base64 had the payload decoded a moment
  earlier, which is why it costs a caller nothing to fill in and is the only place that knows.

  On the blob rather than on the item, and that is checkable rather than a preference: a budget
  is counted over the projected messages, and a `Message` carries a `Content` and nothing else a
  counter could read. A fact left on `ContextItem::meta` reaches `count_item` and never reaches
  the figure a `Compactor` acts on.

  A counter is the reason it exists and not the only thing entitled to read it. A `Provider` may
  too, and one already does: the conventional dialect's attachment part will not go out without a
  filename, so `nachalnik-providers` reads `name` here. That is a convention between a caller and
  a provider rather than anything this crate enforces - there is no key here whose meaning the
  kernel knows, which is the entire point of the field.

- A test that a payload survives a snapshot, nested where a client attaching a file actually puts
  one and with its `meta` intact. Three things that each break silently and none of which the
  session suite reached: serde carrying a `Content::Blob` inside a `Content::Blocks`, the two
  different paths a `meta` that is set and a `meta` that is null take through one `Deserialize`,
  and `uncounted` coming back the same on the other side - a resumed context reporting a picture
  as free would be the abstention lost at the moment nobody would look for it. It round-trips;
  what was missing was anything saying so.

- `Content::blobs`, which collects every `Blob` in a piece of content **including those nested in
  a `Content::Blocks` turn**. This is the seam a counter needs and could not build for itself, and
  the nesting is the whole reason; see the fix below. `Blob::wire_len` is the payload and the
  media type naming it, which is what a blob costs on the wire and what has to come off a byte
  count.

### fixed

- `BytesPerToken` no longer counts a picture at four bytes a token when it arrives inside a turn.
  The abstention was a match on `Content::Blob` and everything else fell through to
  `Content::byte_len`, which sums the blobs nested in a `Content::Blocks` - so the same 400 KB
  screenshot counted `0` on its own and **100,005 tokens** in the sentence-and-a-screenshot turn
  both dialects actually send, which is the shape a model is realistically shown one in. Free or a
  hundred thousand tokens depending on which shape it arrived in, and the hundred thousand is the
  exact failure the abstention was written to prevent.

- A request carrying something the counter would not price no longer teaches `Calibrating`
  anything. It corrects with a single multiplier, so an unpriced screenshot did not stay a local
  gap: it got spread over the bytes the counter *could* see. Two thousand tokens of prose beside
  one picture settles on a scale of about 1.5, and from then on the prose reads three thousand
  while the picture still reads nothing - two figures wrong in opposite directions and no item's
  number right. The ratio is cumulative, so deleting the picture did not undo it; it diluted over
  the next few text-only requests, and `BOUNDS` capped the damage at ten times and did nothing
  else. The counter disowned that content and the kernel now respects the disownment.

### changed

- A blob names its size the way a person reads one: `[image/png, 12.05kB]` rather than
  `[image/png, 12048 bytes]`. That string lands in a terminal's narrowest column, in a model's
  context, and in the sentence a dialect sends where it has nowhere to put a payload, and in all
  three the digits past the third are noise. Two decimals, because these figures get compared
  against each other and `1.05MB` beside `1.10MB` is a comparison where `1MB` beside `1MB` is
  not; thousands rather than 1024, and `kB` rather than `KiB`, because what they get compared
  against is an API's documented limit and those are quoted decimal. Under a thousand it stays an
  exact count with no decimals at all.

- `BytesPerToken` returns `0` for a `Content::Blob` rather than dividing its bytes by four. Those
  bytes are base64, and base64 over four is a number about an encoding and not about a model: a
  400 KB screenshot would arrive as a hundred thousand tokens and send a compactor after a context
  that is nowhere near full. What a picture really costs is a formula over its *dimensions* which
  every vendor publishes and each publishes differently, and none of them is reachable from a byte
  length.

  So the figure is a **floor**, and it no longer is one silently: `BytesPerToken::uncounted`
  answers `1` per blob, and that rides up to `Budget::uncounted` and `ContextItem::uncounted`
  above. A real tokenizer through `Kernel::set_counter` is still the answer for anyone who needs
  the number itself to be right, and `Blob::meta` is where it gets the inputs.

### breaking

- `Budget`, `ContextItem` and `Blob` each grew a public field, and none of the three is
  `#[non_exhaustive]`, so a struct literal for any of them no longer compiles. The fix is `..` or
  the constructor; nothing was removed and no signature changed.
- `Blob` no longer derives `Eq`, because `serde_json::Value` does not implement it. It is still
  `PartialEq`, so `==` is unaffected; a bound that named `Eq` is the only thing that breaks.

## [0.3.3] - 2026-09-09

### fixed

- An interrupt no longer outlives the turn it stopped. `Kernel::interrupt` raises a flag that one
  `step` is spent putting down, which is what stops an interrupt landing between two checks from
  being lost - and there is one path where no step is ever spent on it: a `Provider` that watches
  `DeltaSink::is_interrupted` and hands back what it had honours the interrupt *itself*. The turn
  ends in `Finished` with the flag still up, and the next turn is the one that spends a step on
  it: it transitions nothing and returns the state it was already in.

  What that looks like from a client is a stop that eats the next message. Measured through
  `kamchatka` against a real endpoint: press stop mid-answer, type "say apple", and the message
  lands in the context with nothing answering it - the turn was consumed clearing a flag - while
  the message *after* it gets a reply. The context then holds a question nobody answered, and the
  screen shows an outcome carrying the previous turn's state, so nothing says why. Reaching
  `Finished` now puts the flag down, on the grounds that a turn which is over has nothing left to
  interrupt. The resting states an interrupt is actually for - `Ready`, `Deciding`, and `Idle`
  mid-loop, the ones another request would otherwise go out from - are untouched, so the window
  the single reader closes stays closed.

- `Usage::output_tokens` says whether the reasoning is inside it, and `Usage::reasoning_tokens`
  says it is a part of that number rather than a second one beside it. Documentation only - no
  behaviour here moves - but the absence was load-bearing: the field said "tokens in the response"
  and the dialects disagree about what that means. OpenAI's `completion_tokens` contains the
  reasoning, Google reports `candidatesTokenCount` and `thoughtsTokenCount` side by side, and a
  Google endpoint speaking the OpenAI dialect sends neither and leaves the thinking in the
  difference between a total and its parts. Three providers in this workspace read those three
  shapes into one field and settled the question three different ways, none of them written down,
  because nothing here had settled it for them. A figure that means something different depending
  on which endpoint answered is not a figure anybody can put beside another - which is the whole
  claim `Usage` makes by existing separately from `Kernel::budget`.

## [0.3.2] - 2026-09-08

### added

- `ModelResponse::thinking`, which reads what the model thought wherever it is recorded - the same
  accessor `ContextItem::thinking` has had all along, on the type a turn arrives as rather than the
  one it is kept as. A turn is recorded one of two ways - the `reasoning` field, or a
  `Block::Reasoning` among ordered blocks - and which one a caller gets is a property of whichever
  provider it happens to be talking to. Both `calls` accessors read both, and a context item reads
  both; a `ModelResponse` in hand was the one place left where asking what the model thought meant
  reading a field, and being right on one dialect only.

  An iterator of `Content`, because that is the shape of the two accessors beside it. Found while
  writing a conformance case asserting that a stream cut mid-thought keeps the thinking: it did
  keep it, in blocks, where the assertion was not looking.

### changed

- The `compare` example is `compare_models`. `nachalnik-eval` ships an example called `compare`
  too - it puts saved *runs* side by side where this one puts *models* - and two example targets
  with one name collide at `target/debug/examples/compare`, so `cargo build --workspace
  --examples` produced one binary where two were asked for and whichever built last won. Cargo
  says so and says it may become a hard error; it is a `cargo` warning rather than a `rustc` one,
  which is why `RUSTFLAGS: -D warnings` never caught it and CI has been green over it since
  `nachalnik-eval` landed.

  This one gives way rather than the other because the eval crate's example names are quoted in
  the write-up of a run, and a command in a paper that no longer exists is worse than a longer
  command here. `compare_models` also says which of the two things it compares, which the bare
  word never did once there were two.

### fixed

- A compaction pass that moved nothing leaves nothing behind. The checkpoint was already
  conditional on the pass having done something - and then the summary went in whether or not it
  had, and the condition on the checkpoint counted `summary.is_some()` as something done. A summary
  stands *in the place of* what was taken, which is what its documentation says, so a pass that
  took nothing has nowhere to put one. Left alone this compounds rather than annoys: the pass is
  asked again before the next request, the context is no smaller than it was, so it says yes again.
  Measured against a real endpoint with one pinned tool result over the target - the case
  `Trim`'s own note names - three requests produced three summaries, each saying an earlier tool
  result had been elided when none had, each spending one of the person's undos, and each growing
  the request 53 tokens: a compactor enlarging the context it exists to shrink, for as long as the
  session lasts.

- A pass may only take what the request is carrying, and the projection is what knows. The guard
  asked the item - `is_projected`, which is a question about the state - so an item the projector
  had repaired away passed it: `Active`, holding everything it holds, contributing nothing. A
  second result for a call that already has one is exactly that, and putting the whole of a
  truncated output back beside the copy the model was shown produces one. Eliding it recovers
  nothing while the report credits the pass with the whole of it (204 tokens, on a live run), and
  moves something the model was never being shown into a state nobody chose. `apply_compaction`
  already projects the context to get `tokens_before`; it now reads `included` from that same
  projection and answers both questions from it, which is the answer `Projection` was the one
  place holding.

- A `CompactionReport`'s two totals are the projected ones its documentation always said they
  were. They came from summing the items that were sending content, which drops an elided item
  from the total altogether and never charges for the marker put in its place - so a pass was
  credited with the whole of what it took away and nothing for what it left behind. On a plan
  eliding one 4,000-byte result the report said 5 tokens remained where a request made right then
  cost 27, the difference being the note in the projector's brackets; on a pass eliding twenty
  small results the arithmetic reports a decrease on a context it has made bigger.

  `Kernel::projected` had this right and said why in a note - "an elided item is a marker the size
  of a line where the item behind it may be ten thousand tokens" - and `apply_compaction` was the
  one place not going through it. Both now share a single `projection_tokens`, so the figure in a
  report can be held against the `Budget` that provoked it, which is the only reason a client is
  given both. `Event::ContextRecounted` keeps its item sums: recounting *is* about the items.

  It costs one projection of the context at each end of a pass, which happens at most once per
  request and moves `Content` by pointer.

- What a shortened tool result *is* outlives every state it passes through. `note` is documented
  as why an item is in its current state, and it is replaced whenever that changes - correctly,
  because a reason for being excluded stops being true the moment something is put back. The pair
  an output limit leaves behind was keeping a fact about its *content* in there: which item holds
  the whole of it. So a session that cycled both rows with `space` while trying to understand them
  lost the only sentence saying that one was a short copy of the other - destroyed by looking at
  it, and by the one gesture a person makes while looking.

  It goes in `included_because` now, which is why an item is in the context at all and which no
  state change touches. The archived half keeps its note as well, because "the model was shown a
  truncated copy" really is why *that* one is archived. Both fields say in their own docs which
  facts belong in which, since the answer is not obvious and getting it wrong is silent:
  `included_because` for anything that has to survive a state change, `note` for the state.

  `included_because` has been read out by `kamchatka`'s item view and by `Event::ContextAdded`
  since both existed, and was always empty because nothing in the kernel set it. This is the first
  thing that does.

- A second result for one call is not reported as a missing call. There are two ways for a tool
  result to fail to claim its call and the repair described both as the first: a call this
  projection does not carry is an orphan, but a call it *does* carry whose answer is already spoken
  for is a second result for it. That is not a fault - it is what restoring the whole of a
  truncated output beside the copy the model was shown produces, which is the intended way to
  send the whole instead, and the pairing then drops the short copy exactly as it should. Somebody
  who has just done that read `the call \`c1\` is not in the projection` about a call sitting on
  their screen. It now says `already has a result`, and `Skipped::reason` says
  `a second result for one call` rather than `an orphaned tool result`.

- A superseded item's note no longer repeats the state it is in. `Kernel::supersede` set it to
  `superseded by item 8` while the state was already `Superseded`, and a skipped item's reason is
  written as `{state}: {note}` - so every screen built on that read `superseded: superseded by item
  8`. The note is now `replaced by item 8`, which reads correctly composed and also on its own,
  which the shorter `by item 8` would not: a client listing an item on one line shows the note
  bare, and `by item 8` there says nothing.

## [0.3.1] - 2026-09-06

### added

- `Kernel::recalibrate`: the front door for `TokenCounter::recalibrate`, which recounts. A
  correction changes what is counted *from then on*, exactly as `observe` does, so applying one to
  a context that is already counted leaves every stored figure on the old scale while everything
  projected is on the new one - two budgets for the same bytes, which is what a snapshot's
  calibration exists to avoid. `Kernel::resume` sidesteps it by recalibrating before the items are
  counted; anything reading a `Snapshot::calibration` into a session that is already running had
  no such route and had to know the ordering rule. It recounts for the reason `set_counter` does,
  and does nothing at all for a counter that does not learn or a correction already in force.

- `Kernel::reserve_calls`, and the `Event::ToolCallsReserved` it announces. `Kernel::resume` has
  always taken `Snapshot::used_calls`, which covers a session picked back up in another process;
  the other way of reading a snapshot had nothing. A client that merges one *into* a session it is
  already running keeps the kernel it has, so the turns it pushes arrive carrying identifiers that
  kernel never issued - and the next response is then free to hand one of them back, with the
  repair having nothing to compare it against and the request that follows carrying the same
  `tool_call_id` twice. `kamchatka`'s `/load` is exactly that client, and it could not do anything
  about it from out there: a downstream crate needing a core change to do an ordinary thing is the
  sign of a seam that is not finished.

### fixed

- A compaction pass that moves nothing takes no checkpoint. Every other operation here has
  followed that rule and been tested for it; `apply_compaction` checkpointed before it knew
  whether the plan amounted to anything, so a pass whose every candidate was pinned or already
  elided spent one of the sixteen undos a person has. The sharper half is the redo: `checkpoint`
  discards the redo stack, so an undone change became unreachable - and since a `Compactor` is
  asked before *every* request, a compactor in that state took the redo away on every one of them
  for the rest of the session. The plan is now worked out before anything moves and the checkpoint
  is taken only if something will.

- `LinearProjector` holds a mid-turn item back under `send_blocks` too. The branch that sends an
  assistant turn as ordered blocks pushed its message and skipped the bookkeeping at the foot of
  the loop - which is the only place a turn's outstanding calls are counted and the only place the
  held items are flushed. So with that flag on, the repair released in 0.3.0 never fired at all,
  and an item pushed into the context while a turn was still collecting its results went out
  between the call and its answer: the request every OpenAI-compatible API refuses outright,
  naming the `tool_call_id` that went unanswered. Both shapes now fall through to the same
  bookkeeping, and a test drives the fixture through both.
- Eliding an assistant turn takes its thinking with its words. It did not, so a turn whose content
  had become a one-line marker went on costing every token the model had thought - eliding it
  freed nothing, `tokens_withheld` claimed those tokens were being kept from the model while they
  were still in the request, and a compactor would have watched the total refuse to move and
  elided it again. Measured on a turn with 4,000 bytes of reasoning, the projected budget did not
  change by one token. Under `send_blocks` it was worse than an accounting error: a signed
  thinking block went out beside a marker that is not the words it was signed over, which is the
  thing binding `Part::extra` to its block exists to prevent.
- `Calibrating` scales a wrapped counter's `count_message` instead of discarding it. It delegated
  `count_schema` and `count_item` and not this one, so the default implementation ran on the
  wrapper and counted the parts - and `count_message` is both the method the budget is counted
  over and the one a real tokenizer overrides, precisely to charge for the per-message framing an
  estimate cannot see. A counter charging ten tokens a message reported four for a message it
  counts as fourteen.
- `Selector` knows `state:elided`. The state a `Compactor` is told to prefer was the one state a
  client could not name: `state:elided` was a parse error, and a `Selector::State` holding it
  printed as a string that would not parse back.
- `Kernel::turn` cannot swallow an interrupt. Both it and the `step` it called cleared the flag,
  so one landing between the two checks was spent on a step that transitioned nothing - and the
  turn, handed back an ordinary resting state, went round and sent the next request anyway, with
  `turn.interrupted` already on the log saying it had been asked to stop. The step is now the only
  reader of the flag and reports back whether it acted on it.

## [0.3.0] - 2026-09-05

### added

- `ModelInfo::parameters`: the names of the `Params` a model accepts, where the provider publishes
  them. Empty means "not published", never "takes none" - a provider that says nothing is the
  common case, and reading its silence as a prohibition would invent a restriction it never
  stated. What it is for is the opposite mistake: a parameter set for a model that does not take
  it is accepted, sent and ignored in silence.

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
