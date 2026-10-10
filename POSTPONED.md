# postponed, on purpose

Known issues and ideas deliberately left for later, written down so nobody wastes time rediscovering
them. Each entry says what it is, why it waits, and what would unblock it.

Only what is still open belongs here. No finished work, no record of what was tested or checked,
no history of how an entry got to where it is: those go in the commit message. The fix that closes
an entry removes it; one that closes part of it leaves only the part that is left.

---

- **`Intervention::elided` brings back an item that was already excluded.** It sets the named items
  to `Elided` whatever state they were in, so an item the session had excluded comes back into the
  treated copy as a marker the control never had, and the record lists it as touched - an
  intervention that added something while claiming to take it away. Nothing in the suite does
  this, since every experiment elides an item it has just put in. Skipping such an item, or
  reporting it apart from the ones moved, changes what `Applied` says; unblocked by choosing
  which.

- **An MCP tool's picture never reaches the model in `kamchatka`.** The bridge carries a picture a
  tool returns as a `Content::Blob`, up to 5 MB of base64, and declares no output limit, so
  `kamchatka`'s floor for tools without one - 32,000 bytes, there so that somebody else's server
  cannot fill the context - applies. `Content::truncate_to` measures a blob by its base64 and
  turns a result over the limit into text, which names the picture: any screenshot is replaced by
  `[image/png, …]` and a note that bytes were cut, which asking for less cannot help. Each half is
  deliberate, and together they undo what both changelogs say the bridge does. The bridge's own
  bound as its limit raises the ceiling for all of an MCP tool's text as well; a ceiling of its own
  for pictures, or a truncation that keeps blobs whole and cuts only the text around them, changes
  what a tool's limit means. Unblocked by choosing which.

- **A headless run goes on sending after a length refusal from an endpoint that publishes no
  limit.** `App::oversized` is the refusal and the measurement together: the refusal is any
  `Overrun`, and the measurement compares the budget with the limit the endpoint published. A
  provider that refused for length and published no limit gives an `Overrun` with no limit, and a
  budget with none, so the conjunction is false in the one state it was written for - every
  remaining line is sent and refused, the lines are never passed over unsent, and the person is
  told no figure to act on. Remembering the overrun and comparing the budget with its own token
  count where there is no limit would close it; it changes when a headless run stops sending, so it
  waits for a decision on that.

- **`--connect` to a listener that accepts and then closes retries for a minute.** A first attempt
  that cannot connect fails at once, and one that connects and cannot send `attach`, or reads the
  connection closing before anything came back, is taken for a dropped session: the client retries
  for `GIVE_UP` and then says the session had 0 records when it was last seen, which it never was.
  The write fails only when the peer has closed before it, so a fix needs the first-read path in the
  loop as well as that write, and a test needs a peer that closes at a chosen moment. Unblocked by
  writing that peer.

- **Two `fork`s in one turn under `--parallel` can both call their copies the same context.** The
  copy before is found by whichever fork takes the `last_read` lock first, and the comparison is one
  way, so where a call writing to the context lands between the two snapshots and the later fork
  locks first, neither says the two differ. Taking the lock before the snapshot would make the lock
  order the snapshot order; what is missing is a test that can order the two forks, since the kernel
  runs parallel calls on a `JoinSet`.

- **A refused attach still displaces the client that was attached.** `Serving::answer` seats a
  client before the attach is answered, and `watermark` refuses a projection too long for one frame
  only after that, so the client there before is let go and the newcomer is refused - nobody is
  attached. A projection that long is rare, since the conversation is cut down to fit; seating
  after the size check means a second exchange in the attach, or the check inside the server.
  Unblocked by choosing which.

- **A client that falls behind on the program's own lines can be left with the wrong state.** The
  session says `Busy`, `Model` and `Reaching` only when they change, and a client more than 256 of
  these lines behind is told only how many it missed. If one of the lost lines was one of those
  three, nothing says it again: a piped `--connect` can wait for a `busy: false` that never comes,
  or miss a network question. A resume already sends the standing state, and a lag could do the
  same, from the server or by the client resuming. Unblocked by choosing which, and by a client
  that falls that far behind.

