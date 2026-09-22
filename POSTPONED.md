# postponed, on purpose

Known and decided against *for now*, so that nobody spends an afternoon rediscovering them.
Each entry says what it is, why it waits, and what would unblock it - and where an entry has
been wrong before, it says that too rather than being quietly corrected.

Referenced from [AGENTS.md](AGENTS.md).

---

- **`nachalnik-mcp` carrying a picture rather than naming one.** The bridge answers an image
  block with `[an image (image/png), not carried into the context]`, which was the only thing it
  could do and is no longer. Carrying it is a few lines - a `Content::Blob` instead of a sentence
  - and the reason to wait was that a server offering a 4 MB screenshot would put 5.5 MB of
  base64 into a context whose budget could not count it. **The counter is in as of 0.4.0**, so
  the blocker is gone: a budget now says how many pieces it could not price, and `kamchatka`'s
  compactor takes an unpriced tool result first.

  **The blocker was never only the counter, and this entry said it was.** Neither dialect
  accepts a picture in a *tool result* - `tool` content is a string in one and a
  `functionResponse` in the other - so a `Content::Blob` in one is flattened to
  `[image/png, N bytes]` on the way out, deliberately, and `blobs.rs` pins that. Carrying an MCP
  picture would therefore put megabytes of base64 in the context, send the model the same
  sentence it already gets, and - measured - make `Budget::uncounted` report one unpriced piece
  for a request whose actual content is twenty-four characters of text. That is the budget
  naming a hole the request does not have, which is the thing the elided-item rule exists to
  prevent.

  So what would unblock it is not a counter. It is a decision about **where a tool's picture
  reaches the model**, since the one place it cannot is where it currently sits: a picture has
  to be hoisted into a message that accepts one, which is a `Projector`'s business or a
  provider's, and neither has been asked. Nothing about the bridge changes until that does.

  Worth knowing for whoever picks this up: the kernel's projection carries the blob and the
  *provider* flattens it, so the kernel's budget is right about the request it built and wrong
  about the one that goes out. That is the only place in this workspace where those two differ
  in a way a figure can see.

- **A headless deadline that can cut short a command of the operator's own.** `--deadline` and
  `ctrl+c` are branches of the driver's `select!`, and a line read from the input is submitted
  *inside* the branch that read it - so while `/models` fetches a list, or `/model` and
  `/provider` finish a switch, neither branch can be reached. A deadline falling in that window is
  served when the command returns. The model's own turns are interruptible, which is where a run
  spends its time, so the hole is real and narrow.

  What would unblock it is somewhere for a command to run that the loop can outlive: `App::submit`
  takes `&mut App`, so the obvious move - a `timeout_at` around it - would drop the future
  mid-command and leave a `/provider` half applied, which is a worse thing to leave a session than
  a late deadline. The shape that works is the one `/model` already uses for its switch (a task,
  and `App::settling` awaited before the next line is read), applied to the commands that are
  themselves a request; what has to be decided first is what a deadline *means* for one - whether
  it interrupts the request or merely stops what comes after it.

