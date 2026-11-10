# postponed, on purpose

Known issues and ideas deliberately left for later, written down so nobody rediscovers them. Each
entry says what it is, why it waits, and what would unblock it.

Only open items belong here. Do not record finished work, what was tested or checked, or how an
entry reached its current state; those belong in the commit message. A fix that closes an entry
removes it. A fix that closes part of an entry leaves only the remaining part.

---

- **The list of variables the shell removes can't be changed.** The `shell` tool removes
  kamchatka's own API keys (`endpoint::KEYS`) and any base URL with a `user:password@` in it.
  Nobody can add to that list, for example to hide `GH_TOKEN` or cloud credentials from the
  model, or empty it to give a command the keys. A `shell-env-exclude` setting would allow both:
  a list of variable names and patterns that starts as today's list. When it no longer includes
  kamchatka's keys, the permissions tab should say so. It waits until somebody asks for it.

- **A directory rule doesn't see the working directory's own path.** `Careful::judges` matches
  path rules against the `path` argument as written, so `.ssh/` matches `.ssh/config` but not
  `config` read from a session started inside `~/.ssh`, although the rule is meant to cover that
  directory wherever it sits. Matching the resolved path instead would make every call ask in a
  project that happens to live under a directory such a rule names (`secrets/`, `.aws/`). It waits
  on choosing between that, matching the working directory's components once at startup and
  warning, or leaving it as it is.

- **A model that repeats one refused call word for word gets the same refusal every time.** A
  model in a loop can send the same call that cannot be read many times in a row, and each gets
  the same sentence until the turn's request ceiling ends it. Nothing counts such repeats. It
  waits on choosing between saying that the call is the same one refused N times before, ending
  the turn after some number of repeats, or leaving it to the request ceiling.

- **A turn recorded by another dialect goes to Gemini with its thinking unsigned.** A turn kept as
  `content` and `reasoning` rather than as blocks, which is how the OpenAI dialects record one, is
  sent to Gemini as a `thought: true` part with no `thoughtSignature`, and its calls without one
  either. Whether Gemini takes that, ignores it, or refuses the request as it refuses a signed
  call sent back unsigned is not known here. Unblocked by a live Gemini request continuing a
  session recorded through another dialect, with thinking and a call in it.

- **Chat Completions names a tool's image instead of sending it.** A tool message there is a string,
  so an image in a tool result goes out as a line naming it. The dialect could move the image to a
  user message after the tool messages, where the API takes one, without changing the context.
  Unblocked by a model that needs it and a live test showing it reads the image once moved.

- **Anthropic's cache loses everything after an edit that isn't near the end.** The provider marks
  two cache points, the system prompt and the end of the request. Anthropic looks back only 20
  blocks from a mark for an earlier entry, so a session that edits its own context, as a sweep's
  does, falls back to the system prompt after an earlier edit and writes the rest again at more than
  the price of reading it. The API takes four marks, so two are unused. Placement is a trade-off:
  marks that follow the turns or a fixed grid of block positions catch different edits, each mark is
  a cache write, and blocks are counted after thinking and elision change them. Unblocked by a live
  comparison of placements on a session that edits its context, measuring what is read and written.

- **Serving clients older than the session.** A session refuses a client that names an older
  protocol version, because version 2 writes records a version-1 build of `kamchatka` cannot
  read. Serving it instead means keeping each connection's version and checking every write in
  `remote::server::connection::attend` against it: each message type, and each record format,
  would have to declare the version it was added in, and a version-1 client be sent request
  records with their items written out. It waits on someone needing to reach a newer session from
  an older build. `Message::Unknown` lets clients survive messages they don't know, and
  `Attached::version` tells them what the server speaks.

- **OpenRouter's `reasoning_details` is ignored.** A model whose reasoning appears only there loses
  it, and one that signs its reasoning across tool-call turns gets its turns back without it. No
  test so far has needed it. A model that does, with a live test showing the difference, would show
  whether the OpenAI client should support it.

- **The `/dev/shm` test has not been seen to run.** `a_confined_command_cannot_write_shared_memory`
  skips wherever `/dev/shm` cannot be written outside the sandbox, including a session run under
  `kamchatka`'s own confinement. Its assertions, and whether it fails once `/dev/shm` is added to
  `sandbox::DEVICES`, are unchecked. Unblocked by a run on a machine where `/dev/shm` is writable.

- **`/params` showing a parameter's type and range.** It shows the default and maximum where the
  model listing publishes them, and nothing else, because no endpoint publishes more. A table kept
  in this workspace would be only someone's documentation as of the day it was copied. Unblocked by
  an endpoint that publishes them, read the way `Published` reads the rest.