- **A Gemini stream that carries no candidate is a turn that said nothing.** A 200 stream of
  `usageMetadata` alone is read as a finished, empty turn with `EndTurn`, where Chat Completions
  refuses a stream that never answered. The unstreamed path counts `usageMetadata` as an answer on
  purpose, and Google is not known to send this shape. Unblocked by deciding what counts
  as an answer from this dialect, or by a response that shows the shape.

- **Argument fragments streamed before a call's identifier are announced under an empty one.**
  `Delta::ToolArgs` is sent with the identifier the call has at that moment, so a client drawing
  calls as they stream shows the opening fragments under no identifier and the rest under the real
  one. What is recorded is right. Unblocked by a client that needs it; holding the fragments until
  the identifier comes, or keying deltas on the call's position, are the two ways.

- **A reset HTTP/2 stream is not retried.** `worth_waiting_out` retries a request that never got
  an answer only when it timed out, on the reasoning that every other transport failure is a
  decision or a bug. A server resetting the stream - `http2 error: stream error received:
  unspecific protocol error` - is neither, and a turn fails on it. So
  does `error decoding response body` partway through an answer, which is not retried because the
  answer may already be billed. Unblocked by telling a reset before any of the answer arrived from
  one after it.

- **Two assertions in `kamchatka/tests/introspect/fork.rs` cannot fail.** They check that a reply
  does not say "Nothing of yours was taken away", a sentence the code no longer writes, so they hold
  whatever `fork` says. Pointed at the sameness claim instead they contradict
  `two_forks_with_nothing_writing_between_them_still_say_they_are_the_same_context`, which pins
  that claim on purpose. Unblocked by saying what they should guard, or removing them.

- **`/restart` starts the `--spend` count again.** `--spend` stops "the session" once that much
  has been charged for it, and `/restart` starts a fresh session in the same process, so the total
  goes back to nothing and a run already at its ceiling can spend it again; `/spend` meanwhile says
  what "this run has spent". A script that pipes `/restart` is bounded per session, not per run.
  Unblocked by deciding which the ceiling is for: carrying the total and the stop across a relaunch,
  or saying "this session" where `/spend` says "this run".

- **A headless or served run stopped short after its last turn failed exits 1.** `Headless::run`
  and `Server::run` answer `Err` whenever the last turn failed, and the stop - a deadline,
  `ctrl+c`, a signal - is read only on the `Ok` path, so a run whose last turn failed and which was
  then stopped exits 1 rather than 124, 129, 130 or 143. An interrupted turn ends as a stop rather than a failure, so it takes a turn that
  failed on its own. Unblocked by choosing which of the two a script is told: the stop, under the
  rule that the first cause wins, or the failure.

- **Chat Completions names a tool's image instead of sending it.** A tool message there is a
  string, so an image in a tool result goes out as a line naming it. The dialect could move the
  picture to where its API takes one - a user message after the tool messages - without changing
  the context. Unblocked by a model that needs it, and a live test showing it reads the picture
  once moved.

- **Anthropic's cache loses everything after an edit that isn't near the end.** The provider marks
  two cache points, the system prompt and the end of the request, and Anthropic looks back only
  20 blocks from a mark for an earlier entry. A session that edits its own context, as a sweep's
  does, falls back to the system prompt after an edit anywhere earlier and writes the rest again,
  at more than the price of reading it. The API takes four marks, so two are unused, but where to
  put them is a trade-off: marks that follow the turns or a fixed grid of block positions catch
  different edits, each mark is a cache write of its own, and blocks are counted after thinking
  and elision change them. Unblocked by a live comparison of placements on a session that edits
  its context, measuring what is read and written.

- **Serving clients older than the session, which `protocol::VERSION` promises.** A session should
  serve the older protocol versions it knows, but `watermark` checks a client's version without
  storing it, so once `VERSION` is `2`, nothing knows a connection is a version-1 client, and
  nothing stops it being sent version-2 messages.

  Nothing is needed while the version is `1`. When it changes, each connection has to remember its
  version, and every write in `remote::server::connection::attend` has to check it, which means each
  message type must declare the version it was added in. Meanwhile, `Message::Unknown` lets clients
  survive messages they don't know, and `Attached::version` tells them what the server speaks.