- **`--reconcile`: one context out of several hard forks of one session.** Two or more past
  snapshots that share an ancestor, folded into one session to carry on from. Nothing about it is
  blocked; it is not built. The design was worked out and is written down here rather than in
  nobody's head, because most of it is decisions rather than code.

  It needs nothing in the runtime. `Snapshot` and `Kernel::resume` already are this, which is
  what `context: fork` is built out of and what its own note says - so it is a
  `Vec<Snapshot> -> Snapshot` in this crate and everything downstream is unchanged.

  **It is a common prefix, not a common subset.** Identifiers come from one `next_item` that never
  reuses a number, so two forks of one ancestor share ids 1..K with the same contents and then both
  allocate K+1 for different things. The longest agreeing prefix is a linear scan; if item 1
  already disagrees these are not forks of each other and it should refuse rather than concatenate.
  The one real conflict is an ancestor item a fork `revise`d - same id, different content - and
  `meta.revised` is what makes that detectable rather than a truncated prefix.

  **Identifiers are the easy half.** Renumber the tails, keep an old-to-new map per source, stamp
  `meta` with where each came from. Tool call identifiers need nothing: a result names its call by
  `ToolCallId` rather than by context id, so the pairing survives renumbering. `used_calls` is a
  union that dedupes the shared prefix on its own - and a genuine collision between two tails has
  to be a hard error, never a silent renumber, because a result paired with the wrong call is a
  lie about who asked what.

  **Do not merge the logs.** A snapshot says where things ended up and a log says what happened;
  two append-only logs interleaved are a record of something no observer saw. The reconcile is one
  event at the head of a fresh log, naming what was taken from where, and the source logs stay on
  disk.

  **Carry findings, not transcripts.** Keep the shared prefix whole and take from each tail only
  what the agent wrote down for itself - `Reference` items whose source is `agent`. Merging
  findings is coherent and merging transcripts is not: prefix plus tail A plus tail B is a
  conversation in which the work was done twice in an order that never happened. It also sidesteps
  signed reasoning, which some APIs require echoed back attached to exactly the turn it came from
  and which is invalid across models.

  **Two notes that contradict each other are both carried, labelled by fork.** Decided: a
  contradiction the model can see is what `conflict` in `nachalnik-eval` measures models on, and
  hiding it picks a side on the model's behalf.

  **Where that label goes is the part with a trap in it.** `note` is replaced whenever an item's
  state changes, so provenance cannot live there - the first `pin` would take it.
  `included_because` is already carrying the model's own reason for writing the note, and
  overwriting it would be rewriting the model's words. And `meta`
  never reaches the request at all: a `Reference` projects as `{label}:\n{text}`, so the label is
  the only field of an item a model reads without calling `context: look`. Prefixing the label
  (`a/plan`) buys visibility and breaks addressing - `label:plan` then names neither. So leave the
  label alone, stamp `meta` for the pane and for `look`, and push one **manifest** item at the
  divergence point: which ids came from which fork, and which labels are now carried by more than
  one. Said out loud because the resumed session cannot work it out, the way `fork`'s system
  message is - and it is the read-time counterpart of what `note` says at write time when a name
  is already taken.

  Calibration merges by summing `estimated` and `reported` and recomputing, which is right for two
  forks of one model and meaningless across two models, where it should be dropped - `Option` with
  `serde(default)` already makes "nothing learned" a valid state.

  Build the file-producing form first: `kamchatka reconcile a.json b.json -o merged.json`, then
  `--resume merged.json`. The operation is lossy and opinionated, so the constructed context wants
  reading before anything is sent to a model - the same argument `/request` is there for. A flag
  that does both is a convenience on top of it, not the primitive.

- **Reading a picture back out of a blob, anywhere.** `kamchatka` can now send one and still
  draws none, and that split is deliberate rather than unfinished: a terminal cell is not a
  pixel. It looks like a gap: an attached image is the one item in the context whose *content*
  nobody at this end can inspect. The context tab names it, `enter` on it names it, and the
  person's own knowledge of the file is the only account of what was sent. A client that renders
  is a different client, and the runtime already supports it; see `Blob::meta` and
  `pricing_a_picture.rs` for the half that is not rendering.

- **Confining the shell on macOS, and the Mac binary that would go with it.** Away from Linux
  `confine` answers `Unsupported` and the shell runs unconfined, which the readme says and
  `Confinement` says on the status line. It waits on two things: nobody here can test it, so CI
  would be the only thing that ever checked the boundary, and the one interface that could carry
  it is deprecated.

  macOS has one mechanism, Seatbelt - Apple's TrustedBSD MAC layer - and it is the right shape:
  kernel-level, unprivileged, path-scoped file rules and network rules. Two of its three doors are
  shut. `sandbox_init(3)` is the self-apply call and the true Landlock analogue: deprecated,
  private in the form that takes arbitrary SBPL, and `unsafe` FFI. App Sandbox entitlements are
  what the deprecation notice points at, need code signing, and sandbox the app rather than a
  child command. `sandbox-exec(1)` is deprecated in its own man page and still shipped and
  working; `apple/containerization#737` asks for a removal timeline and a replacement and is
  unanswered.

  A backend goes in `confine`'s `cfg(not(target_os = "linux"))` arm. The child this program
  re-executes would not confine itself; it would `exec sandbox-exec -p <profile> -- sh -c <cmd>`,
  which is no FFI, no `unsafe` and no dependency. Landlock's paths-with-rights map onto SBPL
  directly: `(deny default)`, `(allow file-read* (subpath ...))`,
  `(allow file-write* (subpath ...))`, `(deny network*)`.

  What differs. `(deny network*)` covers UDP, so the module's "`no network` here means no TCP" is
  a Linux-only sentence. There is no `Partial` - a profile applies or it does not - so macOS
  answers `Full` or `Unavailable`, and `Unavailable` becomes a runtime check for
  `/usr/bin/sandbox-exec`, which doubles as the warning if Apple pulls it. `SYSTEM` needs a macOS
  twin: `/System` for the dyld cache, `/private/var`, `/Library`. And the profile is generated
  text, so a working directory holding a `"` is an injection surface that wants escaping and a
  test before the rest is worth having.

  `birdcage` covers both platforms and is the wrong fit: it confines the calling process, so the
  restriction leaks past the spawn, which is the opposite of the re-execution this program does
  deliberately.

  `tests/sandbox.rs` is `#![cfg(target_os = "linux")]` at the file level, so the `macos-latest`
  column in CI is green while checking none of this. Splitting it is the first step, and this
  entry used to say the portable half included *a command cannot write outside the working
  directory* and *a `curl` is refused*. It does not: both of those are a spawned process being
  stopped, which off Linux nothing does - they are the whole of what is left here. What runs on
  both is everything up to the spawn, which is seven of the twenty-three: the `Reach` rules the
  file tools obey, what a refusal names, the `~` refused in words rather than expanded, the
  arguments a confinement travels as, and what `shell` says about itself before anything runs.
  Those want `#![cfg(unix)]` rather than nothing at all, because they are written against `/usr`
  and `/etc` and a root with no drive letter is not absolute on Windows.

  **The Mac binary is separable and is not blocked by any of it.** It is one matrix entry on
  `macos-latest` for `aarch64-apple-darwin` in the `upload-rust-binary-action` the Linux job
  already uses. Gatekeeper gates it rather than the build: an unsigned download is quarantined
  until `xattr -d com.apple.quarantine`, and signing and notarising needs a paid Apple Developer
  account and two secrets in CI. A Homebrew tap avoids quarantine.