- **The Anthropic client and signed thinking after an edited history.** The
  `nachalnik_providers::anthropic` docs describe it, with the models it affects; neither the code
  nor the tests handle it. Possible fixes are retrying on the 400 those docs quote, or a beta header
  no parameter can set. Neither OpenRouter, which rewrites the message and is not what checks
  the signature, nor an older account shows the refusal. Unblocked by an Anthropic account created
  on or after 2026-08-31, to see it rejected by the real API and then fixed.

- **What a session costs in money, and what is left of the balance.** OpenRouter reports each
  request's cost in credits in `usage.cost` on every response, in the last chunk of a stream;
  nachalnik ignores it. No response includes the balance. `GET /api/v1/key` returns what is left
  under the key's own spending limit, null for a key without one. The account balance is only at
  `/api/v1/credits`, which needs a management key. Anthropic, OpenAI and Gemini offer neither to an
  ordinary key, so supporting OpenRouter alone would show a cost on some endpoints and nothing on
  the rest. Unblocked by providers settling on a common way to report a request's price and a key's
  balance, or by deciding an OpenRouter-only figure is worth having.

- **An MCP call whose future is dropped is never cancelled at the server.** `McpTool::invoke` sends
  MCP's `notifications/cancelled` when the call is interrupted or times out. A caller that drops the
  step future while the call is with the server, rather than interrupting it, sends nothing, and the
  server goes on with work whose answer is thrown away. kamchatka stops a call by interrupting it,
  so it is not affected. A guard that spawns the cancellation when dropped would close it, given a
  runtime to spawn on. Unblocked by a caller that drops step futures, and a test server that can say
  whether it was told to stop.

- **A counter is not asked about the calls inside a turn kept as blocks, one by one.**
  `TokenCounter::count_message`, `count_item`, `uncounted_message` and `uncounted_item` hand a flat
  turn's calls to the counter as a name and a JSON value each, so a counter that abstains on JSON
  says so for their arguments. A turn kept as `Content::Blocks`, as Anthropic and Gemini send one,
  is handed over whole, and the calls inside it are priced or declined only if the counter walks the
  blocks itself. The counters here are right either way: `BytesPerToken` declines only pictures and
  finds them at any depth. Splitting the blocks up in the default methods would change what `count`
  is asked for every counter written against them. Unblocked by a counter that abstains on JSON, and
  deciding whether the trait splits blocks or documents that a counter must.

- **What the copies of a failed observation said and cost is lost.** `Ablation::observe` returns
  the first failed copy's error and nothing else, so the answers and the spend of the copies beside
  it that answered never reach the record or the run's total. A run that fails reports less than it
  spent. Carrying them needs an error that holds a `Spend` and the answers, or `observe` returning
  them beside the error, either of which changes `nachalnik-eval`'s public API. Unblocked by a run
  whose reported cost has to match the bill.

- **A streamed answer can keep both the thinking and a summary of it; a whole one keeps one.**
  `summarised` appends a chat-completions `reasoning_summary` to the reasoning a stream already
  carried, on purpose, since a summary is not the thinking repeated. A whole body takes the summary
  only where the message has no reasoning. An endpoint that sends both would get the two paths
  disagreeing, and send the summary back as thinking with `send_reasoning` on. Whether any endpoint
  sends both is unknown: Inception's, the one that sends summaries, was not seen streaming
  `reasoning` beside them. Unblocked by a live run against it with `reasoning_summary: true`.

- **A Gemini text run keeps only the last part's attached fields.** Parts of one kind are joined
  into one block, and each later part with fields beside its text replaces the earlier part's, so
  a field attached to a middle part, such as citation metadata, is lost when a `thoughtSignature`
  follows. Merging them is not clearly right either: a citation's offsets are about its own part's
  text, and what Google accepts back beside a joined run is unknown. Unblocked by a live run that
  returns citations in a streamed, multi-part answer.

- **A sandbox whose process groups cannot be read is let go of as an empty one.** `live` keeps a
  confined command's sandbox only while `Controller::process_groups` returns groups, so an error
  drops it like a sandbox with nothing left in it. `from_a_command` decides from that list whether
  a client connecting to a served session is a confined command, so while such a sandbox's
  processes are alive they are no longer told apart from anyone else - the open way to fail.
  Keeping it on an error and answering "cannot be distinguished" fails closed, but a sandbox that
  errors for good would then refuse every client for the rest of the session. When sandlock
  returns that error is not known here. Unblocked by finding out, from sandlock's source or a test
  that makes it fail.

