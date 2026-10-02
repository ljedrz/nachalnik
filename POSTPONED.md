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
  gone: a budget now says how many pieces it could not price, and `kamchatka`'s compactor takes a
  picture as soon as the model has been shown it.

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

- **Whether `laya-serve`'s `confidence` is the one `system1::Client` reads.**
  [`laya`](https://github.com/NandhaKishorM/laya) is reached over HTTP: `laya-serve` answers
  `POST /v1/systemone`, and
  `KAMCHATKA_SYSTEM1_BASE_URL=http://127.0.0.1:8000/v1` beside any `KAMCHATKA_SYSTEM1_API_KEY`
  reaches it through `system1::Client` - RUNNING.md has the commands. The script that spoke to it over a pipe
  is gone, and with it what that script did to laya's answer: it built the answer rather than
  passing laya's through, because laya's `confidence` was its own quantity and read as this
  program's it drew every command yellow; and it refitted laya's temperatures on sixty labelled
  commands, raised its token budget and chose its checkpoint by script alone.

  `laya-serve` passes `predict()` through and says that is the TypeSafe SDKs' shape, which is the
  one OpenRouter's System One route takes too. Whether its `confidence` is the one the client reads
  has not been tried against a running server, and is the thing to check first. If it is not, the
  recalibration belongs upstream or in the client, and which is the decision. The script and its sixty commands are in git history, last at `bcb9a2df`, for
  whoever takes the working upstream.

- **Naming this program to OpenRouter when the *advisor* is what is calling it.** The System One
  client sends no app headers, so a session that asks OpenRouter for advice is attributed for the
  conversation and anonymous for the advice, out of the same account on the same service. The
  headers themselves are a solved problem - `OpenAiCompatible::on_behalf_of` and `filed_under`
  build them, and `kamchatka::endpoint` already holds the URL, the title and the categories to
  pass.

  What stops it being three lines is that `Attribution` is an inherent part of one client, and the
  System One client is deliberately not a `Dialect`. Two unrelated clients now want the same pair of headers, and
  copying them onto the second would be the third copy of one thing in this workspace, which is what
  `is_openrouter` was consolidated out of. So what would unblock it is deciding **where the pair
  lives** now that it is not one client's business: a builder the crate offers, rather than a method
  each client grows.

  `KAMCHATKA_NO_ATTRIBUTION` has to cover both the day it does, and as one switch. Somebody who
  turned attribution off for their conversation has not agreed to be named by a second client on
  the same account, and two switches would be a way to be half off without noticing.

  Worth knowing for whoever picks this up: it is only ever half applicable. An engine of one's own
  keeps no ranking of the apps calling it, so a client pointed at one has nothing to send and must
  not send it - `is_openrouter` is already the test for that, and it is the same test the request
  path uses.

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

- **The first undo of a fresh session takes its setup back.** `--system` and `-f` are pushed onto
  the context like anything else, so each is an undo step: `/undo` on a session nobody has typed
  into yet takes the system instruction out of the context, and a second takes the attached file,
  each said only as `undone`. A resumed session says there is nothing to undo, because
  `Kernel::resume` restores its items without a checkpoint. A host cannot do the same for its own
  setup - the bounded history cannot be read from outside, and an undo followed by a redo is two
  records for nothing - so what would unblock it is a kernel operation that starts the undo
  history afresh, which is a core addition for a client's sake.

- **An output limit that does not know the window.** A call's output is cut at a fixed number of
  bytes - 32,000 for `fs:read` - whatever the model's context, and the compactor never takes the
  turn in progress. Against a 14,000-token window one read is over 8,000 tokens: a turn of two
  reads fills the context past where the full notice leaves any room, the model's next call
  takes it over the limit, the request is refused, and a headless run passes every message
  after it over. Either half would settle it, and both change what the program does: limits that
  scale to a share of the window, or the compactor taking what the model has already been shown
  in this turn once the request would otherwise be refused.

- **`context`'s `revise` reaches the person's own messages.** What it refuses is a system
  instruction, the person's pins and the turn it is speaking in, so a message the person typed
  can be rewritten: a model that meant to revise its note and gave the id of the message asking
  for it replaced that message with the note's text. Nothing is lost - `context.replaced` keeps
  the old words and an undo brings them back - but every later request reads the person as having
  said something they did not. Refusing it is one more case in `protected`; whether the model may
  tidy the person's words at all is the decision.

- **`/params` showing a parameter's type and range.** It shows the default and the maximum where
  the listing publishes them, and nothing else, because nothing else is published: OpenRouter's
  `/models` and its per-model `/endpoints` give names, some defaults and the cap on an answer,
  and Inception's gives names. A table of types and ranges kept in this workspace would be a
  gateway's documentation as of the day it was copied, said of whichever model is behind it -
  the restriction invented out of silence that `/params` already refuses to invent. What would
  unblock it is an endpoint that publishes them, read as `Published` reads the rest.

  `--gemini` says nothing beside its parameters at all. Its native listing has `temperature`,
  `topP`, `topK`, `maxTemperature` and `outputTokenLimit`, but no list of what a request may
  carry, and those five go under `generationConfig` rather than beside it, so there is no list
  for `/params` to put them on. Reading them needs `/params` to say something about a key inside
  a parameter, which is a decision about the command rather than the dialect.

- **The advisor's rubric is worded for the models it was written against.** `tests/advise.rs`
  passes whole against TypeSafe's and Liquid's models on OpenRouter; others place a command a level
  away from what it asserts - `rm -rf target` drawn red rather than yellow, `cd /tmp && ls` yellow
  rather than green - and one reads every stage of a chain by the whole command, so no stage is
  underlined. Respan's answer none of it, taking only plain `noul` questions about a conversation.
  None of that is the client's: every model that answers gets the same request and is read the
  same way. What would unblock a change is a decision about **whose reading the wording is for** -
  rewording the levels and the claim until the suite passes across the list, measured with that
  suite, or naming in RUNNING.md the models it has been checked against and leaving the rest to
  whoever picks one.

- **A reasoning count of zero the OpenAI dialect reports is read as none.** `usage_of` keeps
  `reasoning_tokens` only when it is above zero, so an endpoint that says
  `"reasoning_tokens": 0` - the model reasoned, for nothing - comes back as `None`, which
  `Usage::reasoning_tokens` documents as the provider not having said, "not the same as zero and
  must not be shown as it". Gemini's dialect keeps a reported `thoughtsTokenCount` of zero as
  `Some(0)`, so the two disagree on the same fact. The filter does earn its place on the figure
  inferred as a residual, where zero is no evidence of reasoning at all. Keeping a reported zero is
  a one-line change, and it changes what a client shows; whether a zero from a model that cannot
  reason is worth showing is the decision. `cargo mutants` found it: `> 0` replaced with `>= 0`
  survives every test, and a test pinning either answer would be choosing one.