- **`undo` after the token counter changes.** `set_counter`, `recalibrate` and `recount` re-price
  the context without creating a checkpoint. An `undo` after one that changed a figure restores the
  old counter's figures, with no `context.recounted` event saying so, and reports every re-priced
  item as changed even though nothing in it changed; a recount that changes nothing copies nothing,
  and `undo` doesn't notice it. The open question is whether a recount should be an undoable
  operation (with a checkpoint and the undo history it uses) or something `undo` re-applies.

- **`compaction::Removed` is the type of every item a compaction report names.** The elided items,
  the ones the kernel refused to touch and the summary that was added are each a `Removed`, so a
  report's type says the opposite of three of its four fields, against the rule of one word per
  mechanism. Its doc says what it is. Renaming it is a breaking change to `nachalnik`, so it waits
  for the next release that breaks anything; a type alias for the old name would ease it.

- **`Kernel::set_full_notice` is announced by nothing.** It is the one component setter with no
  event, so a log reads the notice only once one is placed, as a `context.added`, and never says
  which notice was set, changed or taken away before then. Announcing it needs a new event kind -
  a change to the record every reader of the log meets - and whether to carry the notice's text
  or only that one is set is the open question. Unblocked by a client that needs to tell from the
  log what notice a session had.

- **One resource that can't be read loses `Server::resources` the whole listing.** A server
  refusing one read makes the call an error, and the resources that read fine are not returned.
  Naming the failure in place, as a resource with no text is named, would also turn a server that
  has died into a list of failures and an `Ok`, unless the two kinds of error are told apart.
  Unblocked by a caller that needs a partial listing.

- **`--allow-server` for a server this run doesn't start.** A server rule naming a server that isn't
  running is refused, for both allow and deny, just as rules for a domain no tool uses are. A
  settings file that allows a server is refused along with it when `--mcp` on the command line
  replaces the file's server list and leaves that server out. An unused allow grants nothing, so
  the alternative is to refuse only unused denies, at the cost of treating the two differently.

- **Reference text is copied on every projection.** `LinearProjector` sends a reference as
  `{label}:\n{text}`, building a new string from the item's content every time the context is
  projected. That's the largest cost in a projection, and the one place content shared everywhere
  else gets copied. Avoiding it means sending the label as a separate block, which changes what is
  sent in every API.

- **A version counter for the context.** Several things project the same context twice in a row
  with nothing to say it hasn't changed: `maybe_compact` and then `build_request`, and a client's
  frame, which builds `Going` and then calls `Kernel::budget`. Reusing the first projection isn't
  safe while the context can change in between. A number the context increments on every change
  would make all of these cacheable, but it's an addition to the core only for clients' benefit,
  which is why it waits.

- **The model's `undo` in the person's undo history.** When the `context` tool undoes a change that
  moved items into several states, it creates one kernel checkpoint per state and note, because
  `set_state` moves items into one state at a time and the kernel has no operation that moves them
  into several at once. So undoing one change by the model can take the person several undos.
  Unblocked by a core operation that moves items into several states at once.

- **The model's undo history after a resume.** The history `context`'s `undo` uses is kept in memory,
  not in the snapshot, so a resumed session has nothing for the model to undo, and a change made
  before the restart can only be reverted with `restore`; the refusal says so. The fix is writing
  the history into the snapshot. Entries read back from it would follow the same rule as live ones:
  an item that has changed since the entry was recorded is left alone.

- **`log` stops at the resume.** A resumed session's log starts at `session.resumed`, so `log read`
  can't show anything before the restart, even though the record saved next to the snapshot has all
  of it. `App::recall` reads that file for `context.replaced`, and `-r` for the session's model, but
  nothing else uses it. Going further means the tool reading a file the kernel doesn't hold, with
  every answer saying which records came from it.

- **No time limit on an MCP server's startup.** The handshake, `tools/list` and `resources/read`
  wait as long as the server takes, and the first `npx -y` can legitimately take minutes to
  download its package. A limit has to be long enough for that and short enough to be useful, and
  what a person sees while waiting is part of the same decision. Each server is named on stderr
  before it starts, and `--deadline` limits a headless run; a session with a screen just waits.