- **Two sessions in different pid namespaces sharing `/tmp` can take each other's `$TMPDIR`.**
  `scratch_path` names a command's directory after this process's pid and a counter, and
  `make_scratch` removes and remakes a directory of that name that is already there and the same
  account's, on the grounds that pids come round again. Two `kamchatka` processes in different
  pid namespaces - two containers sharing one `/tmp` under one account - can hold the same pid
  at once, and then one removes the other's directory under a running command. Nothing here has
  been seen to do it. Unblocked by naming the directory with something no other process can
  share, such as a random part, at the cost of the name no longer saying whose it was.

- **A headless run with `--rate-shell-commands` pays for ratings it never shows.** `Advised`
  rates every shell command its policy would ask about, and a headless run answers those
  questions with `--on-ask`, so each command is sent to the advisor and paid for, and nothing a
  headless run prints or records carries the band. The note on `Advised::evaluate` gives exactly
  that as the reason not to rate an `Allow` or a `Deny`. It waits on choosing between printing the
  band beside the answer, not rating where nothing draws it, and refusing the flag in a headless
  run.

- **A granted call that is cancelled is decided a second time.** `cancel_pending_calls` announces
  `permission.decided` with `deny` and `cancellation` for every pending call, including those
  already granted and waiting in `Ready`, so the log holds two decisions for one permission id,
  while `decide` refuses a second decision for one. Leaving the granted ones out would keep one
  decision per id, but leave an `allow` on record for a call that never ran, explained only by its
  result. It waits on choosing which the log should say.

- **The conformance suite's server reads a request once, sixteen KiB at most.** A socket closed
  with bytes still unread is reset rather than closed, and a reset can throw away an answer the
  client has not read yet, so a provider whose request is larger, or arrives in two pieces, could
  see a case fail for the fixture's sake. Every case's request here is far smaller, and a 256 KiB
  request against a loopback server got its whole answer, so it could not be shown failing.
  Unblocked by a provider under test whose requests are that large failing a case this way.

- **A person's pin or edit can land between the `context` tool's check and its change.** `moved`
  reads whether the person pinned an item, then calls `set_states`; `revise` reads the item, then
  calls `replace`; `Undoing::apply` does the same. The terminal's `p` and `e` act during a turn,
  so a pin or an edit made in that gap is overwritten, and the model is told its change went
  through. The gap is the time between two calls on the same task, but it is there. Closing it
  needs the kernel to check and change under one lock, a conditional `set_states` or `replace`,
  which is a core change for one client's sake. Unblocked by deciding whether the runtime should
  offer one.

- **`provenance`'s digest leaves out the call's identifier.** `Errand::call` is fixed in the
  material because the copies read it and it goes out on the wire, but `Errand::text` omits it, so
  two errands that differ only in their call identifier have the same digest and `compare` treats
  their runs as comparable. Adding it moves the `provenance` fingerprint, so every run taken so far
  stops being comparable with later ones although none of them read different material. It waits
  on choosing between that break and leaving it until the material changes for another reason.

- **A call whose arguments are exactly `{"_unparsed": "..."}` reads back as an unparsed call.**
  `ToolCall` reads a call recorded before `unparsed` existed by recognising the old wrapper: an
  object whose only key is `_unparsed`, holding text that does not parse. A call written today
  with those arguments - models do copy the wrapper, which is why `kamchatka`'s `ops::inner` looks
  for it - is the same bytes, so after a resume its arguments are empty and it is marked unparsed.
  One already answered only changes how it is sent back; one waiting on a question when the
  session was saved is answered by the kernel as unparsed rather than asked about. Telling the two
  apart needs the writer to mark one of them, which is a snapshot format change. Unblocked by
  deciding whether the old form still has to be read, or by the next format change.

- **`--connect` writes the session's records from where it attached, not from the start.** A
  client's standard output is documented as the whole log, the bytes `/save` writes. It writes
  records live as they stream, and fetches the ones from before it attached only when it detaches,
  from a mark that every streamed record moves. So a client that saw any record stream -
  `printf 'question\n' | kamchatka --connect` against a session with history - writes the new
  turn's records and never the history before them. Writing the log in order and in full means
  either holding streamed records back until the history has arrived, so a watching client writes
  nothing live, or fetching the history on every attach, which the client avoids on purpose for
  somebody watching. Unblocked by choosing which.

