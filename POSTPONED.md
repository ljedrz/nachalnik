# postponed, on purpose

Known issues and ideas deliberately left for later, written down so nobody rediscovers them. Each
entry says what it is, why it waits, and what would unblock it.

Only open items belong here. Do not record finished work, what was tested or checked, or how an
entry reached its current state; those belong in the commit message. A fix that closes an entry
removes it. A fix that closes part of an entry leaves only the remaining part.

---

- **A hard link to a protected file is read under its other name without a question.** Path rules
  match the name a tool is given, and a hard link has no target to follow, so `alias` sharing
  `.env`'s inode reads as an ordinary file. Catching it means finding a file's other names, which
  only a walk of what the session reaches can do, on every read of a file with more than one link;
  a build directory is full of them. It waits on choosing between that walk, refusing every file
  with more than one link under a path rule, or leaving it as SECURITY.md describes.

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

- **What the copies of a failed observation had cost is lost.** `Ablation::observe` returns the
  first failed copy's error and nothing else, so the spend of the copies beside it that answered
  never reaches the run's total. A run that fails reports less than it spent. Carrying it needs an
  error that holds a `Spend`, which changes `nachalnik-eval`'s error type. Unblocked by a run whose
  reported cost has to match the bill.

- **A call whose arguments are exactly `{"_unparsed": "..."}` reads back as an unparsed call.**
  `ToolCall` reads a call recorded before `unparsed` existed by recognising the old wrapper: an
  object whose only key is `_unparsed`, holding text that does not parse. A call written today
  with those arguments - models do copy the wrapper, which is why `kamchatka`'s `ops::inner` looks
  for it - is the same bytes, so after a resume its arguments are empty and it is marked unparsed.
  One already answered only changes how it is sent back; one waiting on a question when the
  session was saved is answered by the kernel as unparsed rather than asked about. Telling the two
  apart needs the writer to mark one of them, which is a snapshot format change. Unblocked by
  deciding whether the old form still has to be read, or by the next format change.

- **A command can leave its temporary directory behind.** The directory a confined command gets
  as `TMPDIR` is removed when the call ends, and a failed removal is not reported. A command that
  makes a directory inside it unreadable (`chmod 000`) makes the removal fail, and the directory
  stays in the temporary directory with whatever was written to it. Opening such directories up
  before removing them is done by this unconfined process while something the command left
  running may still be swapping names for links, so it needs `chmod`s relative to a held
  directory that never follow a link. Unblocked by writing that walk, or by deciding a report of
  what was left is enough.

- **`--connect` writes the session's records from where it attached, not from the start.** A
  client's standard output is documented as the whole log, the bytes `/save` writes. It writes
  records live as they stream, and fetches the ones from before it attached only when it detaches,
  from a mark that every streamed record moves. So a client that saw any record stream -
  `printf 'question\n' | kamchatka --connect` against a session with history - writes the new
  turn's records and never the history before them. Writing the log in order and in full means
  either holding streamed records back until the history has arrived, so a watching client writes
  nothing live, or fetching the history on every attach, which the client avoids on purpose for
  somebody watching. Unblocked by choosing which.

- **A step that compacts projects and prices the whole context four times.** A step with
  nothing to compact now projects once, and its compactor is shown the cost of the request about
  to go out. One that compacts builds that request, projects twice more in `apply_compaction` for
  the report's before and after figures, and builds the request again; one that changes whether
  the context is full adds a `budget` and, with a notice, a fit check. Unless the context
  changed while the plan was awaited, the compaction's two figures are the costs of the requests
  built either side of it, so two of those projections are redundant. Each is linear in the context and milliseconds against a
  request, and a step compacts rarely, which is why it has not been done.