- **A bare file name is a domain, not a path rule.** `--deny b.txt` is refused with a suggestion to
  write `b.txt*`, because a domain, a tool id and a file name are all bare words. Treating a bare
  word that isn't a domain as a path rule would turn a typo like `--deny contextt` into a rule about
  a file that silently matches nothing, so this waits for a syntax that's unambiguous.

- **`--allow .env` doesn't stop the question about `.env`.** The default rule `.env*` asks, every
  path rule that matches a call is consulted, and the strictest answer wins, so an allow for the
  exact file never beats the broader ask; `--allow '.env*'` does, because it replaces the default.
  Capability rules don't work this way: there `--allow fs:read` decides over an ask for `fs`. It
  fails safe, but the rule is accepted and nothing says it changes nothing. Saying so at
  startup, refusing the rule with the pattern that would work, or letting an exact path's allow
  decide over a default pattern are all changes to what a rule means, so it waits for a decision
  on how path rules combine.

- **`fs write` doesn't create directories.** Writing into a directory that doesn't exist is refused
  with the directory's name, and a model without `exec` can't create it. Creating missing parent
  directories inside the allowed paths would change what `fs:write` allows, so it's a question for
  the permission rules, not the tool.

- **OpenRouter's `reasoning_details` is ignored.** A model whose reasoning only appears there loses
  it, and one that signs its reasoning across tool-call turns gets its turns back without it.
  Nothing tested so far needed it; a model that does, with a live test showing the difference,
  would show whether the OpenAI client should support it.

- **Under `--no-sandbox`, a refusal by name-matching is presented as a person's decision.** Without
  the sandbox, `net:reach` is judged from the command's name. A command refused that way under
  `--on-ask deny` is reported as if someone had been asked, and the model is told another approach
  might be allowed, even though the rule will keep refusing for the rest of the session. Saying
  which rule applied means letting a policy name it in `PermissionPolicy::why`, a change to
  `nachalnik`.

- **Compressed responses are refused, not decompressed.** An endpoint that compresses its response
  without being asked to gets an error naming the encoding. Supporting it means reqwest's `gzip`
  feature, one more dependency in a crate that keeps them to a minimum.

- **A `y` typed into `--connect` before its question arrives.** At a terminal it's sent to the model
  as a message, and the question waits for the next one. Piped input doesn't have this problem,
  because a piped client only reads its next line once the turn is over or a question is open.
  The fix would be holding a lone letter typed at a terminal until a question arrives and dropping
  it if none does, but then `y` means different things depending on when it was typed.

- **A record that proves it hasn't been edited.** Each record could include a hash of the previous
  one, so `--check` could confirm a log is unedited from start to finish. That proves "unedited",
  not "authentic": whoever edits a record can recompute all the hashes after it, and this workspace
  has no signing key. It would be cheap to add on top of `FORMAT` and `Record::format`, since the
  field would be additive; it waits because it's worth less than what `--check` already does: work
  for readers other than this program, and records that say what went wrong.

- **A confined command isn't told its `$TMPDIR` is writable.** Every command is handed a scratch
  directory of its own as `TMPDIR`, writable even where the working directory is read-only, as
  SECURITY.md says. The `shell` tool's description and the permissions tab's sandbox line name the
  working directory and the extra paths and not it, so a model in a read-only session doesn't know
  it has anywhere to write. Saying so is a sentence in a description every request carries, so it
  waits for a decision on whether that's worth its tokens.

- **A question about a call whose arguments can't be read shows the raw wrapper.** The policy
  (`Careful::judges`), the question panel and the shell advisor read a call's arguments through
  `ops::inner`, and where that fails they fall back to the arguments as sent. A malformed `call`
  wrapper is then judged, rated and shown as the wrapper itself, and only the tool, once allowed,
  says it can't read it. Whether the question should say up front that the call will be refused is
  a choice about what a question is for.

