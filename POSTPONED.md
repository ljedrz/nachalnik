# postponed, on purpose

Known and decided against *for now*, so that nobody spends an afternoon rediscovering them.
Each entry says what it is, why it waits, and what would unblock it - and where an entry has
been wrong before, it says that too rather than being quietly corrected.

Referenced from [AGENTS.md](AGENTS.md).

---

- **`nachalnik-mcp` carrying a picture rather than naming one.** The bridge answers an image block
  with `[an image (image/png), not carried into the context]`, which was the only thing it could do
  and is no longer. Carrying it is a few lines - a `Content::Blob` instead of a sentence - and the
  reason to wait was that a server offering a 4 MB screenshot would put 5.5 MB of base64 into a
  context whose budget could not count it. **The counter is in as of 0.4.0**, so the blocker is
  gone: a budget now says how many pieces it could not price, and `kamchatka`'s compactor takes an
  unpriced tool result first.

  **The blocker was never only the counter.** Neither dialect accepts a picture in a *tool result* -
  `tool` content is a string in one and a `functionResponse` in the other - so a `Content::Blob` in
  one goes out as the sentence naming it, the blob's own `Display`, deliberately, and `blobs.rs`
  pins that for both dialects.
  Carrying an MCP picture would therefore put megabytes of base64 in the context, send the model the
  same sentence it already gets, and make `Budget::uncounted` report one unpriced piece for a
  request whose actual content is a line of text. That is the budget naming a hole the request
  does not have, which is the thing the elided-item rule exists to prevent.

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
  *inside* the branch that read it - so while `/models` fetches a list, or the line after a
  `/model` or `/provider` waits in `App::submit` for the switch to settle, neither branch can be
  reached. A deadline falling in that window is
  served when the command returns. The model's own turns are interruptible, which is where a run
  spends its time, so the hole is narrow.

  What would unblock it is somewhere for a command to run that the loop can outlive. `App::submit`
  takes `&mut App`, so the obvious move - a `timeout_at` around it - would drop the future
  mid-command and leave a `/provider` half applied, which is a worse thing to leave a session than
  a late deadline. `/model` and `/provider` already run their switch as a task, `App::settling`,
  and that is half of the shape: the other half is that the wait for it, and for a command that is
  itself a request, has to be something the driver's `select!` can race against the deadline rather
  than an `await` at the top of `App::submit`. On the way out `App::wait_for_turn` already waits
  for a settling switch, under `LEAVING`, so a deadline does not strand one. What has to be decided
  first is what a deadline *means* for a command - whether it interrupts the request or merely
  stops what comes after it.

- **`--reconcile`: one context out of several hard forks of one session.** Two or more past
  snapshots that share an ancestor, folded into one session to carry on from. Nothing about it is
  blocked; it is not built. The design is written down here because most of it is decisions
  rather than code.

  It needs nothing in the runtime. `Snapshot` and `Kernel::resume` already are this, which is
  what the `fork` tool is built out of and what its own note says - so it is a
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
  state changes, so provenance cannot live there - the first `pin` would take it. `included_because`
  is already carrying the model's own reason for writing the note, and overwriting it would be
  rewriting the model's words. And `meta` never reaches the request at all: a `Reference` projects
  as `{label}:\n{text}`, so the label is the only field of an item a model reads without calling
  `context: look`. Prefixing the label (`a/plan`) buys visibility and breaks addressing -
  `label:plan` then names neither. So leave the label alone, stamp `meta` for the pane and for
  `look`, and push one **manifest** item at the divergence point: which ids came from which fork,
  and which labels are now carried by more than one. Said out loud because the resumed session
  cannot work it out, the way `fork`'s system message is - and it is the read-time counterpart of
  what `note` says at write time when a name is already taken.

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

  Against Landlock, `(deny network*)` covers UDP, so the module's "`no network` here means no TCP"
  is a Linux-only sentence. There is no `Partial` - a profile applies or it does not - so macOS
  answers `Full` or `Unavailable`, and `Unavailable` becomes a runtime check for
  `/usr/bin/sandbox-exec`, which doubles as the warning if Apple pulls it. `SYSTEM` needs a macOS
  twin: `/System` for the dyld cache, `/private/var`, `/Library`. And the profile is generated text,
  so a working directory holding a `"` is an injection surface that wants escaping and a test before
  the rest is worth having.

  `birdcage` covers both platforms and is the wrong fit: it confines the calling process, so the
  restriction leaks past the spawn, which is the opposite of the re-execution this program does
  deliberately.

  **The first step is done.** `tests/sandbox.rs` is `#![cfg(target_os = "linux")]`, so everything
  up to the spawn is in `tests/boundary.rs`, under `#![cfg(unix)]`, where the `macos-latest`
  column in CI runs it: the `Reach` rules the file tools obey, what a refusal names, the `~`
  refused in words rather than expanded, the arguments a confinement travels as, what the scratch
  directory may be made through, and which errors `Sandbox::note_for` will claim. It is
  `cfg(unix)` rather than nothing at all because those tests are written against `/usr` and
  `/etc`, and a root with no drive letter is not absolute on Windows.

  The portable half does not include *a command cannot write outside the working directory* or
  *a `curl` is refused*. Both are a spawned process being stopped, which off Linux nothing does;
  they are what is left in `sandbox.rs`, and a backend is what would check them.

  **The Mac binary was separable and is shipped.** It is a second entry in the `binary` job's
  matrix, `aarch64-apple-darwin` on `macos-latest`, through the `upload-rust-binary-action` the
  Linux one already used; the target is that runner's own host, so nothing cross-compiles.
  Gatekeeper gates it rather than the build: it is unsigned, so a download is quarantined until
  `xattr -d com.apple.quarantine`, which `kamchatka`'s readme says beside the link. Signing and
  notarising needs a paid Apple Developer account and two secrets in this repository, and that is
  what is still not done - a Homebrew tap is the other way to avoid the quarantine. What the
  binary does not have is any of the confinement above.