- **An elided assistant turn goes back without its thinking, which some endpoints refuse.** With
  `send_reasoning`, each assistant turn is sent back with its thinking, except one the projector
  elided, which goes as its marker alone. An endpoint that requires the reasoning of every earlier
  assistant message answers that request with a `400` naming the message, and every request after
  it the same way until the turn is restored. A model that elides its own turns with the `context`
  tool, or a compactor that elides them, has in effect ended its session there; a sweep's model
  did, eliding "all explored reads" before its last answer. It waits on choosing between sending
  the thinking beside the marker, which keeps part of what eliding took out, refusing to elide an
  assistant turn while `send_reasoning` is on, and naming the turn to restore in the refusal.
  Unblocked by that choice and a live test against such an endpoint.

- **A step that compacts projects and prices the whole context four times.** A step with
  nothing to compact now projects once, and its compactor is shown the cost of the request about
  to go out. One that compacts builds that request, projects twice more in `apply_compaction` for
  the report's before and after figures, and builds the request again; one that changes whether
  the context is full adds a `budget` and, with a notice, a fit check. Unless the context
  changed while the plan was awaited, the compaction's two figures are the costs of the requests
  built either side of it, so two of those projections are redundant. Each is linear in the context and milliseconds against a
  request, and a step compacts rarely, which is why it has not been done.

- **The repo-sweep status counts a resumed sweep's earlier cuts as messages passed over.** A
  resumed session's log begins with the records of the part before it, cuts `drive.py` sent
  included, but `sweeps.py` discounts only the cuts in the current part's `NAME.carried`; the
  earlier part's is in `old/parts/`. Such a sweep shows `2/1 sent` and `status.py` warns that a
  message was passed over when none was. Whether it is complete is unaffected, since that also
  needs the session to end on its own after its last message. Adding up every part's `.carried`,
  or counting only the records after the last `session.resumed`, would settle it; it waits for
  the next change to the skill's scripts.

- **`ANTHROPIC_API_KEY` is read only under `--api anthropic`, even at Anthropic's own address.**
  Anthropic also serves an OpenAI-compatible API at `https://api.anthropic.com/v1`. A session
  pointed there in the default dialect, with only `ANTHROPIC_API_KEY` set, is refused at startup
  with "set KAMCHATKA_API_KEY", although the key is set and the address is its issuer's: `keyed`
  names `OPENROUTER_API_KEY` and `OPENAI_API_KEY` for their hosts but not this one, and only
  `anthropic::key` reads it. `--help` says so ("with --api anthropic, and only there"), while
  RUNNING.md says only "for Anthropic's own API". Naming it in `keyed` for Anthropic's host would
  keep the rule that a key goes only to whoever issued it. It waits on whether that dialect at
  that address is something to support, and then the two documents are made to agree either way.

- **A command can print a permission error and get the sandbox's note for it.** `refused`
  (`sandbox/mod.rs`) takes any line of a command's standard error that says `Permission denied`,
  `os error 13` or `EACCES`, so a line a command only prints - a script's `echo`, a log it replays -
  naming a path outside the reach gets the note that says "the permission error below is the
  confinement rather than the file's own permissions" and to ask for the path to be opened up,
  though nothing was refused. SECURITY.md says to say nothing when the refusal wasn't the
  sandbox's. Output can already tell a model anything, and opening a path is the person's
  decision, so this puts the program's voice behind words it did not check rather than opening
  anything. The output cannot tell a printed refusal from a real one; hedging the named-path notes
  as the pathless one is hedged would make them true either way, at the cost of the plain wording
  that stops a model trying `sudo`. It waits on that choice, or on a way to learn which calls the
  confinement refused (a Landlock audit log, or sandlock reporting it).

- **Three files under `kamchatka/src/app/` hold more than their own concern.** `transcript.rs` is
  about entries and where they anchor, but also holds `overran` and `remedy`, which read the budget
  and price turns to say what to do about a request over the limit, and `recall` and `replay`, which
  read the record beside a snapshot; the first pair could be `app/overrun.rs` and the second could
  join `session.rs`. `text.rs` holds `trace_line` and its tests, a third of the file, beside the
  views and figures it is named for; `trace.rs` could take them. And `submitted`
  (`app/command/mod.rs`) decides whether a line waits in two blocks that each push onto `held` with
  their own note, beside a third queue for messages typed into a turn, and asks `switches` - a
  second list of the commands that switch, beside the dispatch, that nothing checks - whether a
  line is one; one function saying why a line waits, and the dispatch declaring which commands
  switch, would make it one place. None of it is wrong today. It is a refactor worth doing with the
  next change to any of them.