- **No test that a confined command can't write `/dev/shm`, or open IPv6 or RDS through the gate.**
  A confined command may use the devices in `sandbox::DEVICES` and nothing else under `/dev`, and
  the gate holds `AF_INET6`, `AF_SMC` and `AF_RDS` as it does `AF_INET`. The devices aren't
  enumerated by any test, and the other families are tested at the filter, not through a command.
  A `/dev/shm` test needs a machine where `/dev/shm` is writable outside the sandbox, or it passes
  for the wrong reason; that is what unblocks it.

- **The terminal's loop and a served session's loop handle the kernel's events in two copies.**
  `main.rs` and `remote/server/mod.rs` each take in the broadcast, drain the outcomes, answer held
  questions around `serving.answer` and pump the server twice, and the copies have drifted in
  wording: a client that fails to connect is a note at the terminal and an error when served.
  Sharing them is a refactor of both loops, worth doing together with the next change to either.

- **"Prune" is still the documents' word for taking something out.** `prune` is gone as a command
  and as an action of `context`, in favour of exclude and elide, but CONCEPTS.md, the root README,
  `kamchatka`'s README and RUNNING.md, `nachalnik-providers`' README and INVARIANTS.md still use it
  for excluding or eliding in general. Each reads as ordinary English where it stands; replacing
  them is a pass over the prose, and waits on whether the one-word rule covers a document's general
  verbs as well as the program's own.

- **A sweep whose last turn fails is run again from the start.** The repo-sweep skill counts a
  sweep complete only when every message was answered, so one whose last turn was lost to a 429
  or cut off by the endpoint is moved aside and rerun, hours of work for one answer. `friction.sh`
  already carries a lost session on from its snapshot. `sweep.sh` could do the same, but
  `sweeps.py` reads completeness from one session's log, so a resumed sweep's two logs would have
  to be read as one first. The opposite case is counted complete: a last answer that ended with
  reasoning and no text never wrote its FINDINGS section, and its findings are only in its notes
  and that reasoning. On GMI those answers were streams cut off and read as finished (see the
  `[DONE]` entry), so the text being empty is the sign to go by. It is one more message away from
  done, so it waits on the same resume.

- **An outage the runner starts a sweep into can still cost it a try.** `run.py` sets a try aside
  without counting it only if the endpoint never answered it once. When an endpoint fails every
  request but a few, a sweep that got one answer before the failures counts as a try, and a long
  outage can use up all of a sweep's tries. A probe can come back fine during such an outage: GMI
  once answered one request in ten. Checking the endpoint before each start would not have
  helped. What would settle it is a rule for what makes a try the endpoint's fault, such as
  failures with no answer after them, read from the records like the rest of `sweeps.py`.

- **`nachalnik-eval`'s digest leaves out the answer-format sentence.** Every question is sent with
  `Reading::instructions()` after it (`Dossier::asked`), and `suite::instrument` hashes the
  dossiers, the templates and the fork's preamble but not that sentence. Changing it would change
  what every subject is asked while every digest, and the test pinning them, stayed the same.
  Hashing it moves every digest at once, so runs made before could no longer be compared with runs
  after though nothing they were asked changed; it waits for the next change that bumps
  `script::VERSION` anyway.

- **A session's root directories are resolved on every check.** `Reach` resolves the working
  directory and every `--sandbox-allow` and `--sandbox-read` root each time it checks a path, which
  a `grep` over a large tree pays for every file and symlink. Resolving them once at startup would
  mean a root that appears, moves or is relinked during the session is judged by its old location;
  that changes the boundary, so it isn't just an optimisation.

- **The first undo in a fresh session removes its setup.** `--system` and `-f` add items to the
  context like anything else, so each is an undo step: `/undo` in a session nobody has typed in yet
  removes the system instruction, and a second removes the attached file, each only reported as
  `undone`. A resumed session says there's nothing to undo, because `Kernel::resume` restores its
  items without a checkpoint. A host can't do the same for its own setup (the undo history can't be
  read from outside, and an undo followed by a redo leaves two pointless records), so this needs a
  kernel operation that clears the undo history, a core addition only for a client's benefit.

