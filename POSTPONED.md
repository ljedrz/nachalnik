# postponed, on purpose

Known issues and ideas deliberately left for later, written down so nobody wastes time rediscovering
them. Each entry says what it is, why it waits, and what would unblock it.

Only what is still open belongs here. No finished work, no record of what was tested or checked,
no history of how an entry got to where it is: those go in the commit message. The fix that closes
an entry removes it; one that closes part of it leaves only the part that is left.

---

- **Chat Completions and Gemini name a tool's image instead of sending it.** A tool message there
  is a string and a `functionResponse` an object, so an image in a tool result goes out as a line
  naming it. Each dialect could move the picture to where its API takes one - a user message after
  the tool messages, an `inline_data` part beside the `functionResponse` - without changing the
  context. Unblocked by a model on each that needs it, and a live test showing it reads the
  picture once moved.

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

- **Two runs of the live tests at once.** `kamchatka`'s `live.rs` uses `live-{name}` directories
  under the target directory, so two simultaneous runs with the same `CARGO_TARGET_DIR` delete each
  other's files. `common::scratch` keeps tests within one run apart (one test binary at a time under
  `cargo test`, one directory per test under nextest), but two runs share every name either way. A
  target directory per run, or a run id in the directory name, would fix it if concurrent runs are
  needed.

- **No time limit on an MCP server's startup.** The handshake, `tools/list` and `resources/read`
  wait as long as the server takes, and the first `npx -y` can legitimately take minutes to
  download its package. A limit has to be long enough for that and short enough to be useful, and
  what a person sees while waiting is part of the same decision. Each server is named on stderr
  before it starts, and `--deadline` limits a headless run; a session with a screen just waits.

- **Schemas and names Gemini rejects.** Google's API rejects some JSON Schema features an MCP server
  may use (`$ref`, `additionalProperties`) and tool names starting with a digit, so a server that
  works with the OpenAI API can fail with Google's. Checking needs a Google key; the fix is either
  translating the schema before sending, or refusing at install time with an explanation.

- **A bare file name is a domain, not a path rule.** `--deny b.txt` is refused with a suggestion to
  write `b.txt*`, because a domain, a tool id and a file name are all bare words. Treating a bare
  word that isn't a domain as a path rule would turn a typo like `--deny contextt` into a rule about
  a file that silently matches nothing, so this waits for a syntax that's unambiguous.

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
  an endpoint that publishes them, read the way `Published` reads the rest. Under `--gemini` it
  shows nothing, since that listing puts its figures under `generationConfig`, and showing them
  needs `/params` to handle keys inside a parameter.

- **A tool call with unparseable arguments goes back to the model as `{"_unparsed": "..."}`.**
  That's how `nachalnik-providers` stores such a call, and `to_wire` sends it back that way. A model
  can copy it and wrap every later call the same way; `kamchatka` accepts a wrapper whose text parses
  as the call inside it, which stops that without changing what's sent. Sending back exactly what the
  model wrote would be more accurate, but some endpoints reject it, so one broken call would make
  every later request fail; sending `{}` works everywhere and leaves it to the tool result, which
  already quotes what arrived. Unblocked by a decision on what the history should say the model
  sent, and a live test of that choice on the strict endpoints.

- **A turn that gets 429 four times is abandoned.** Without a `Retry-After`, the waits double (two,
  four and eight seconds), then the turn fails and a headless run exits with `1`, to be resumed with
  `-r`. A rate limit shared by several sessions lasts longer. Waiting longer is bad for someone
  watching the screen, who'd rather be told; the open question is whether a headless run should
  wait out rate limits, since its `--deadline` limits the total time anyway.

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