- **`kamchatka/CHANGELOG.md` is over six thousand lines, nearly all of them released.** The
  `[unreleased]` section is the first hundred or so, and each release adds hundreds more below it,
  so nobody reads the file whole. Keep a Changelog allows moving released sections, unchanged, to an
  archive file beside it (`CHANGELOG-0.1-0.19.md`, say) with a line pointing there, so the main
  file holds the working end and the recent releases. `scripts/references.sh` leaves out only
  files named `*CHANGELOG.md`, so an archive named that way would have its old item names checked
  against today's code; its pattern would widen with the move. It waits on whether the history is
  read in the file at all, or only through the release tags.

- **kamchatka's three documents say some things twice, and RUNNING.md holds two pages of its own.**
  The `/compact` confirmation (the list, <kbd>y</kbd> or <kbd>n</kbd> in place of the prompt,
  pinning while it waits) is described in both GUIDE.md and RUNNING.md, the status line's estimate
  and its 70% and 90% colours in GUIDE.md, RUNNING.md and README.md, and which rule wins in
  GUIDE.md and RUNNING.md's headless table; a change to any of them has to find every copy. Saying
  each once - the screen's behaviour in GUIDE.md, the mechanics in RUNNING.md - and linking from
  the rest would make one place to change. RUNNING.md's last three sections (what a command can
  read, a toolchain in the home directory, the network when a command tries) are the sandbox
  from end to end and could be a page of their own that GUIDE.md and README.md link to; the
  advisor's section, behind a feature that is off by default, could join it or stand alone. It
  waits for the next change to any of these passages.

- **The introspection tools have grown past what their files can hold at a read.**
  `introspect/context/changes.rs` keeps the undo journal and the eight operations that write to it,
  over a thousand lines; the journal (`Journal`, `Entry`, `Left`, `Undoing`, `Applied`,
  `moved_since`, `hold`) is a file of its own. `introspect/context/reads.rs` keeps five renderers
  that share little: the list, search, budget and request answers could each be one under a
  `reads/` directory. The four tools (`context`, `fork`, `log`, `setup`) each write the same
  `limit`, `unparsed` and `malformed` over the same fields, and `log`'s has gone its own way - an
  absent `action` is `read`, and an unknown one is refused without the `unplaced` hint the others
  give. One struct holding the shared fields, with those methods, would keep them alike. Inside the
  files, `fork`'s `branch`, `changes.rs`'s `moved` and `setup`'s `permissions` each do several
  things in one body and could hand the parts to functions. None of it is wrong today; it is a
  refactor for the next change to these tools.

- **An error and its causes are written out by two copies of one function.** `described` in
  nachalnik's kernel and `with_causes` in nachalnik-providers each join an error and the causes
  under it, skipping a cause the message already holds. The two crates ship together and their
  output reads side by side - a tool's failure and a request's - so a change to one rule and not
  the other would show. One copy means public API somewhere: in nachalnik, which takes nothing that
  can live outside it, or in a crate both already depend on. It waits on where that should be.

- **A frame works out the next request more than once, and redraws what has not changed.** A frame
  takes one `Going` and one `Budget`, and each projects every item and counts every message again,
  serialising every tool call's arguments; the chat tab rebuilds the whole conversation, call
  arguments as JSON, on every frame; a question is rendered two or three times a frame while it is
  asked; and the permissions tab builds its rows four times. A frame is drawn only when something
  changed, but a model streaming and a question being asked both change something many times a
  second. A reading kept until the log's sequence or the counter moves would make it cheap; it
  waits on measuring a long session first, since the cache is what could go stale.

- **`kamchatka/src/app/mod.rs` is the `App` struct and five groups of methods in one file, and some
  of its state is kept in step by hand.** The answer types, the keys and help, waiting and leaving,
  the item operations and the notices each have a sibling module they could move to, leaving `App`
  and its construction. Inside it, `held` is a queue of `(String, bool, bool)` whose two flags are
  the caller's `keys` and `remote`, unnamed wherever they are pushed or swapped back in; seven
  fields of the spend ceiling must move together, which a note says and no type does; three scroll
  positions are clamped by hand beside the overlay's own; `question_key` answers three kinds of
  question in turn, each with its own copy of the scrolling keys; and the compaction pass is started
  in two places with guards of their own. Named structs - a held line, a ceiling, a scroll - and a
  match on the open question's kind would make each one place. A refactor for the next change to
  any of them.