- **A second System One engine, `laya` among them.** The module is `nachalnik-providers::system1`
  and the variables are `KAMCHATKA_SYSTEM1_*` because the three question types are the *category's*
  rather than TypeSafe's - a claim to weigh, a closed set, an ordered rubric, under those names.
  What the module holds is still one HTTP client, `Jev`.

  The obvious candidate is [`laya`](https://github.com/NandhaKishorM/laya), which is open, has the
  same three primitives under the same names, answers quickly, and benchmarks itself against
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

  **A second engine is not a second struct beside `Jev`, because it is not a second *service*.** A
  library is reached by spawning a process, and this crate does not spawn processes, so a second
  implementation could never have sat beside `Jev` at all. That is why there is a trait:
  `system1::SystemOne` is the seam, and `kamchatka::advisor::Local` is on the other end of it,
  talking to `contrib/laya_advisor.py` over a pipe in the body `Jev` already sends.

  What is still not built is a laya HTTP *client*. What would unblock one is `laya` publishing an
  HTTP interface, or somebody standardising the body this workspace already sends.

- **Naming this program to OpenRouter when the *advisor* is what is calling it.** `Jev` sends no
  app headers, so a session that borrows its own key for `--advise` is attributed for the
  conversation and anonymous for the advice, out of the same account on the same service. The
  headers themselves are a solved problem - `OpenAiCompatible::on_behalf_of` and `filed_under`
  build them, and `kamchatka::endpoint` already holds the URL, the title and the categories to
  pass.

  What stops it being three lines is that `Attribution` is an inherent part of one client, and `Jev`
  is deliberately not a `Dialect`. Two unrelated clients now want the same pair of headers, and
  copying them onto the second would be the third copy of one thing in this workspace, which is what
  `is_openrouter` was consolidated out of. So what would unblock it is deciding **where the pair
  lives** now that it is not one client's business: a builder the crate offers, rather than a method
  each client grows.

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
  says so, to everybody, and the first line is still lost. Anybody who attaches two phones to one
  session meets this, and `RUNNING.md` names it in passing.

  There is nothing to unblock: what is missing is a decision about what several people driving one
  agent *means*. The cheap version is a queue instead of a slot, and it is cheap because the slot
  is one `Option<String>` on `App` - but a queue of messages into one turn is a different thing to
  be shown on a screen, and a second person's line arriving in the middle of the first person's
  thought is a conversation nobody has designed. The honest first step is smaller: say who typed
  what. Nothing on the wire carries a client identifier today, and every later answer needs one.

- **A client command that awaits the endpoint holds the whole session loop.** `remote::server`'s
  loop applies a command inside its own `select!`, and `App::submit` awaits: `/models` fetches a
  list, `/compact` works a pass out and then takes it, and a `/model` or `/provider` switch still
  in flight is awaited before the next line is read at all. While any of those is
  awaited the loop answers no other client, and one client typing `/models` at an endpoint that
  has gone quiet stalls everybody attached.

  **Nothing is lost while it waits.** Both loops read the kernel's broadcast while a command is in
  flight, because that is the one channel here that *drops* what nobody took: `App::trace` is
  built from what the loop read, and `Attached::trace` hands it to every client that attaches
  afterwards, so a lagged session would give everybody who arrived later a trace with holes in it
  and nothing would say so. Everything else queues. A connection waits in the listen backlog, an
  outcome in an unbounded channel and a `ctrl+c` in its own stream; all of them arrive late, none
  of them is dropped, and a client cannot tell the difference between the loop taking them early
  and taking them at the end.

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
  the projection: a message larger than the cap makes `Message::Attached` itself too long, and
  unlike a record a projection cannot be skipped. A client with no projection has nothing.

  So it wants abridging rather than naming, and that is the decision. A tool result already reaches
  a projection as its first lines, and a file or a note as one line naming it, but a message is
  `Line::text` whole, and clipping one changes what every client is handed - the browser, the
  gateway and `--connect` alike. `Message::Item` is already *the whole of what one
  context item says*, fetched on demand, so there is somewhere for the rest to live and the shape
  of the answer is not in doubt. What is in doubt is the number: a cap per line has to leave an
  ordinary conversation untouched and still hold when a session has a thousand lines in it, and a
  projection is one message however many lines are in it.

  Worth knowing for whoever picks this up: a file does not reach it. This entry once said `/attach`
  was the way in, and it is not: `/attach`, `-f`, `/note` and an embedder's `ContextItem::file` all
  read in a projection as one line naming the item. What reaches it is a message - one pasted at
  the desk, one in a session that was loaded, or one an embedder pushes. A client cannot send one,
  because its own line is held to `MAX_LINE` on the way in.

- **Serving a client older than the session, which is half of what `protocol::VERSION` promises.**
  The rule on the constant is that a session refuses a version it does not know and serves an older
  one it does. The first half is machinery - an attach naming a later version is refused, by that
  name, before anything else in the message is read. The second is not: `watermark` checks the
  number and does not keep it, so the moment `VERSION` is `2` nothing in a connection knows it is
  talking to a version-1 client and nothing can stop it being sent a version-2 message.

  There is nothing to unblock and nothing to do while this is `1`. The day the number moves is the
  day it is needed, and it is not the day anybody will be thinking about it. What it costs is the
  number kept per connection and every write in `remote::server::attend` asking about it - which is
  more than a field, because "a message this version lacks" is a fact about each variant that
  nothing declares today.

  What stands in meanwhile is `Message::Unknown` at the client, which makes an unrecognised message
  something a client survives rather than something that ends it, and `Attached::version`, which is
  how a client finds out what the other end speaks without being refused first. Neither is the rule:
  an unknown message is *counted* as an answer, because a client cannot tell one from a broadcast,
  and a client that leaves a beat early is the cost of that guess.

- **How many clients a session will accept.** Not bounded, and nothing refuses a connection.

  **Where the attach and leave lines go is decided: the trace.** `client N attached` and `client N
  left` went through `App::say`, so they were in `App::loose` and therefore in the conversation of
  every projection handed out afterwards. A browser reconnecting every second on a flaky link,
  which is what the relay's `retry: 1000` tells `browser.html` to do, filled the conversation with
  them until somebody typed `/cleanup`. They are trace lines now, the ring a thing that happens once
  a second belongs in, and `Attached::trace` carries them so every client still sees them.

  What is left is the count itself. A session will take connections until something else runs out,
  and there is nothing to say what "too many" is: a phone on a bad link is one client making a
  hundred connections, and five people watching one agent is five clients making five. Telling
  those apart is what a bound would have to do, and nothing on the wire carries a client identifier
  to do it with - which is the same thing the arbitration entry above needs first.

- **Knowing a command wants the network from its trying, rather than from its name.** Whether a
  shell command is judged against `net:reach` is decided by `reaches_the_network`, which looks the
  program up in `NETWORKED` - a list of names. It is a lexicon to maintain, it is wrong both ways
  (`git status` is asked about, a script that opens a socket is not), and it is the only reason a
  session with `exec:run` allowed is still asked about a command that never goes near the network.
  Reading subcommands would make the list longer and no less of a guess, so it is not the fix.

  What would be is the attempt itself. The sandbox already cuts TCP from a confined command with
  Landlock, but a refused `connect` is invisible from outside - Landlock denies it and tells nobody
  unprivileged. Seccomp user notification is the mechanism that sees it: the confined child, which
  already re-executes this program before running the command, installs a filter that holds any
  `socket()` for `AF_INET` or `AF_INET6` - the first thing any use of the network does, a DNS
  lookup included - and hands the listener to the parent. The parent asks the person once per
  command, then lets that call and every later one through, or refuses them all with `EACCES`. The
  filter reads only the call's integer arguments, so there is no address a command could change
  after the answer, and nothing is decided per destination. With it, `NETWORKED` goes on Linux: an
  allowed `exec:run` runs unasked, and the question comes only if the command actually reaches out.

  What it costs, and why it waits:
  - raw system calls for the filter and the notification `ioctl`s, which `#![deny(unsafe_code)]`
    in this crate rules out - so a small crate of its own, or a dependency that does it;
  - the listener passed from child to parent over a unix socket, since it is created in the child;
  - a question *during* a call, which nothing has today. Every permission question is asked in
    `Deciding`, before the call runs; this one arrives while the tool is executing, so the shell
    tool needs its own way to reach whoever answers, and a headless run answers it by `--on-ask`;
  - `io_uring_setup` refused by the same filter, since `io_uring` can open a socket without
    calling `socket()`;
  - and Linux only. Off Linux the shell already runs unconfined and nothing equivalent exists, so
    the list stays there - or the question is dropped there, since it guards nothing.

- **Saying that a refused `connect` was the confinement.** `Sandbox::note_for` accounts for a
  permission error a confined command hit, and it says nothing where every path the error names is
  one the session reaches - because such a refusal is normally the file's own permissions, and a
  hedge there sends a model looking for a boundary that had nothing to do with it. A socket under
  `/run` is now exactly that case and the reasoning no longer holds: the session can read it and
  cannot connect to it, so `docker ps` comes back `Permission denied` with nothing said about why.

  Knowing when to say it is not in the way. This entry once said it was - that the terminal's own
  process could not ask `confines_unix_sockets`, and the answer belonged to the child - but that
  function builds a ruleset and applies none, so any process can ask it, and the kernel it answers
  about is the one the child runs on; `tests/sandbox.rs` asks it the same way. Below 7.1 it answers
  `false`, and there the refusal really is the socket's own permissions, so the sentence belongs
  only where it answers `true`.

  What is left is reading a refusal as a `connect`. `Sandbox::reaches` is the reading half of the
  rule and a socket is held to the writing half, so the note wants a test of its own beside it: a
  named path that is a socket by its file type rather than by the wording of the error, outside
  every writable path, on a kernel that handles the right.

- **An `undo` across a change of counter.** `set_counter`, `recalibrate` and `recount` re-price
  the context and take no checkpoint. An `undo` after one that moved a figure puts back what the old
  counter gave, with no `context.recounted` to say so, and lists every item it re-priced as changed
  though nothing in the item did; a recount that moves no figure copies nothing, and `undo` does not
  see it. Whether a recount is an operation `undo` should see - and so a checkpoint, and the undo
  history it costs - or a fact that `undo` should re-apply on the way back is the decision.

- **Screenshots in the guide.** `kamchatka`'s readme shows the chat and context tabs, from
  `kamchatka/assets/`; the guide still carries no pictures, only prose about what each screen
  holds. A capture pasted in as text is a copy of one session's output: the program's words move
  on and the copy does not, and one edited by hand is a picture of no session at all. What
  unblocks it is real screenshots, saved as images beside those two, where the guide's prose now
  says what a screen holds.

- **Calling a partial ruleset confined.** On a kernel that enforces only part of the ruleset
  Landlock answers `Partial`, and below Linux 6.7 the part it drops is TCP: the files are held and
  the network is open. The permissions tab says "partly confined", SECURITY.md gives the kernel
  floor, and the `shell` description hedges with "TCP may be closed" - but a call says nothing, and
  a person reading "confined" on the status line of a 6.5 machine reads more than is there. The
  choice is between the word and the hedge: keep "confined" for `Full` and name what a `Partial`
  one leaves open, or leave the words and make the hedge a statement where the kernel is known.

- **`/save` writes with the umask.** A snapshot saved over a file somebody had made private comes
  back readable by whoever the umask allows, where the record the session writes itself is kept
  private. What waits is the rule: whether a save takes the target's existing mode, the record's
  mode, or the umask a person chose for their shell.

- **An extra writable path under `--deny fs:write`.** `--sandbox-allow` paths stay writable for
  `shell` when the policy denies `fs:write`, and `fs` is refused them, so the two tools disagree
  about one path. It is documented beside `Sandbox::of` and the screen draws the path read-write, so
  nothing is hidden. Moving the extra paths into the readable set when writing is denied, for the
  ruleset and for the shell's list of what it opened, is the fix if the two tools are to agree.

- **Which checks the record directory's privacy makes.** It looks at the mode bits and not at who
  owns the directory, which matters only to a process that can read past the bits anyway - root, or
  one holding `CAP_DAC_OVERRIDE` - and `std` has no way to ask for the uid. Windows has no ACL step
  and relies on `%TEMP%` being per-user. At most a line saying the check is unix's.

- **Three small things in the sandbox's accounts.**
  - `/dev` files are readable and `/dev` listings are not, so `note_for` can blame the boundary for
    a refusal that was the file's own permissions.
  - `reaches` and `Reach::allows` treat a leftover `..` differently (`/work/missing/../../outside`),
    which costs a hint in a contrived case; `reaches` refusing a `ParentDir` component settles it.
  - `Confinement::complaint` is public and nothing calls it. Removing it is a break of
    `kamchatka`'s library API, so it goes in a minor release.

- **A path rule about a link's name, not its target.** `grep` and `glob` judge a symlink by the
  name it has in the walk, as `fs read` does, because `Careful` matches names and a rule cannot see
  a link - so `alias -> .env` is not caught by `.env*`, and the sandbox is the boundary. The walk
  already holds the resolved target and could check it too; doing so for the walk alone would make
  it stricter than `read`, and doing it for both is a change to what a path rule is.

- **An `--allow-server` for a server this run does not start.** A server rule naming no server
  is refused, allow and deny alike, as a rule about a domain no tool declares already is. A settings
  file that allows a server is refused with it when the command line's `--mcp` replaces the file's
  list and leaves that server out. An unmatched allow grants nothing, so refusing only unmatched
  denies is the other reading, at the cost of the two rules no longer being held alike.

- **A refusal explained by the policy installed now.** A denied call's `why` is asked of whichever
  policy is installed when the explanation is written, so a `set_policy` while calls wait explains a
  refusal with the rules of a policy that did not make it. Keeping the deciding policy beside the
  call - an `Arc<dyn PermissionPolicy>` in `PreparedCall` - is the fix, and it touches core types.

- **Reads that span more than one lock.** `snapshot()` reads the context and then `last_seq` under
  separate locks, so an event that changes no item - an interrupt, a component setter - can land
  between them and be named by a snapshot whose items do not reflect it. A request is built from
  the tools, the projector and the counter read at separate moments; the counter is captured once
  now, and a concurrent swap of the tools or the projector can still mix versions. Both are
  closed by taking the reads under one lock, which is a change to the core's locking.

- **A pin named twice in one compaction list is reported twice.** A pin named in both lists being
  reported once per list is asserted on purpose; a pin named twice in the *same* list is reported
  twice as well. Deduplicating within a list is the change, if a report is meant to name each
  refusal once.

- **`n` above 1.** The core refuses no parameter, so a request asking for several choices has them
  merged. Parameters are the caller's to set and the runtime carries them verbatim, which is rule
  one, so this is written down rather than fixed: a caller that asks for alternatives gets what the
  provider does with them.

- **A reference's text is copied on every projection.** `LinearProjector` sends a reference as
  `{label}:\n{text}`, which builds a new string from the item's content each time the context is
  projected - the largest cost in a projection, and the one place it copies content the rest of the
  runtime shares. Not copying means sending the label as a block of its own, which changes what goes
  out in both dialects.

- **A generation counter for the context.** Several things project the same context twice with
  nothing between them to say it has not changed: `maybe_compact` and then `build_request`, and a
  client's frame, which builds `Going` and then asks for `Kernel::budget`. Reusing the first
  projection is unsound while the context can change in between, and nothing says whether it did.
  A number the context bumps on every change would make every one of them a cache, and it is a core
  addition for clients' sake - which is the reason it waits.

- **`--spend` and a lagging broadcast.** The `App` counts spend from the events it reads, and a
  broadcast it falls behind drops them, `model.finished` included - so a ceiling can be passed by
  whatever the lag took. Reading spend from the log, which drops nothing, rather than from the
  broadcast is the fix.

- **The model's `undo` against a person's later change.** The `context` tool's own undo puts an
  item back where the model had left it, and respects a pin, but not a person's later exclusion or
  edit of the same item - one after the model's `revise`, too. Its undo of a move over several
  states takes several kernel checkpoints, and its journal takes no operation lock under
  `--parallel`. What waits is the rule: whether the model's undo stops at anything a person did
  since, and what it says when it does.

- **Whose pin it is after a resume.** Which pins the model made is kept in memory and not in the
  snapshot, so after a resume every pin is the person's and the model is refused unpinning its
  own. That fails safe. Keeping it needs the pin's author written down, in `meta` for instance.

- **The model's undo history after a resume.** What `context`'s `undo` walks is kept by the process
  and not in the snapshot, so a resumed session has nothing of the model's to walk back, and a
  change it made before the restart comes back only by `restore`; the refusal says so. It is the
  question above for a second thing the tool remembers about its own changes: whether that
  journal is written into the snapshot, and what an entry naming an item a person has changed
  since means once it is read back.

- **`log` stops at the resume.** A resumed session's log begins at `session.resumed`, so `log read`
  cannot answer for anything before the restart, though the record written beside the snapshot
  holds all of it; `App::recall` reads that file for `context.replaced` and nothing else. Reaching
  further means the tool reading a file the kernel does not hold, and every answer saying which of
  its records came from there.

- **A headless run resumed already over its spend ceiling ends without saying why.** Nothing in
  its output names the ceiling, so the run reads as one that simply stopped.

- **`--connect` runs one answer into the next.** Two answers in a row can come out on one line in
  the connecting client's output. It is known and not yet fixed.

- **`/prune` and `/keep` in CONTRIBUTING and in `/help`.** CONTRIBUTING calls them undocumented and
  `/help` lists them, and `every_command_that_exists_is_in_the_help` requires every accepted name in
  the help. One of the two has to change, and which is a decision about whether those names are
  meant to be found.

- **Three small differences between the loops.**
  - With `--parallel`, streamed output from two calls interleaves on one line of the transcript.
  - `--headless` prints a `ToolRequested`'s arguments in full, and `--connect` through `one_line`.
  - `wiring::ended` asks whether the last record ended the session and `main::finish` whether any
    did; they differ only after a double `ctrl+c`, and one predicate would serve both.

- **`glob` counts past its cap.** A `glob` shows the first 200 paths and walks the rest of the tree
  to say how many there were, because the exact total is part of the answer. Stopping at the cap and
  saying "at least 200", as `grep` says there may be more, would make a glob over a large tree
  cheaper and give the model a floor instead of a count.

- **`app::when::read_off` returns an `Option` that is always `Some`.** A zone that cannot be read
  falls back to UTC and says so, so there is never a `None`. Returning `When` removes an arm that
  cannot run, and changes the signature of a public function.

- **Two runs of the live suite at once.** `live.rs` works in `live-{name}` directories under the
  target directory, so two runs against one `CARGO_TARGET_DIR` at the same moment clear each other's
  files. One test binary at a time is `common::scratch`'s rule; a target directory per run, or the
  process id back in that one name, is the way round it if concurrent runs are wanted.

- **No bound on an MCP server's first answers.** The handshake, `tools/list` and `resources/read`
  wait as long as the server takes, and a first `npx -y` can legitimately take minutes to download
  its package. A bound has to be long enough for that and short enough to mean something, and what
  a person sees while it runs is part of the same decision.

- **Schemas and names Gemini refuses.** Google's dialect rejects some JSON Schema an MCP server may
  send - `$ref`, `additionalProperties` - and a tool name that starts with a digit, so a server
  that works through the OpenAI dialect can fail a request through Google's. Checking needs a Google
  key; the fix is a translation of the schema on the way out, or a refusal at install that says why.

- **`structuredContent` and the blocks beside it.** A result that carries structured content is
  taken from it, and a non-text block beside it goes unnamed. The spec says `content` normally
  repeats it, and it is documented, so what is lost is only a block the structured half does not
  cover.

- **`Installed::remove_from` removes by identifier.** A tool installed since under one of the same
  identifiers is the one that goes, which the doc says. Removing only the very tool that was added
  needs `Arc` identity and `ptr_eq`, and the check and the removal would not be one step.

- **What `system1` does with a busy service.** Its retries ignore `Retry-After`, do not ask
  `out_of_quota` about a 429, and retry only 429 and 529 - the two statuses the service documents -
  where the dialects retry every 5xx. That fits a client answering a person at a permission prompt,
  who is better served by a quick failure than a long wait. Whether a 502 or 503 is worth one more
  try is the decision.

- **A stream silent before its headers is sent again.** A streamed request that has heard nothing
  for `PATIENCE` is retried up to `RETRIES` times, on the reading that a server which took the
  connection and went quiet is busy - and it is for a whole answer, whose headers come with its last
  token, that `may_have_been_heard` says otherwise. An endpoint that holds a stream's headers until
  its first token, as ollama does while it loads a model, can be asked, and billed, more than once.
  The choices are to keep it, to resend only a request that never connected, or to send an
  idempotency key where an endpoint takes one.

- **`Permits::unlimited()` has no caller.** It is published and coherent beside the bounded
  constructor, so it stays unless a minor release wants the surface smaller.

- **What `Trim` says and what it takes.** Its summary can count more than it elided when orphaned
  results are elided with the rest, and it elides a picture whatever the model has seen of it -
  including one the model has not yet been shown.

- **The examples' own copies, and one arm nothing reaches.** `compaction` and `transparency` keep
  their own `thousands` and line wrapper rather than using `examples/common`, because
  `transparency` says everything it shows is in its one file. `panel` handles `State::Deciding`,
  which its one tool, asking for nothing, can never reach; the arm is a defensive branch or dead
  code, with the unreachable `Deny` beside it.

- **A context the model's own turns have filled.** `Trim` takes only tool results, so at a small
  limit a session can reach a point where nothing is left for it to take: what remains is the
  model's turns and the tool schemas every request carries, the kernel refuses to send a request
  over the limit, and a headless run ends on "the last turn failed". A session that only talks
  gets there first. One way out is for compaction to elide the oldest assistant turns as a last
  resort, which changes what `Trim` promises and leaves the projector to repair any results
  whose call it took. The other is to give the model a request of its own to free room, which
  needs room held back for that request, since it is over the limit too.

- **Forks asked in one turn read each other.** `fork` copies the context as it stands when the
  call runs, and a turn's calls run one after another, so the second of two forks asked together
  is handed the first one's answer. `without` refuses an item that does not exist yet, so it
  cannot keep out a sibling that has not run. Leaving out results produced in the same turn by
  default is one fix; taking one copy for every fork of a turn, before any of them runs, is the
  other.

- **`fs read` has no range.** It takes a path and nothing else, so a file past the 32,000-byte
  output limit comes back cut, with the whole kept as an archived item, and a model that wants
  the rest reads it through `shell` with `sed -n`. An `offset` and a line count on `read` would
  answer that inside the tool and its policy, and would add to a description every request pays
  for; the decision is whether that toll is worth what `shell` already does.

- **A stream that fails after it has started loses what it said.** An `error` event after the
  answer has begun ends the turn with the error, and what had streamed - already handed on as
  deltas - is not kept, where a stream the transport cut off keeps it and stops as `cut off`.
  OpenRouter sends one when a stream stays quiet too long, as `Upstream idle timeout exceeded`,
  and its failover stops once part of an answer is out, so the error is final either way. Keeping
  the partial the way a cut-off is kept, with the server's sentence as the notice, is one choice;
  the other is that an answer the server disowned is not an answer.

- **The fuzzing and soak harnesses are not in the repository.** What drove `kamchatka` headless
  and served with a live model, mined the records for errors and checked them, and what soaked
  `nachalnik-providers` against OpenRouter through a fault-injecting proxy, all live outside it.
  Committed, a campaign could be run again after a change rather than rebuilt; the cost is a
  key, hours of wall-clock time, and Python beside a workspace whose scripts are shell, so where
  they go and in what language is the decision.
