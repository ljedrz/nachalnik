# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### fixed

- A request that stalls is waited out, the way a busy server already was. A 429 or a 5xx got four
  tries and a doubling; a connection that timed out got `?` and took the session with it - which
  is the same event wearing different clothes. Eleven of fourteen runs against one upstream died
  this way while the same model answered a single request in six seconds; run one at a time they
  all passed, so what they had met was load, not a wall. A refused connection is deliberately not
  retried: an address with nothing behind it is an answer, and making a typo take four doublings
  to report helps nobody.

### added

- Every session is written out when it ends, to a temporary directory, and the path is the last
  thing printed - the way `/save` writes it, and without anybody having had to think of it.
  `--no-record` turns it off. The old condition was backwards: a session that ended badly is the
  one worth reading afterwards, and it was the one that left nothing. Nine runs against a provider
  that timed out left empty files and no way to see how far any of them had got. A session is now
  named `kamchatka-<seconds>` rather than a counter that restarts at 1 with the process, which is
  fine as an identity and useless as a filename; a resumed session keeps the name in its snapshot,
  so carrying on writes back to the same pair of files.

- `/params` shows what else the model takes, and names any you have set that it does not. A
  parameter a model does not accept is not refused - it is sent, ignored, and nothing says so, so
  a `seed` set for a reproducible run buys no reproducibility and looks exactly like one that
  worked. Two models compared one session apart differed by eight of them. The list is read from
  the same listing entry the context limit already comes from, so it costs no extra round trip,
  and an endpoint that publishes nothing is read as silence rather than as a prohibition.

- A `prune` that hides items while the agent has written nothing down says so. A run gathered
  ~19,400 tokens of evidence across seventeen tool results, said nothing in any of its own seven
  turns, elided all seventeen in one call, and then answered all ten questions from a context that
  no longer held any of it - confidently, and wrong on every one, inventing a crate and seven enum
  variants. The tool had told it what it saved (`~19,380` down to `~2,453`) and nothing about what
  it had just spent. It now adds one line naming `note` as the thing that would have kept a
  finding, and says nothing once a note is in context.

- `look` hands back a long item as its start and its end rather than the whole of it, and
  `whole: true` asks for all of it. Reading an item copies that item into the context, so a model
  asking to see a 9,000-token tool result *in order to decide whether to keep it* pays very nearly
  what keeping it costs. One did exactly that, twice, and finished a correct clean-up 7,688 tokens
  heavier than it started - it identified 9,324 tokens of genuine rubbish, removed them, and spent
  17,142 finding out. The sample keeps both ends, because what tells build noise from something
  worth keeping is usually visible at the edges, and it names the bytes it left out. Recovering an
  archived output whole is still possible, which is why this is an argument rather than a cap.
- Hiding an item says how to get it back, on the line where it says what it did. `amend` has one
  way back from all four of the states that hide or hold an item, and it is a `state` called
  `restore` rather than an `action` - which a session that had just elided twenty-two items could
  not find. It asked for an `action` called `restore` twice, was told no such thing existed, and
  gave up with its whole context hidden. Six words on the line that hid them is cheaper than that,
  and it names `undo` too, which is the better answer when the whole call was the mistake.
- `state` takes any of the words for putting something back. There is exactly one such state and
  a great many spellings, so `unelide`, `unexclude`, `unarchive`, `unpin`, `include` and `active`
  all reach it. They are accepted and deliberately left out of the schema's `enum`: a list of
  eleven words, six of them the same word, is harder to read than a list of five.
- The error for an unrecognised `state` says what each one *does* rather than only what it is
  called. Choosing between `elide` and `exclude` is the decision that settles whether a tool call
  keeps its answer, and five bare words never helped anybody make it.
- A prune that made the request *bigger* says so. An elided item leaves a marker carrying the
  reason given for eliding it, and on a short item that reason costs more than the content did: a
  live session elided twenty-two items and added 162 tokens. Both figures were already printed and
  a careful reader could work it out; three models in a row did not, and one went on to elide
  everything it had.