- **The crates' READMEs and crate docs say the same things in two or three places.** nachalnik's
  loop diagram, trait table, protection model and features list are in its README, its `lib.rs` and
  partly in `kernel/mod.rs` and AGENTS.md; nachalnik-eval's scoring decisions and claim families in
  its README and `lib.rs`; kamchatka's `lib.rs` retells the tabs, the dialects and served sessions
  that GUIDE.md and RUNNING.md describe; nachalnik-mcp's trust default is argued in its README,
  `lib.rs` and `Trust`'s docs; nachalnik-providers' dialects and its "reads no environment" promise
  in its README and `lib.rs`; and the workspace's crates are listed in the root README, AGENTS.md
  and nachalnik's README in three wordings. CONCEPTS.md's table of event names matches the 41 in
  `event.rs` today, but nothing checks it does. Keeping each fact where it is normative - the type's
  docs - and having the rest summarise and link would make a change land once. It waits for the
  next change to one of these facts.

- **The browser page, the web module and the examples each carry what a split or a shared piece
  would hold once.** `kamchatka/src/web/browser.html` is styles, markup and script in one file of
  seventeen hundred lines; the styles and its markdown renderer are self-contained and could be
  pieces `web/mod.rs` joins with `include_str!`, and its `draw()` is three views in one function.
  `web/mod.rs` keeps generic HTTP parsing (`Request`, `head`, `body`, `reply`, `param`) beside the
  relay, and five hundred lines of tests. Among nachalnik's examples, `compare_models` and `panel`
  each write the same flag loop and the same run-every-task harness, `compaction` and
  `transparency` each a scripted provider that has come to differ, and `examples/common` two word
  wrappers. And `scripts/windows.sh` copies CI's feature configurations by hand; they match today,
  and nothing would say when one is added to CI alone. A refactor for the next change to any of
  them.

- **The `context` tool's undo journal has no depth, and goes into every snapshot.** Each change it
  makes is an entry holding the items as they were, old content included, and nothing drops the
  oldest; `History::saved` writes the whole journal into the snapshot at the end of every turn. The
  kernel's own undo stops at `Config::context_undo_depth` (16). A session that revises large items
  many times writes all their old versions into every snapshot after. Holding the journal to the
  kernel's depth would bound it, at the cost of the model undoing a change further back than that;
  nothing documents how far back it may now. It waits on choosing the depth, and saying it.

- **The `context` tool works out the next request two or three times a call.** Its reads (`look`,
  `matched`, `budget`, `request`) and its changes each project and price the whole context more than
  once - before and after a change, or once for the answer and once for the budget - which is the
  single reading a frame wants too. Milliseconds a call; it waits with the frame's.

- **The libraries' longest test files hold several subjects each.**
  `nachalnik-eval/tests/harness.rs` (about 2,500 lines, no section markers), nachalnik's `live.rs`
  (five sets of environment variables), `concurrency.rs` (four stories under a header naming three),
  `tokens.rs` (calibration is half of it, and the context-limit refusals an unmarked fourth part),
  `state.rs` (the request budget and interrupts beside the transitions), `kernel/running.rs` (output
  limits are 400 of its lines), and nachalnik-providers' `reading.rs` and `anthropic.rs` (each two
  halves its own markers name). Each could be a directory, as `tests/kernel/` already is, with the
  shared fixtures in its `main.rs` and a module per subject. It is a move of whole tests, with
  nothing to change in them, for the next time one of these files is worked on.

- **The served session's three files each hold more than one concern.** `remote/server/mod.rs` is
  the listener (binding, accepting, dropping), the coordinator sharing the session between clients,
  and the application of each command, fifteen hundred lines before its tests; `listener.rs`,
  `serving.rs` and `apply.rs` would hold one each, and `apply` spells each command's name again in
  its refusals where `name()` already has it. `remote/client.rs` drives the connection and renders
  prose in one file, its renderers could be `client/draw.rs`, and its `Client` carries a dozen
  booleans recombined differently by `resting`, `reads`, `ready` and `idle`, where a connection
  phase and a question-answering policy would each be a type. `remote/protocol/mod.rs` keeps the
  payload types beside the verbs; they could be a module of their own, re-exported. A refactor for
  the next change to any of them.

- **A confined `shell` call does blocking work on the runtime's threads.** Before the command
  starts, the scratch directory is made and opened, the environment rebuilt, the git
  configuration and each allowed root resolved, and sandlock holds each root, all on one of the two
  runtime workers; while it is quiet, `stopped_itself` reads `/proc/<pid>/stat` with a blocking
  read every 120 ms; and standard error, up to `KEPT`, is copied whole on every call although the
  copy is used only when it is cut. `note_for_in` and the scratch's removal already go to
  `spawn_blocking`; the preamble and the heartbeat's read could too. Separately, a command's
  stragglers are kept running after the call, and the scratch is removed while they may still
  write to it, which can leave the directory behind; removing it once the kept sandbox ends would
  close that. It waits for the next change to how `shell` starts and ends a command.