- **Output limits that ignore the context window.** A call's output is cut at a fixed number of
  bytes (32,000 for `fs:read`) regardless of the model's context size, and the compactor never
  touches the turn in progress. With a 14,000-token window, one read is over 8,000 tokens: two reads
  in a turn fill the context past the point where the full-context notice leaves any room, the
  model's next call goes over the limit, the request is refused, and a headless run skips every
  message after it. Either of two changes would fix it, and both change behaviour: limits that scale
  with the window, or letting the compactor drop what the model has already seen in this turn when
  the request would otherwise be refused.

- **`/params` showing a parameter's type and range.** It shows the default and maximum where the
  model listing publishes them, and nothing else, because no endpoint publishes more. A table kept
  in this workspace would just be someone's documentation as of the day it was copied. Unblocked by
  an endpoint that publishes them, read the way `Published` reads the rest.

- **A tool call with unparseable arguments goes back to the model as `{"_unparsed": "..."}`.**
  That's how `nachalnik-providers` stores such a call, and `to_wire` sends it back that way. A model
  can copy it and wrap every later call the same way; `kamchatka` accepts a wrapper whose text parses
  as the call inside it, which stops that without changing what's sent. Sending back exactly what the
  model wrote would be more accurate, but some endpoints reject it, so one broken call would make
  every later request fail; sending `{}` works everywhere and leaves it to the tool result, which
  already quotes what arrived. Unblocked by a decision on what the history should say the model
  sent, and a live test of that choice on the strict endpoints.

- **A stream that sends `[DONE]` and no `finish_reason` is an end of turn, even after reasoning
  alone.** `nachalnik-providers` reads the marker as the server ending the turn on purpose, which a
  note in `openai/wire.rs` defends. GMI ends a stream that way when its backend fails mid-answer:
  several thousand tokens of reasoning, no text, no finish and no usage, though every stream asks
  for usage. The turn is then recorded as finished with nothing said. Settled by deciding whether a
  marker with neither a finish nor usage after it is a cut-off rather than an end.

- **The web page relies on `Origin` for browsers that don't send `Sec-Fetch-Site`.** An image or a
  no-cors fetch from another site has no `Origin`, and the page refuses it based on fetch metadata;
  but a browser old enough to send neither gets through to `GET /events` and takes over the session
  from the tab that had it. The page can't be framed, which covers frames; covering the rest for
  every browser would need a token in the page's URL that `/events` checks, which changes how the
  page is opened.

- **A command that stops itself is waited on until the turn is interrupted.** `kill -STOP $$` or
  `SIGTSTP` leaves the call waiting for output that never comes, and a headless run waits until its
  `--deadline`, which then ends it and the stopped process cleanly. Waiting with `WUNTRACED` would
  detect the stop; what to do then (continue or kill the command, and whether a background job
  stopping counts too) is the open question.

- **A malformed MCP response leaves the call waiting.** A response with the call's id but neither a
  `result` nor an `error`, or a result with a different id, is dropped by `rmcp`, and the call waits
  for an answer that never comes until an interrupt or `--deadline` ends it, just as with a server
  that never answers. Reporting it needs `rmcp` to pass malformed responses on, which is up to
  `rmcp`; a time limit on calls is the alternative, and the same decision as the startup time limit
  above.

- **An attachment named as an image is sent as one, whatever it contains.** `attach.rs` picks the
  media type from the file extension alone, so a `.png` containing text is sent as `image/png`; the
  endpoint then rejects the request (a 502, or `Invalid image data-url`) and every later one while
  the item is in the context, with an error that doesn't say which item. An empty file is reported
  when it's attached, with the `/exclude` to remove it; a non-empty file of the wrong type isn't,
  since only its contents would show it. Checking an image's first bytes against its type would
  prevent this, but would also reject files that can be attached today, which is why it waits.

- **The Anthropic client and signed thinking after an edited history.** Described in the
  `nachalnik_providers::anthropic` docs, with the models it affects, and handled by neither the
  code nor the tests: some models reject a signed thinking block sent back after anything before
  it was edited (which pruning, rewriting and eliding all do), on accounts created on or after
  2026-08-31. The possible fixes are retrying on that one 400 error message, or a beta header no
  parameter can set. Neither OpenRouter, which rewrites the message and isn't what checks the
  signature, nor an older account shows the refusal. Unblocked by an Anthropic account created on
  or after 2026-08-31, to see it rejected by the real API and then fixed.
