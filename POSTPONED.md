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

- **Whether `laya-serve`'s `confidence` is the one `Jev` reads.**
  [`laya`](https://github.com/NandhaKishorM/laya) is reached over HTTP: `laya-serve` answers
  `POST /v1/systemone`, and
  `KAMCHATKA_SYSTEM1_BASE_URL=http://127.0.0.1:8000/v1` beside any `KAMCHATKA_SYSTEM1_API_KEY`
  reaches it through `Jev` - RUNNING.md has the commands. The script that spoke to it over a pipe
  is gone, and with it what that script did to laya's answer: it built the answer rather than
  passing laya's through, because laya's `confidence` was its own quantity and read as this
  program's it drew every command yellow; and it refitted laya's temperatures on sixty labelled
  commands, raised its token budget and chose its checkpoint by script alone.

  `laya-serve` passes `predict()` through and says that is `Jev`'s shape. Whether its
  `confidence` is the one `Jev` reads has not been tried against a running server, and is the
  thing to check first. If it is not, the recalibration belongs upstream or in `Jev`, and which is
  the decision. The script and its sixty commands are in git history, last at `bcb9a2df`, for
  whoever takes the working upstream.

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

- **A projection larger than `protocol::MAX_LINE` makes a session unattachable.** The *record* half
  of this is closed: a record over the cap goes out as `Message::Oversized`, which names its
  sequence and its size, and the client takes that sequence as seen and carries on. What is left is
  the projection: a message larger than the cap makes `Message::Attached` itself too long, and
  unlike a record a projection cannot be skipped. A client with no projection has nothing.

  So it wants abridging rather than naming, and that is the decision. A tool result already reaches
  a projection as its first lines, and a file or a note as one line naming it, but a message is
  a `Line`'s `text` whole, and clipping one changes what every client is handed - the browser, the
  gateway and `--connect` alike. `Message::Item` is already *the whole of what one
  context item says*, fetched on demand, so there is somewhere for the rest to live and the shape
  of the answer is not in doubt. What is in doubt is the number: a cap per line has to leave an
  ordinary conversation untouched and still hold when a session has a thousand lines in it, and a
  projection is one message however many lines are in it.

  Worth knowing for whoever picks this up: a file does not reach it. This entry once said `/attach`
  was the way in, and it is not: `/attach`, `-f`, `/note` and an embedder's `ContextItem::file` all
  read in a projection as one line naming the item. What reaches it is a message - one pasted at
  the desk, one of the model's own answers with its reasoning, one in a session that was loaded, or
  one an embedder pushes. A client cannot send one, because its own line is held to `MAX_LINE` on
  the way in.

- **Serving a client older than the session, which is half of what `protocol::VERSION` promises.**
  The rule on the constant is that a session refuses a version it does not know and serves an older
  one it does. The first half is machinery - an attach naming a later version is refused, by that
  name, before anything else in the message is read. The second is not: `watermark` checks the
  number and does not keep it, so the moment `VERSION` is `2` nothing in a connection knows it is
  talking to a version-1 client and nothing can stop it being sent a version-2 message.

  There is nothing to unblock and nothing to do while this is `1`. The day the number moves is the
  day it is needed, and it is not the day anybody will be thinking about it. What it costs is the
  number kept per connection and every write in `remote::server::connection::attend` asking about it - which is
  more than a field, because "a message this version lacks" is a fact about each variant that
  nothing declares today.

  What stands in meanwhile is `Message::Unknown` at the client, which makes an unrecognised message
  something a client survives rather than something that ends it, and `Attached::version`, which is
  how a client finds out what the other end speaks without being refused first. Neither is the rule:
  an unknown message is *counted* as an answer, because a client cannot tell one from a broadcast,
  and a client that leaves a beat early is the cost of that guess.

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

- **An `--allow-server` for a server this run does not start.** A server rule naming no server
  is refused, allow and deny alike, as a rule about a domain no tool declares already is. A settings
  file that allows a server is refused with it when the command line's `--mcp` replaces the file's
  list and leaves that server out. An unmatched allow grants nothing, so refusing only unmatched
  denies is the other reading, at the cost of the two rules no longer being held alike.

- **A `/endpoint` that keeps the model name is in no record.** The kernel announces a switch by
  comparing the `ModelInfo` a provider reports, and a `ModelInfo` carries no address, so
  `/endpoint URL` with no model leaves the record saying the session never moved; only the line the
  command printed says otherwise. `/endpoint URL MODEL` is recorded, because the name changes.

  It waits because every fix costs something. An `endpoint` field on `ModelInfo` is the honest one
  and a break, since the struct is not `#[non_exhaustive]`; a field on `Event::ModelChanged` is a
  break too; and folding the address into the provider's `provider` label puts a URL where a name is
  documented and drawn. What would settle it is the next release that may break `nachalnik`: mark
  `ModelInfo` `#[non_exhaustive]` and give it the address in the same one.

  It matters more now that `-r` carries on with the model the record names: a session moved by
  `/endpoint URL` alone resumes that model at whatever address the flags give, and the record gives
  no hint that it had moved.

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

- **The model's `undo` in the person's undo history.** The `context` tool's own undo of a move over
  several states takes one kernel checkpoint for each state and note it puts items back into,
  because `set_state` moves items into one state at a time and the kernel has no operation that
  moves several at once - so walking back one change of the model's can cost the person several
  undos. What would unblock it is an operation in the core that moves items into several states
  at once.

- **The model's undo history after a resume.** What `context`'s `undo` walks is kept by the process
  and not in the snapshot, so a resumed session has nothing of the model's to walk back, and a
  change it made before the restart comes back only by `restore`; the refusal says so. Writing the
  journal into the snapshot is the change, and an entry read back from one meets the rule a live
  one does: an item that no longer looks the way the entry left it is left alone.

- **`log` stops at the resume.** A resumed session's log begins at `session.resumed`, so `log read`
  cannot answer for anything before the restart, though the record written beside the snapshot
  holds all of it; `App::recall` reads that file for `context.replaced`, and `-r` for the model the
  session was talking to, and nothing reads it for anything else. Reaching
  further means the tool reading a file the kernel does not hold, and every answer saying which of
  its records came from there.

- **Two calls' streamed output on one line.** With `--parallel`, the output two running calls
  stream interleaves on one line of the transcript, because `Event::ToolOutput` is appended to
  whichever line is open and the transcript keeps only one open. Keying the open line by call is
  the change, and it reaches `caught_up`, which drops every streamed line as soon as any one call's
  result is in the context.

- **Two runs of the live suite at once.** `live.rs` works in `live-{name}` directories under the
  target directory, so two runs against one `CARGO_TARGET_DIR` at the same moment clear each other's
  files. One test binary at a time is `common::scratch`'s rule; a target directory per run, or the
  process id back in that one name, is the way round it if concurrent runs are wanted.

- **No bound on an MCP server's first answers.** The handshake, `tools/list` and `resources/read`
  wait as long as the server takes, and a first `npx -y` can legitimately take minutes to download
  its package. A bound has to be long enough for that and short enough to mean something, and what
  a person sees while it runs is part of the same decision. Each server is named on standard error
  before it starts, and `--deadline` holds a headless run to the person's own bound; a session with
  a screen waits.

- **Schemas and names Gemini refuses.** Google's dialect rejects some JSON Schema an MCP server may
  send - `$ref`, `additionalProperties` - and a tool name that starts with a digit, so a server
  that works through the OpenAI dialect can fail a request through Google's. Checking needs a Google
  key; the fix is a translation of the schema on the way out, or a refusal at install that says why.

- **The fuzzing harness and the provider soak are not in the repository.** What drove
  `kamchatka` headless and served with a live model and mined the records for errors, and what
  soaked `nachalnik-providers` against OpenRouter through a fault-injecting proxy, live outside
  it. The soak of `kamchatka` itself is in: `.claude/skills/soak/` carries one session across
  resumes, kills and a context wall and checks the chain of records it leaves. Committed, the
  other two could be run again after a change rather than rebuilt; the cost is a key and hours of
  wall-clock time, and where they go is the decision. Python is not new here - two test servers
  and both skills are Python - but `scripts/` is shell, and a campaign that finds
  errors rather than reviews code is a different thing from the sweep.

- **A bare file name is a domain, not a path rule.** `--deny b.txt` is refused, and told to write
  `b.txt*`, because a domain, a tool's id and a file name are all bare words. Reading a bare word
  no domain claims as a path rule would make a typo like `--deny contextt` a rule about a file that
  quietly matches nothing, so it waits for a spelling that is unambiguous either way.

- **`fs write` makes no directories.** A write into a directory that is not there is refused and
  names the directory, and a model with `exec` refused has no way to make one. Making the missing
  parents beneath the reach is a change to what `fs:write` can do, and so a question for the
  permission table rather than for the tool.

- **OpenRouter's `reasoning_details` is neither read nor sent back.** A model whose thinking is
  carried only there loses it, and one that signs its thinking across tool-call turns gets its
  turns back without it. Nothing tested here needed it; a model that does, and a live test that
  shows the difference, would say whether it belongs in the dialect.

- **A turn paused by `--requests` is not in the record as a pause.** The kernel returns at the
  request budget with the machine `Idle`, and the records of that turn read exactly as those of a
  turn the model ended in as many requests, so a log read afterwards cannot tell them apart. A
  `turn.stopped` event would say it, and is additive since `Event` is `#[non_exhaustive]`; it is a
  new part of the runtime's record, which is why it waits for a person rather than a release week.

- **Two endings the record does not say plainly.** A termination signal between `model.requested`
  and the calls' results writes a snapshot holding a call that never ran, which resume repairs; and
  a turn whose answer asked for a tool and named none ends as `tool_use`, as though it had called
  one. Both want the same decision as the entry above: what the record should say about a turn that
  did not end the way its last event suggests.

- **A heuristic refusal under `--no-sandbox` is described as a person's.** With no confinement,
  `net:reach` is judged from a command's name; a command that trips it under `--on-ask deny` is
  refused as though somebody had been asked, and the model is told a different approach may be
  allowed, though the rule will refuse it for the rest of the session. Saying which subject was
  asked about means letting a policy name it in `PermissionPolicy::why`, a `nachalnik` change.

- **A compressed answer is refused, not read.** An endpoint that compresses its body despite not
  being asked gets a sentence naming the encoding. Reading it means reqwest's `gzip` feature, one
  more dependency in a crate that rations them.

- **A `y` typed at `--connect` before its question arrives.** At a terminal it goes to the model as
  a message, and the question waits for the next one. A pipe does not meet this, because a piped
  client reads its next line only once the turn is over or a question is open. Holding a bare
  letter at a terminal until a question comes, and dropping it if none does, is the change, and
  it makes `y` mean something different depending on when it was typed.

- **A record that shows it has not been edited.** Each record could carry a hash of the one before
  it, so that `--check` could say a log is unedited from its first record to its last. That says
  "unedited", not "authenticated": whoever edits a record can recompute every hash after it, and a
  key to sign with is something this workspace does not hold. It is cheap to add on top of `FORMAT`
  and `Record::format`, because the field is additive, and it waits because it is worth less than
  what `--check` already reads: a reader that is not this program, and a record that says what went
  wrong with it.

- **A session's roots are resolved on every check.** `Reach` canonicalises the working directory
  and every `--sandbox-allow` and `--sandbox-read` root each time it judges a path, which a `grep`
  over a large tree pays per file and per link. Resolving them once would move when a root is read
  from when it is used to when the session starts, so a root that appears, moves or is relinked
  during a session would be judged by what it was; that is a change to the boundary, not a speed-up.