- **`fs`'s files and search modules share their path-rule machinery across each other.** `search.rs`
  takes `files::allowed`, `files.rs` takes `search::{barred, led_past, climbing}`, and each resolves
  the working directory with the same fallback; one module both import would hold the cluster. The
  path-rule refusal for a link and for a hard link's other name is written out twice in
  `files.rs`, the glob refusal and its matcher twice in `search.rs`, and an operating-system error
  on the resolved path four times. `Grep::invoke`, `read` and `Edit::invoke` each parse, work and
  answer in one long body, `ops.rs` is the operation table and the reading of a call's arguments in
  one file, and three argument readers used only by searching sit in the module every tool shares.
  A refactor for the next change to any of them.

- **`shell.rs`, `advice.rs` and `policy.rs` each hold a self-contained piece beside their main
  type.** `tools/shell.rs` (about 2,600 lines) holds the pure parsing of a command's exit and
  joints, which `advice.rs` and the screen use too, the stragglers' bookkeeping and the network
  gatekeeper beside the tool; each could be a module, the gatekeeper beside `reaching.rs`, whose
  queue it answers. `advice.rs`'s rating algebra is pure and tested on its own, apart from the
  questions it sends; `policy.rs`'s path-pattern language and its network heuristic sit beside
  `Careful`. `shedder.rs`'s `plan` works out its sets of items and runs both rules in one body.
  Small repeats: `Sandbox { shell_readable, devices, ..Sandbox::of(..) }` is written twice because
  `Sandbox::of` defaults those two fields, so a field added to `Sandbox` would keep its default at
  both without a word, and `Sandbox::of` taking them would end that; reading the arguments as the
  tool reads them (`inner(..).unwrap_or(..)`) is written four times, and walking back to a
  character boundary three. A refactor for the next change to any of them.

- **nachalnik's `model/mod.rs` holds a tool call beside the messages, and its selectors keep their
  own copies of three vocabularies.** `ToolCallId`, `ToolCall`, `StoredCall` and the measuring
  helpers around `json_len` are a unit of their own in `model/mod.rs` and could be `model/call.rs`,
  re-exported so no path moves; and the tests of `truncate_to`, `sized` and `Blob` sit there, six
  hundred lines from `content.rs`, which they test and which has none of its own. `selectors.rs`
  lists the item kinds, states and sources by hand beside `ContextKind::name`, `ContextState`'s
  `Display` and the item constructors that define them; all three match today, and a new kind,
  state or source would be refused by selectors until somebody added it there too. Deriving the
  kinds and states from the types, and keeping only the plural aliases by hand, would make each
  list one. A refactor for the next change to either.

- **The wiring's preflight, `main`'s `session` and a few readers are each written more than once
  or in one body.** `wiring/mod.rs` holds the check that refuses a rule naming nothing (`offered`,
  `built`, `judged_as`, `unreached`, `named`, `Refused`, `Setup::check`), which `wire` touches only
  through `check()`, and it could be a module of its own. `main.rs`'s `session` dispatches four
  one-shot modes before a 560-line body that opens each session. `mcp.rs`'s `attach_within` and
  `reinstall` each claim, partition and install with only their sentences apart; `headless.rs`
  drains queued events three times inside `run`; the log beside a snapshot is read three ways
  (`past_the_snapshot`, `last_talked_to`, `Earlier::read`), each skipping what it skips; and
  `endpoint.rs` holds the advisor's account rules beside the endpoint. A refactor for the next
  change to any of them.

- **Five of SECURITY.md's claims about the network gate have no test.** That `io_uring` is refused
  outright (with `ENOSYS`, so a confined command cannot reach the network around the gate), that a
  raw inet socket is refused, that an inet `bind` is held like a `connect`, that a ping socket is
  judged by `net:reach`, and that `sendmsg` and `sendmmsg` are held as `sendto` is: each rests on
  sandlock's filter, and `tests/sandbox.rs` exercises `connect`, `sendto` and the socket families
  but none of these. Each would be a confined command in the shape of
  `a_socket_that_can_leave_the_machine_is_refused_by_a_shut_gate`, with the same unconfined control
  so a machine that refuses the call anyway skips rather than passes. They wait on a machine that
  can run them: the session these were found in refuses `io_uring` unconfined and fails the gate's
  own tests, so nothing written here could be seen to pass, or to catch anything.