- **A second System One engine, `laya` among them.** The module is
  `nachalnik-providers::system1` and the variables are `KAMCHATKA_SYSTEM1_*` because the three
  question types are the *category's* rather than TypeSafe's - a claim to weigh, a closed set, an
  ordered rubric, under those names. What the module holds is still one client, `Jev`, and there
  is deliberately no trait: a second engine would be a second struct beside it, and a trait with
  one implementor is a seam invented for a caller that does not exist.

  The obvious candidate is [`laya`](https://github.com/NandhaKishorM/laya), which is open, has the
  same three primitives under the same names, answers in ~33ms, and benchmarks itself against
  `jev-1.13.0` directly. **It is not a service.** It is a Python library - `pip install laya`, a
  `Router` with a `predict(state, questions)` method - and it publishes no HTTP API at all. So
  there is nothing to write a client against: the request shape a client would post does not
  exist yet, and inventing one here would be guessing at somebody else's interface and then
  shipping the guess.

  What is already there for it, and is the whole of what a third service takes today: an address.
  `Service::of` reads anything it does not recognise as keeping TypeSafe's paths, because that is
  the shape a self-hosted one has - so a shim in front of `laya` that answers `state` and
  `questions` at `/systemone` works through `Jev` with `KAMCHATKA_SYSTEM1_BASE_URL` and
  `KAMCHATKA_SYSTEM1_MODEL` set and no Rust written at all. Anybody wanting this before the
  upstream has a wire format should write that shim rather than a client.

  **The trait this entry argued against now exists, and the reason it was wrong is worth
  keeping.** It said a second engine would be a second struct beside `Jev` and that a trait
  would be a seam shaped around the only thing that fits it. What it missed is that the second
  engine is not a second *service*: a library is reached by spawning a process, this crate does
  not spawn processes, and so the second implementation could never have sat beside `Jev` at
  all. `system1::SystemOne` is the seam it needs instead, and `kamchatka::advisor::Local` is on
  the other end of it, talking to `contrib/laya_advisor.py` over a pipe in the body `Jev`
  already sends.

  What is still not built is a laya *client* - there is nothing to write one against, and a
  shim somebody runs is the honest answer while that is true. What would unblock one is `laya`
  publishing an HTTP interface, or somebody standardising the body this workspace already
  sends.

- **Naming this program to OpenRouter when the *advisor* is what is calling it.** `Jev` sends no
  app headers, so a session that borrows its own key for `--advise` is attributed for the
  conversation and anonymous for the advice, out of the same account on the same service. The
  headers themselves are a solved problem - `OpenAiCompatible::on_behalf_of` and `filed_under`
  build them, and `kamchatka::endpoint` already holds the URL, the title and the categories to
  pass.

  What stops it being three lines is that `Attribution` is an inherent part of one client, and
  `Jev` is deliberately not a `Dialect`: two unrelated clients now want the same pair of headers,
  and copying them onto the second is the third place in this workspace to write out the same
  thing - which is what `is_openrouter` was just consolidated out of. So what would unblock it is
  deciding **where the pair lives** now that it is not one client's business: a builder the crate
  offers, rather than a method each client grows.

  `KAMCHATKA_NO_ATTRIBUTION` has to cover both the day it does, and as one switch. Somebody who
  turned attribution off for their conversation has not agreed to be named by a second client on
  the same account, and two switches would be a way to be half off without noticing.

  Worth knowing for whoever picks this up: it is only ever half applicable. TypeSafe's own API
  keeps no ranking of the apps calling it, so a `Jev` pointed there has nothing to send and must
  not send it - `is_openrouter` is already the test for that, and it is the same test the request
  path uses.

- **Arbitration between clients attached to one session.** Every attached client may submit,
  interrupt and answer questions, and there is room for exactly one message queued into a running
  turn - so a second client typing during a turn silently takes the first one's place. The session
  says so, to everybody, which is the least it can do and is not the same as the line not being
  lost. Anybody who attaches two phones to one session meets this, and `RUNNING.md` names it in
  passing, which is the right disclosure in the wrong file.

  There is nothing to unblock: what is missing is a decision about what several people driving one
  agent *means*. The cheap version is a queue instead of a slot, and it is cheap because the slot
  is one `Option<String>` on `App` - but a queue of messages into one turn is a different thing to
  be shown on a screen, and a second person's line arriving in the middle of the first person's
  thought is a conversation nobody has designed. The honest first step is smaller: say who typed
  what. Nothing on the wire carries a client identifier today, and every later answer needs one.

- **A client command that awaits the endpoint holds the whole session loop.** `remote::server`'s
  loop applies a command inside its own `select!`, and `App::submit` awaits: `/models` fetches a
  list, `/model` and `/provider` finish a switch, `/compact` runs a whole compaction pass, and a
  switch still in flight is awaited before the next line is read at all. While any of those is
  awaited the loop accepts no connections, answers no other client, processes no kernel events and
  does not poll `ctrl_c`. One client typing `/models` at an endpoint that has gone quiet freezes
  everybody attached.

  **The half that was losing something is closed, and the paragraph above overstated the rest.**
  Both loops now read the kernel's broadcast while a command is in flight, because that is the one
  channel here that *drops* what nobody took: `App::trace` is built from what the loop read, and
  `Attached::trace` hands it to every client that attaches afterwards, so a lagged session gave
  everybody who arrived later a trace with holes in it and nothing said so. The other three do not
  lose anything. A connection waits in the listen backlog, an outcome in an unbounded channel and a
  `ctrl+c` in its own stream; all of them arrive late, none of them is dropped, and a client cannot
  tell the difference between the loop taking them early and taking them at the end.

  What is left is a client waiting for its turn, and it is not a queue anybody can add out here. It
  is `App::submit` taking `&mut App` for the length of a round trip, so answering one client while
  another's command is in flight would need two of the one thing there is one of. The fix is the
  same one the headless deadline above wants - somewhere for a command to run that the loop can
  outlive - and it is rejected here for the same reason: a future dropped mid-command leaves a
  `/provider` half applied. The shape that would work is `App::submit` splitting into what needs
  the session and what only needs an `Arc`, and the question to settle first is what an interrupt
  means for the half that is already in flight.

- **A projection larger than `protocol::MAX_LINE` makes a session unattachable.** The *record* half
  of this is closed: a record over the cap goes out as `Message::Oversized`, which names its
  sequence and its size, and the client takes that sequence as seen and carries on. What is left is
  the projection, and it was found by writing the test for the other half - a 33 MB context item
  makes `Message::Attached` itself too long, and unlike a record a projection cannot be skipped. A
  client with no projection has nothing.

  So it wants abridging rather than naming, and that is the decision: `Line::text` in a projection
  is the whole of what an item says, and clipping it changes what every client is handed - the
  browser, the gateway and `--connect` alike. `Message::Item` is already *the whole of what one
  context item says*, fetched on demand, so there is somewhere for the rest to live and the shape
  of the answer is not in doubt. What is in doubt is the number: a cap per line has to leave an
  ordinary conversation untouched and still hold when a session has a thousand lines in it, and a
  projection is one message however many lines are in it.

  Worth knowing for whoever picks this up: the item does not have to be attached by hand. Anything
  that puts a large file in the context reaches it, and the tool results that would are already
  bounded by `tools::CEILING` - so the way in is `/attach`, or an embedder pushing one.

- **Serving a client older than the session, which is half of what `protocol::VERSION` promises.**
  The rule on the constant is that a session refuses a version it does not know and serves an older
  one it does. The first half is machinery - an attach naming a later version is refused, by that
  name, before anything else in the message is read. The second is not: `watermark` checks the
  number and does not keep it, so the moment `VERSION` is `2` nothing in a connection knows it is
  talking to a version-1 client and nothing can stop it being sent a version-2 message.

  There is nothing to unblock and nothing to do while this is `1`, which is exactly why it is
  written down: the day the number moves is the day it is needed, and it is not the day anybody
  will be thinking about it. What it costs is the number kept per connection and every write in
  `remote::server::attend` asking about it - which is more than a field, because "a message this
  version lacks" is a fact about each variant that nothing declares today.

  What stands in meanwhile is `Message::Unknown` at the client, which makes an unrecognised message
  something a client survives rather than something that ends it, and `Attached::version`, which is
  how a client finds out what the other end speaks without being refused first. Both are honest and
  neither is the rule: an unknown message is *counted* as an answer, because a client cannot tell
  one from a broadcast, and a client that leaves a beat early is the cost of that guess.

- **A model switch that is in no record.** `/model` and `/provider` hand the new model to the
  `Dialect` the kernel already holds, and `wiring.rs` gave the kernel a clone of that same `Arc` - so
  the kernel's provider slot never changes and `Event::ModelChanged` is never emitted. The session
  is talking to something else and the log does not say so, which means a `/save` and a resume from
  it cannot say when the model changed either, and neither can anything reading the records
  afterwards.

  One case is now on the record and it is the first pick rather than a switch: a session started
  without `-m` is wired with no provider at all, so the `/model` that ends that calls
  `Kernel::set_provider` and the event says `from` nothing, `to` the model. Every switch after it
  is the paragraph above, unchanged.

  `Message::Model` covers the clients, which was the visible half: it is broadcast on a change, like
  `Message::Busy`, and for the same stated reason. What it does not do is make this a *state change*
  in the runtime's sense, which is the invariant the workspace holds itself to.

  What would unblock it is deciding where a switch belongs. `Kernel::set_provider` emits the event
  and computes `from` by asking the outgoing provider - so calling it with the same `Arc` after the
  fact reports `from` and `to` as the same model, which is worse than silence. The honest shapes are
  a `Dialect` that is replaced rather than mutated, so the kernel sees a new provider, or a way to
  tell the kernel that the one it holds now answers differently. The first is a change to how
  `App::provider` is shared; the second is API the runtime does not have and should be asked for
  carefully, because "the seam I am holding changed under me" is a door worth opening once.

- **How many clients a session will accept.** Not bounded, and nothing refuses a connection.

  **The half this entry was mostly about is decided and done.** `client N attached` and `client N
  left` went through `App::say`, so they were in `App::loose` and therefore in the conversation of
  every projection handed out afterwards - and a browser reconnecting every second on a flaky link,
  which `examples/browser.html` asks for with `retry: 1000`, filled the conversation with them
  until somebody typed `/cleanup`. They are trace lines now, which is the ring a thing that happens
  once a second belongs in, and `Attached::trace` carries them so every client still sees them. The
  question this entry posed - conversation or trace - has an answer.

  What is left is the count itself. A session will take connections until something else runs out,
  and there is nothing to say what "too many" is: a phone on a bad link is one client making a
  hundred connections, and five people watching one agent is five clients making five. Telling
  those apart is what a bound would have to do, and nothing on the wire carries a client identifier
  to do it with - which is the same thing the arbitration entry above needs first.

- **Saying that a refused `connect` was the confinement.** `Sandbox::note_for` accounts for a
  permission error a confined command hit, and it says nothing where every path the error names is
  one the session reaches - because such a refusal is normally the file's own permissions, and a
  hedge there sends a model looking for a boundary that had nothing to do with it. A socket under
  `/run` is now exactly that case and the reasoning no longer holds: the session can read it and
  cannot connect to it, so `docker ps` comes back `Permission denied` with nothing said about why.

  What stops it is that the note is written in the terminal's own process, which never applies a
  ruleset and so cannot ask `confines_unix_sockets` about the kernel that matters - the answer
  belongs to the child. Saying it unconditionally would be wrong on every kernel below 7.1, where
  the connect is not confined and the refusal really is the socket's own permissions.

  What would unblock it is the startup probe carrying the answer back. `available` already spawns
  the binary and reads one line out of it; that line naming the socket right as well as the
  ruleset status would give the parent the fact, at no extra process. It wants a shape that does
  not make `Confinement` mean two things at once.