- The requests say which program made them, where the endpoint keeps a ranking of programs.
  OpenRouter builds an app's page against the `HTTP-Referer` it is sent and names it from
  `X-OpenRouter-Title`; without them a session is anonymous traffic, and [the crate's own
  page](https://openrouter.ai/docs/app-attribution) is the thing that goes missing. What is sent
  is this crate's own directory and the word `kamchatka` - not the key, not the model, not a syllable of
  what anybody asked - and it is sent **only to OpenRouter**, because `KAMCHATKA_BASE_URL` points
  this at anything and a `HTTP-Referer` volunteered to somebody's own machine is something they
  did not ask to send. The host is matched on its authority rather than by looking for the name in
  the address, so `openrouter.ai.example.com` is not it. `KAMCHATKA_NO_ATTRIBUTION` turns it off:
  a program that names its user's tooling to a third party should say so and let them stop it.
  The library type takes it as `OpenAiCompatible::on_behalf_of` and defaults to none, so anything
  built on the crate is not quietly filed under this one.

- <kbd>f</kbd> on the context tab lists only what the next request carries. After a compaction most
  of the pane is items the model will never read again - archived originals, elided markers, the
  superseded halves of rewrites - and reading past them to find the conversation is the thing the
  tab is for. The rule is one anybody can hold in their head: it hides every row with a figure in
  the `held` column. Nothing is changed and nothing is logged, because it is a view; the header
  says how many rows are missing and which key brings them back, and the selection follows the item
  it was on rather than the row number, since the rows underneath have just moved. Asking for a
  hidden item by number says it is hidden rather than that it does not exist, which are two
  different answers and only one of them is somebody's typo.

- `--forget-truncated`, which drops the whole of a tool's output once it has been shortened rather
  than keeping it as an archived item. The runtime has had the switch since it had the behaviour
  and its documentation says when to reach for it - "when a tool can produce more than you are
  willing to go on holding" - and nothing here reached it, so the answer for a terminal was always
  yes. It is worth a flag because the cost is not in the session, it is in the file: `/save` writes
  the snapshot, an archived output goes into it whole, and one `grep` that wandered into `./target`
  put 11MB of build noise into every save of that session from then on. Keeping it is still the
  default, because being able to open the item and read what the command actually said is the
  point of the pane.

### fixed

- Both readmes said the sandbox reaches further than it does. "Nothing outside that directory is
  reachable either way" and "nothing outside the working directory is readable or writable at all"
  are true of writing and false of reading: the system paths - `/usr`, `/etc`, `/bin`, `/lib`,
  `/proc` and the rest - are readable on purpose, because a command that cannot read `/usr/bin`
  cannot be a command, and `sandbox.rs` has said so in a note since it was written. Two live
  sessions read `/etc/passwd` through a confined shell with nothing refusing them, which is
  correct behaviour and was documented as impossible. A claim about what a sandbox stops is the
  last place to be loose, so both now say what is writable, what is readable, and give the two
  commands that show where the line is: `cat /etc/passwd` works, `cat ~/.ssh/id_rsa` does not.
  The file tools are unaffected - they refuse `/etc/passwd` by their own code, and always did.
- A test pins that boundary from both sides now. The one that sounded like it covered the claim
  reaches for `/home/*/.bashrc`, which is outside the system paths and so was never the case in
  question; it keeps its assertion and loses its name, and the case it appeared to cover has a
  test of its own.

- A tool call numbered from one left a phantom call at zero. The streamed `index` says which call
  a fragment belongs to; it is not a position in a list, and using it as one meant a first call at
  `index: 1` pushed an empty `PartialCall` into slot zero that nothing ever filled. That reached
  the kernel with no identifier and no name, was assigned one by the repair path, and came back to
  the model as `tool.unknown` for a tool called `""` - a wasted round trip every turn and an error
  it had to read and work around. Found by pointing this at `minimax/minimax-m3`, which numbers
  its calls from one. An index is looked up now rather than indexed into, so any base works, and
  so does a provider that skips a number.
- The same treatment for an error that arrives *mid-stream*, which is a different shape and was
  missed the first time. A refused request nests its sentence under `error`; a stream that fails
  halfway sends the object on its own with `message` at the top, and that path printed the whole
  thing. Found with `inception/mercury-2.5-preview`, whose upstream answers a question it does not
  like with a 502 whose message is the refusal - so the screen and the session log got
  `{"code":502,"message":"...","metadata":{"error_type":"provider_unavailable"}}` where one
  sentence would do. One function reads both shapes now.
- A refused request no longer copies the server's whole envelope into the transcript. A spent
  daily quota came back as six hundred characters of JSON - the message, the remedy, the
  rate-limit headers, and the account's `user_id` - and all of it went on the screen and into the
  session log, which is a file people send each other. What is reported now is the server's own
  sentence, plus the upstream's where the wrapper only says that something upstream failed, and
  the account identifier is not in it.
- `Retry-After` is honoured where the server sends one, instead of always doubling. The two are
  not the same question: a per-minute limit answers `5`, and a spent daily quota answers with the
  seconds until midnight. Past a minute this stops rather than sitting through four doublings to
  discover the answer will not change, and says how long it was asked to wait.
- `amend` points a state-named action at the argument it belongs to. `restore` is what `prune`
  puts an item back to, and `there is no ``restore``` was true of the action list and useless to a
  model holding the right tool at the wrong level - two live models in a row spent a call each on
  it and gave up. A word this tool knows anywhere now comes back with the call that would have
  worked. A word it does not know still gets the list.
- A figure too wide for its column stopped taking the columns from its neighbour. The `held`
  column is seven wide, which stops at `999,999`, and `{:>7}` pads without truncating - so an item
  holding 3,370,258 tokens printed all nine characters, ran into the `sending` figure beside it
  (`0` and `1,400,000` arriving as `01,400,000`) and pushed two characters off the end of the row.
  It is the one column where an unbounded number can turn up: what is being *sent* is bounded by
  the window it is being sent to, and what is being *held* is whatever a tool actually produced.
  The figure is abbreviated when it will not fit and exact whenever it will, which is almost
  always; the status line goes on reporting the whole of it, where there is room.

## [0.3.0] - 2026-09-01

### added

- `Careful` hands its reason for a refusal to the model as well as to the screen, through the
  runtime's new `PermissionPolicy::why`. It had written down which capability or path rule did it
  since the day it was built, and nothing carried it any further than the transcript: what reached
  the model was `the call was not permitted`, from which a standing `deny` and a one-off `n` are
  indistinguishable. The reason is no longer handed out once, because there are two readers now
  and whichever asked first used to get it.
- A marker that says the runtime is still working: three dots under `asking` or `running`, one of
  them lit and moving, joined after five seconds by how long it has been going. Which dot is lit
  comes from the clock rather than from a frame counter, so it moves at a steady rate whatever the
  screen is doing and stops where it is if the screen stops being drawn - `asking` on its own is
  the same word whether a request is in flight or the program is wedged. Absent while the runtime
  is resting, including while it waits on an answer from you.
- `/load [PATH]`, the other half of `/save`. `kamchatka -r` was the only way back into a saved
  session and it is a restart, which is right for what it does - `Kernel::resume` is a constructor,
  and a second kernel built inside a running one would arrive with no provider, no policy, no
  tools and none of the subscriptions the screen draws from. So this is a context operation
  instead, and it follows the rule the rest of them do: nothing is destroyed. The current context
  is archived, keeping its numbers and contents; pinned items stay, because a pin is the person
  saying so and `--system` is pinned; the saved items come in as new items and are read back onto
  the chat tab as the conversation they were; the parameters and the counter's calibration come
  with them; and `u` twice puts it all back. A saved file is now a checkpoint you can return to
  mid-session. Refused while a turn is running or a call is waiting to be answered.
- <kbd>enter</kbd> on a context item opens a paged box instead of a single body, moved between
  with `←` and `→`. `to the model` is what the item puts into the next request - read out of the
  projection of the *whole* context, so a call the projector dropped or an ordered turn it
  flattened shows as the repair it is, and an item that is not going says so with the reason.
  `as stored` is what the item holds. For an elided or an excluded item those are two different
  answers, and the box opens on the first one rather than the second, because the gap between
  them is what somebody pressed enter to find.
- `v1`, `v2`, … pages: what an item said before it was rewritten, newest first, up to eight deep.
  A terminal edit supersedes and leaves the old text a row of its own, but `amend revise`
  replaces in place - deliberately, so the model keeps the number it refers to the item by - and
  the old text then exists nowhere but the `context.replaced` event. `App` keeps what that event
  carries, which is what the event carries content *for*. No change to the runtime.

### fixed

- A stopped `shell` command lost the line saying so. `[the command was stopped before it
  finished]` was appended after the standard error, and an output limit cuts from the end - so
  the one command that most often has more to say than the limit allows was also the one whose
  explanation went, leaving output that stopped mid-sentence under a truncation marker with
  nothing to say why. It is on the exit line now, first in the result, where nothing can cut it.
- A markdown table wider than the window came apart. The renderer lays a table out at the width
  its contents want and hands back rows of box characters, and those were then wrapped like
  prose - so half a border arrived on the next line and the borders scattered across the pane.
  Tables are drawn here now, the way fenced blocks already were: the columns give when the window
  is short, widest first and down to a floor, the cells wrap inside them with their inline styling
  intact, and the delimiter row's colons decide which end of its column a cell sits at. A table
  inside a fence is still a code block.

### changed

- **Breaking, for the library:** `Overlay::Permission` is a struct variant carrying its own
  `scroll`, and `Overlay::Text` holds `pages` and `page` where it held one `body`. Both follow
  from the same discovery - that a box showing one thing had no way to admit there was more than
  one - and neither is expressible without changing the shape somebody matches on. The library
  exists so that the screen can be tested against a `TestBackend`; the program is the product, and
  it is unaffected.
- The trace says what its events carry. A third of them printed a dotted name against an empty
  line - including `context.replaced`, which holds the only surviving copy of what an item used to
  say, and `tool.repaired`, which is the kernel announcing that a provider reused a call
  identifier. They all say something now, and a test refuses a name with nothing beside it.
  `tools.changed` lists the tools rather than counting them, and `permission.decided` says who
  answered in words rather than in a `Debug` of the source.
- A column down the left of the trace holds the gap since the line above, blank under a tenth of a
  second. A log with no clock cannot answer the question people bring to one - which step was slow
  - and a column of timestamps would make them subtract to find out. Nearly everything happens
  between one frame and the next, so what is left with a number beside it is the model thinking, a
  command running, and however long somebody took to answer a question. It is dropped on a window
  too narrow to spare the columns.
- The context pane's token column reported what an item *held* under a heading that said what it
  cost, so an elided item claimed the nine thousand tokens it was no longer spending and the
  status line beside it disagreed by exactly that much. There are two columns now: `sending`, read
  out of the projection so that an elided item costs what its marker costs and an archived one
  costs nothing, and `held`, which is what it is keeping out of the request. They add up to the
  two figures on the status line.
- The label column is as wide as the widest label rather than a fixed twenty-six. A session whose
  longest label is `read` was spending twenty columns on nothing, and they belong to the column
  saying what an item holds.
- One word per mechanism. An output limit **truncates** and a compactor **elides**, and both were
  being called "shortened" - in the same pane, on adjacent rows. The archived half of a truncated
  result now says `the model was shown a truncated copy`, and `Trim`'s summary says its results
  were `elided`, which is the word on the row, the word `amend`'s `prune` takes, and the name of
  the state itself.
- The tool definitions are written for the thing that reads them. Every argument says what it is
  for - a bare `{"type": "string"}` left a model to guess whether a path was absolute, what `old`
  had to match exactly, what a `select` accepts, and a guess costs a turn each time. `read` and
  `shell` admit that long output is cut off; `write` says it replaces the whole file and points at
  `edit`; `shell` says it is confined **when it is**, because a command stopped by Landlock comes
  back with an ordinary permission error and a model that cannot tell those apart spends its turns
  trying `sudo`. The exit line reads `exit: 0` rather than `exit: exit status: 0`, and a non-zero
  one says it is a failure.
- Nothing a model reads is written in this program's own vocabulary any more: no `at the
  terminal`, which is this codebase's idiom for "a person did it here" and reads to a model like a
  state it should recognise. A test now holds every offered tool to all of it.
- A path outside the sandbox told the model to restart the program with `--sandbox-allow`, which
  is advice for somebody who can do that. It now says what the model can do instead.

### fixed

- The first line of a new session was wrong in both halves. `tab moves to the context` is
  something tab has never done - on the chat tab there is nothing to move the focus to, so it
  does nothing at all - and `ctrl+t swaps it for the trace` describes the second press, not the
  first. It is `ui::GREETING` now, beside `HELP` and for the same reason: a test checks that the
  keys it names do what it says they do, and that F1 lists every one of them.
- `ctrl+p` in a session nobody had typed into yet answered with `the context projects to an empty
  request`. That is the runtime's own sentence for a rule it is enforcing correctly - `step`
  refuses to send a request with no messages - but it is the wrong answer to "what would go
  next?" when what happened is that nothing has been said, and it reads as a fault. It now says
  there is nothing in the context yet and what puts something there. When the context is *not*
  empty and still sends nothing, the list of what was left out and why goes above the answer -
  which is the one moment that list is worth most, and exactly when it used to be thrown away,
  because the error returned before the list was built. `/request` and `/payload` too.
- A request that stalled sat at `asking` for ever without saying so. The heartbeat in both
  providers only made a silent stream *interruptible* - it woke up, checked whether escape had
  been pressed, and went back to waiting - so a server that answered the connection and then went
  quiet, which is what an overloaded one does, was indistinguishable on screen from a model
  thinking hard. A stream that has said nothing for ten seconds now says so, again every thirty
  after that, says when it starts again, and is given up on at a hundred and fifty. A turn that
  asks for a tool makes two requests rather than one, which is twice the exposure - and is the
  shape "it hangs whenever it uses a tool" really has.
- The retry budget for a busy server belonged to the session rather than to the request, so an
  afternoon that had already ridden out four `503`s answered the fifth by giving up on the first
  try.
- The conversation stayed where it was scrolled to. Every fragment of a streamed answer set the
  window back to following the newest line, so scrolling up to re-read something during a long
  turn lasted exactly until the next fragment arrived - which is to say, not at all. Only a
  message of your own moves it now; the chat tab's footer says how many lines have arrived
  underneath, `ctrl+e` follows again, and so does scrolling down to the end.
- A permission question whose arguments were longer than the screen lost its answers. The box was
  sized to its whole body and then clipped to what would fit, and what came last in the body was
  the line saying `y` and `n` were keys - so an `amend` carrying a rewritten tool result, which is
  as long as the result was, produced a question that could only be answered by guessing. It is
  three regions now: the header and the answers hold their rows, and the arguments scroll between
  them with `pgup` / `pgdn`, which until now moved the conversation hidden behind the box. Both
  overlays also remember how far they really scrolled rather than how many times a key was
  pressed, so four pages down past the end is no longer four pages back up before anything moves.

## [0.2.0] - 2026-08-30

A second wire format, in which a turn keeps its order, and two tools an agent reads and manages
its own context with.

### added

- `--gemini` talks to Google's own API instead of an OpenAI-compatible one, and the difference is
  the whole point: `generateContent` answers with `content.parts[]` - a thinking part, a sentence,
  a `functionCall`, in the order they were produced - and the compatible shim flattens that into a
  `content` string beside a `tool_calls` array, because the dialect it imitates has no order to
  report. `kamchatka/src/gemini.rs` records the order as `Content::Blocks` and sends it back the
  same way, with `LinearProjector::send_blocks` turned on to match. Streamed, with the same
  heartbeat that makes `esc` reach a request that has gone quiet.
  Signatures are the reason to bother beyond tidiness. This API answers `400 Function call is
  missing a thought_signature` to a request that returns a turn without one, and it signs text
  parts as well as calls; every part's own fields ride back out on the block they arrived on,
  unread. `finishReason` is deliberately not what decides the stop reason - it says `STOP` for a
  turn that asked for three tools - so the parts are.
- `provider::Endpoint`, the half of a provider the person at the terminal drives: where the
  requests go, what is served there, which model is being asked, what the last retry was about.
  `Provider` is the kernel's half and is one method. `App` now holds an `Arc<dyn Endpoint>` and
  never finds out which wire format is behind it, so `/model`, `/models`, `/provider` and the
  status line work the same against either.
- `introspect` and `amend` read an ordered turn. `look` reads a turn back block by block, marking the
  ones that came signed, because between two calls is where the thinking that led to the second
  one belongs and the request the model will be sent has it looking like a field instead. The
  guard that stops `amend` excising the turn it is speaking in now finds that turn by its calls
  wherever they are recorded - matching on the kind alone, it would have found an ordered turn to
  have asked for nothing, and quietly stopped holding.
  `enter` on the context tab reads out the blocks in order rather than only the text.

- `--introspect`, and `/introspect` while it is running, offer the model two more tools for
  reading and managing its own context. `introspect` reads: `look` lists every item with its
  state, its cost and the projector's own reason for leaving it out, and reads any of them in full
  - block by block, including what the model was thinking when it produced them. `budget` is what
  a decision about what to give up is made from: the next request against the limit, split into
  context and tool definitions, what the last one really cost as the provider counted it, the
  correction the counter has learned from the difference, and the most expensive items *actually*
  going into it - an orphaned tool result the projector repairs away costs nothing however active
  it looks, and offering it as something to elide would be advice that buys nothing. Items that
  are not the model's to move are marked as such, rather than costing it a refused call.
  `request` summarizes what would go next, and summarizes it on purpose: the request *is* the
  context, so quoting it would double every token being asked about. `draft` and `fork` snapshot
  the context, resume it as a second kernel with no tools and a limit of one request, ask it, and
  hand back only what it said - `draft` for reading your own answer before giving it, `fork` for
  putting a question to a copy of yourself with some items left out. A fork can think and cannot
  act, and nothing it does reaches this session's context or its log. None of it needed anything
  added to the runtime: forking a session is what `Kernel::snapshot` and `Kernel::resume` already
  are.
  `amend` manages: `prune` moves items between the states the context tab's `space` key moves
  them between, named by `ids` or by `select`, which takes the same selector language `/prune`
  does - so "the tool results I am done with" is one call rather than twelve numbers read off a
  listing, and a selector it gets wrong is answered with the whole grammar. `revise` rewrites what
  one item says (recording the old text as `context.replaced`, which is the one event that carries
  content, and the reason in the item's metadata). `note` writes something into the context - a
  plan, a conclusion, a thing not to try again - attributed to `agent` so the pane can say who put
  it there, and pinnable, because saying the same thing out loud in a turn is not a promise about
  anything and a pin is. `undo` and `redo` walk this tool's own changes. Deliberately not
  `Kernel::undo`: that stack belongs to the person at the terminal, and its top while a tool is
  running is always the assistant turn that asked for the call - one step would erase the model's
  own question and orphan the answer it is waiting for. A reason is required on every change and
  it is what the context pane shows. A pinned item, a system instruction and the turn the model is
  speaking in are refused, and the refusal is handed back to it; it may unpin only what it pinned
  itself.
  Two tools rather than one with a mode argument, because a `ToolSpec` declares its capabilities
  once for every call it will ever receive: one tool would have made "may it read its own
  context?" and "may it rewrite a tool result?" the same question. They declare `introspect` and
  `amend`, and the permissions tab grows a row for each without being told anything.
  Off by default. The tools hold a weak handle to a `Kernel` the `App` owns rather than a kernel
  of their own - the cycle the runtime's documentation warns about - so `/introspect` taking them
  away really does take their reach away, and what `amend` had been remembering goes with it.

## [0.1.0] - 2026-08-29

The first release: a terminal agent built on `nachalnik`, and a demonstration of it.

### added

- Four tabs, each taking the whole window: `chat`, `context`, `trace`, `permissions`. `ctrl+t`
  for the next, `alt+1` to `alt+4` for one in particular, `tab` between the prompt and the open
  tab. The prompt and the status line are under all of them; a message sent into a turn that is
  already running waits for the end of it, says so, and then goes in and gets a turn of its own.
  A long message wraps in the prompt rather than sliding sideways under the left border, and the
  box grows to hold every row of it: breaks fall at word bounds, so a path or a URL with no spaces
  in it is broken at a `/` rather than run off the edge, and a word too long for a row of its own
  is split. The box is sized by `ui::wrapped_rows`, which counts the rows the widget will draw -
  two pieces of code that have to agree, so a test asks the widget rather than trusting the
  arithmetic.
  A pasted block goes into the prompt as the lines it was pasted as: bracketed paste stops a
  pasted newline being read as `enter` and sending half of what was pasted, and the carriage
  returns a terminal spells those newlines with are put back, or the whole of it arrives as one
  line with invisible characters in it. A scrollbar runs down the right border of any tab holding
  more than fits, and of the overlays - drawn on the border rather than in a column of its own,
  so nothing gets narrower and a window with nothing to scroll looks exactly as it did.
- The context tab is a table: every item the runtime holds, its kind, what it costs, whether it is
  going into the next request, and what the model will actually read of it - or, for the ones that
  are not going, why not, in the projector's own words. `space` cycles how much of it the model
  gets - all of it, then a `…` marker where it was, then nothing, then all of it again - `p` pins
  it, `enter` reads the whole of it, `u` undoes the last change, and `23G` goes to the item
  numbered 23, the number every note names and every selector takes. The middle step is the one
  worth a key: taking a tool result out makes the projector drop the call that asked for it, and
  eliding it leaves the call answered, so which of the two somebody wants is a choice rather than
  something this program should be guessing at.
- `e` on a context item changes what it says, through `Kernel::supersede`: the original stays,
  marked `~`, naming the item that replaced it, and one `u` brings it back. `space` and `p` decide
  whether the model reads an item; this decides what it reads.
- The trace tab is every event as it happens, in the same names the session log uses, in two
  aligned columns, wrapped rather than cut off, and readable backwards.
- A `permissions` tab: every capability the policy has an opinion about *and* every capability a
  registered tool declares, what the policy will answer about each, and which tools that covers.
  `space` cycles a row through ask, allow and deny; `a`/`n`/`r` set one directly. The permission
  prompt writes to the same table, so "always" and the tab are one object rather than two - and a
  refusal is visible in advance rather than only when it fires. The tab lists the answers somebody
  has actually given and counts the rest along the bottom, since `ask` is what the policy does
  when it has not been told anything and a screenful of it buried the one or two lines that say
  what this agent can do without stopping. Cycling a row back to `ask` takes it off the tab, which
  is what taking a decision back looks like.
- **Every stance starts at `ask`**, `read` and `network` included. `read: allow` would have been
  the answer most people would have given and `network: deny` the cautious one, and both would
  still be answers given on somebody's behalf before they had been asked - by a program whose
  whole argument is that it does not do that. The tab starts empty, the first `read` is a
  question, and what is on the tab is what somebody decided.
- Permissions are finer than a capability where that is worth anything: `Careful` holds path rules
  as well as capability stances - `.env*`, `*.pem`, `id_rsa*`, `.ssh/` and a few more, all `ask` -
  and the strictest of everything consulted wins. Reading `src/main.rs` is silent; reading `.env`
  is a question, and stays one the moment `read` is answered `always`: the capability goes to
  `allow` and `.env*` does not. They bind `read`, `write` and `edit` and deliberately not `shell`,
  because a command names its files inside a string and a check over that string would refuse
  `cat .env` while waving `sed -n 1p .env` through.
- The permission question names everything the policy actually consults, and `[a] always` answers
  for all of it - including the calls already waiting behind it. A `yes, always` that answered only
  for the declared capability would ask again on the very next call, whether the question came from
  the network fold or from a path rule; and a model that asks for three commands in one answer
  produces three questions, all of them decided before the first is drawn, so an `always` that did
  not reach them would go back on itself one keystroke later. Anything still waiting that the
  policy would now let through is let through; anything that needs something else is still a
  question.
- A tool's arguments are shown as the lines they are rather than as `\n` inside a JSON string,
  since the permission question is the moment somebody has to read them; `[i]` still shows the
  JSON verbatim. A question that arrives while somebody is typing does not take their typing as an
  answer - its keys are ordinary letters - so the letters go on reaching the prompt until the
  typing stops. `d` drops every call the model is waiting on, with one reason, and the model is
  told: a call that silently vanished would leave it waiting.
- A call the policy refuses on its own says which stance refused it - ``shell: refused by
  `network`, which this command reaches for`` - because the tool result records only `the call was
  not permitted`, and when the tool's own capability is `allow` that leaves a refused call with
  nothing on screen accounting for it.
- Four tools (`read`, `write`, `edit`, `shell`) and a policy that asks about all of it. "Always"
  answers for a capability rather than a tool name, so it works for tools the program has never
  heard of. The `network` stance is consulted for a `shell` call whose command names a program that
  goes out to the network - `curl`, `pip install`, `git push` - because no tool declares
  `Capability::Network`, a model that wants the network writes `curl`, and a row reading `deny`
  beside `nothing registered needs it` would be a restriction that is not there. The policy's own
  documentation is plain about the heuristic being over the command as written rather than a
  sandbox.
- **The `shell` tool runs under [Landlock](https://landlock.io)**, so the permission stances are
  enforced rather than reported: `network: deny` is refused by the kernel at `connect()`,
  `write: deny` makes the working directory read-only, and nothing outside that directory is
  readable or writable either way. It is applied by re-executing this program in a mode that
  confines itself and then *becomes* the command: Landlock restricts the calling thread, and a
  single-threaded helper is the shape that needs no thought about which one. The `exec` matters -
  the domain is inherited across it, so nothing is given up by leaving, and the process a stopped
  call kills is the command rather than a helper standing in front of it. A directory of the run's
  own is handed over as `TMPDIR` and `/tmp` itself is not opened up; the spawning process removes
  that directory afterwards, being the only one of the two that can, since unlinking a directory is
  a write to the one it sits in.
- `read`, `write` and `edit` are held to the same boundary by their own code, resolving `..` and
  symlinks before comparing. Weaker in kind than a ruleset, and said to be.
- `--sandbox-allow PATH` opens up another path, `--no-sandbox` turns the whole thing off, and the
  permissions tab says which of `shell: confined` and `shell: a command can do any of these` is
  true here - the second of them also while a registered tool that runs commands is not refused
  outright, since `Capability::Shell` subsumes every other capability and a tab that listed five
  verdicts without saying so would be reporting four restrictions that are not there. The binary
  that confines a command is settled once at startup rather than asked for per call, and if it
  cannot confine, the shell runs unconfined and the tab says so.
- The model's answers are rendered as markdown - headings, emphasis, inline code, lists and
  fenced blocks - because a terminal that printed the asterisks would be showing the punctuation
  instead of what it meant. `tui-markdown` does the parsing; the styling is this crate's, since
  the defaults put a coloured slab behind headings and code, which reads as a redaction on a dark
  theme and a bruise on a light one. Nothing else is treated as markdown: a tool's output is what
  the tool said.
- Fenced code blocks are syntax-coloured, by token *name* rather than by theme: `synoptic` says
  which pieces are comments, strings, keywords, numbers and calls, and this program picks the
  colours. The fences are split out before the markdown renderer sees them, which is what makes the
  language, the whole block and the rule down its left all available at once - a block still
  streaming in is a block, and one in a language nothing recognises still gets the rule.
- Nothing is drawn against a background this program does not know it has. The secondary things -
  why an item is not being sent, what an event says, the tab headers, the status line - are `Gray`
  rather than `DarkGray`, which is the terminal's bright *black* and sits a shade off the
  background on many themes; `DarkGray` is left to the things that draw lines rather than words.
  The selected row of a tab the keys are not on is underlined rather than backed by a slab of some
  guessed-at colour.
- `/step` performs exactly one transition of the state machine instead of a whole turn, which is
  the only way to stand in `State::Ready` - the moment the model has said what it wants to do and
  none of it has run. A turn walks through that state without ever drawing it.
- `/seams` says what is plugged into each of the runtime's six parts, asked of the kernel rather
  than restated from what this program set up: the provider, the tools, the policy, the projector,
  the counter and the compactor - or that no compactor is installed and nothing will ever be
  dropped to make room.
- `/tools drop ID` stops offering a tool from the next request onward, because the kernel's
  registry is live rather than fixed at startup. `/prune` with no selector prints the language
  rather than reporting that the empty string is not a selector.
- `ctrl+p` heads the request with what the projector left out and what it repaired, because "why
  is that not in there?" is the question somebody opens it to answer.
- `/budget`, and a `~` on the status line's estimate beside what the provider really charged: the
  runtime's counter corrects itself from the difference, and this is where that is visible. It asks
  whichever counter is installed what it has learned, through the kernel, rather than keeping a
  typed handle to one this program set up - and a counter that never corrects itself says so in a
  sentence rather than leaving the line out.
- Cooperative stopping on `esc`: the provider returns what it had streamed, the shell tool kills
  the command - and everything the command started, since it runs in a process group of its own -
  and still answers the call, and the partial turn is an ordinary context item. Both of them wait
  on a heartbeat rather than on the next byte, so a model or a command that has said nothing at
  all is as interruptible as a chatty one.
- A compactor that shortens the oldest tool results to a marker past `--compact` of the limit, and
  is refused anything pinned. It elides rather than removes, so the call each result answers keeps
  its answer: removing them would have the projector take the calls down too - a call with no
  result is a request most providers reject - and the model would have been reading a conversation
  in which it never asked for any of this, directly above a summary saying the results had been
  dropped. Nothing is deleted; every one is on the context tab marked `…`, holding every byte it
  held, and restoring it is a keystroke. The tab's footer counts them separately, because "going"
  and "not going" is the wrong question about an item that is in the request without being read.
- MCP servers with `--mcp '[name=]cmd args'`, behind the default `mcp` feature. The name prefixes
  the server's tools and is what an "always" grant is for, so it is worth giving: taken from the
  program it would be `npx` for most of them.
- `/model [ID]` and `/provider [URL [ID]]` show or change the model and the address its requests go
  to, without restarting. The second takes a model too, since a model belongs to the address that
  serves it, and given none it keeps the name and asks the new endpoint whether it has one by that
  name rather than leaving a 404 for the next request. Both are shown, because the same model name
  at a different address is a different model, and a comparison that cannot see the address is a
  comparison of names. The key is not changed with the address: it is read from the environment at
  startup, and a key typed at the prompt would be a key in the transcript.
- `/models [FILTER]` asks the endpoint what it serves, marks the one in use with `▸`, and takes a
  filter because fifty-four of them is not an answer. The ids belong to the address rather than to
  the model - the same thing is `google/gemini-3.5-flash` at one and `gemini-3.5-flash` at another
  - so `/model` was a command you could only use if you already knew what to type, and after a
  `/provider` you did not. The provider had always fetched this list, to say when a model is not
  on it; this is the same call with the answer shown rather than checked.
- The status line carries the host beside the model name - `gpt-4o-mini @ openrouter.ai`,
  `qwen3-coder @ localhost:11434` - so the address is there without being asked for. Naming it only
  in `/model`, `/provider` and `/seams` meant a session pointed at a local model drew exactly like
  one talking to a hosted one, which is the confusion the paragraph above says it is avoiding. The
  host alone, since the rest of the URL is `/provider`'s to show and there is no room for it here.
  Where even that does not fit, the address is what gives way rather than the figures beside it:
  shortened with a `…` while enough of it is left to recognise, and dropped below that. The line
  is drawn without wrapping, so anything past the right edge is gone, and what sits at that end is
  the one number on it this program did not estimate.
- **The TLS is `rustls` over `ring`, and building it needs nothing installed first.** In reqwest
  0.13 `default-tls` means rustls with `aws-lc-rs`, which is 1,659 C files and, on some platforms,
  cmake and NASM - so `cargo install kamchatka` was asking for a build toolchain nobody had been
  told about. `ring` is 17 C and assembly files and no system libraries. It is a smaller surface
  rather than none: the pure-Rust providers are unaudited, which is not a trade a program that
  talks about sandboxing should make.
- `--help` lists the environment as well as the flags. `KAMCHATKA_MODEL` was there, because it is
  declared to `clap`; `KAMCHATKA_BASE_URL` and `KAMCHATKA_CONTEXT_LIMIT` are read directly and so
  appeared nowhere the program itself would tell you about - and the base URL is the one somebody
  running a local model needs before anything works at all.
- `/save PATH` writes the event log and a resumable snapshot beside it; `-r PATH` picks it back
  up in a fresh process. Both take a path you chose, on your disk - there is no session id, no
  server, and nothing to look up. Saving over files that already exist says which ones it
  replaced.
- Tested by drawing the screen into a `TestBackend` and reading the characters back, against a
  scripted model - including that an item taken out of the context really does leave the next
  request.