- **Two of nachalnik-providers' files are as much tests as code, and `Gathering::fold` does three
  things in one body.** `openai/wire.rs` is about 1,900 lines, half of them its test module, and
  `system1.rs` about the same, its wire types, its client and its tests; each test module could be a
  child module of its own, which keeps the access to private items it needs, and System One's types
  could leave its client. `Gathering::fold` works out which call a fragment continues - by index,
  by identifier or neither, each with a carve-out for a server that sends the whole call again -
  then parses and merges it in the same body; the first part as a function of its own would let
  each be read alone. A refactor for the next change to either.

- **nachalnik-eval's suites repeat their steps, and several of its files hold two concepts.** The
  ask-then-sweep of `attribution`, `feedback` and `instrumented`, recording what each copy did
  before it fails, is written three times, and asking one probe per label four times; "what the
  copies answered" is said through `said_or_nothing` in one suite and with an inline
  `unwrap_or("nothing")` in seven. `score/reports.rs` holds twelve figure types, `trial.rs` the
  recorder beside `Resolution` and `Step`, `suite/handles.rs` two tools and their policy,
  `experiment.rs` the runners beside the report types, `fork.rs` the result types every suite reads
  beside the copying,
  and `instrumented`'s `ladder` three stages in one body. In nachalnik-mcp, `tool.rs` names tools
  and converts results beside `Trust` and `McpTool`, and its `invoke` stops a call two ways in one
  loop. A refactor for the next change to any of them.

- **kamchatka's longest test files hold several subjects, with their helpers between the tests.**
  `tests/headless/main.rs` (about 4,700 lines) keeps the recorder's tests, the slash commands and
  the endpoint switches beside each other, and helpers such as `said`, `OneEach`, `FailsToFlush`,
  `Watchful` and `answering` thousands of lines from the harness at its top; `program.rs`,
  `sandbox.rs`, `config.rs` and `live.rs` each mix three or four subjects the same way. In
  `sandbox.rs` a `Shell { .. }` is built inline eleven times, each spelling every field, so a new
  field means eleven edits where a `shell(dir, confined)` beside `gated_shell` would mean one. Each
  file could be a directory of subjects, as `tests/headless/` already is in part, with its helpers
  together. A refactor for the next time one of them is worked on.

- **`sandbox/mod.rs` holds the refusal notes and the served sandboxes beside the sandbox, and a
  confined command's environment is built in two places.** The notes (`note_for_in`, `reads_only`,
  `connecting`, `refused`, `paths_in`, `Described`) and the bookkeeping of sandboxes a served
  session started (`SERVED`, `SANDBOXES`, `Starting`) are each a module's worth, and `argv` and
  `EXEC_FLAG` are read only by `confinement.rs`. `note_for_in` grew a branch per cause and picks
  between "is" and "are" three ways; one sentence builder over the groups would hold it.
  `run_if_asked` (`--confine-and-run`) and `shell`'s `spawn` each set `TMPDIR` and
  `GIT_CONFIG_GLOBAL` with near-identical notes, and only `spawn` asks `fits`, which matters for a
  model and less for a person reading sandlock's own error; one `Sandbox::environment_for` would
  serve both. `Reach::range` and `Described` render the same roots, kept alike by a note; and the
  two-press `ctrl+c` is written three times around `Stopping`. A refactor for the next change to
  any of them.

- **The screen's tabs live in one file, and some of what they decide belongs to the app.**
  `ui/tabs.rs` holds the conversation, context, permissions and trace tabs, already divided by its
  section comments, and each could be a file. `draw_chat` builds the blocks, resolves the scroll
  anchor, measures what is off screen and writes five fields back in one body. Some decisions are
  made in the drawing layer or twice: whether an item is mid-flight is worked out from the pending
  permissions in `tabs.rs` while its sibling `sends_content` is in `Going`; which question is open -
  a tool's, then the network's, then a compaction's - is written once for drawing, once for the
  panel's title and once for the keys, so a new kind of question must land in three places; a
  selection is clamped by the keys and again by the drawing; and what the policy will consult is
  worked out in `overlay.rs` beside `App::about`, which has the rest of the question. The
  permissions tab draws its empty state two ways, the context and trace tabs their "nothing here"
  lines each in their own words, and the faint bar down a code block is built three times. One
  `App` method returning the open question first, since it is the one with three copies. A
  refactor for the next change to the screen.
