# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- `/limit`, which is how much of each tool's output the model is shown - and now something a
  person can change. `/limit` lists the table; `/limit read 64000` moves one, from that tool's
  next call onward. `Tool::spec` is called afresh for every request, so it lands without a
  restart, the same property `/tools drop` leans on; the limits live in one shared `Limits` table
  that the tools declaring them and the command changing them both hold, because a second copy is
  a command that reports success and does nothing.

  The rows are numbered and the number is one the command takes, so `/limit 3 64000` is the same
  instruction as naming the tool - `introspect` is eleven characters to reach the one limit that
  most often wants moving. A tool has no identifier but its name, which is what the model calls
  and what `/tools drop` takes, so the number belongs to the listing rather than to the tool; that
  is exactly why it is only worth printing if it can then be typed, and a row out of range is
  answered by the same listing a name nothing limits gets, since that listing is where the range
  is written down. Same argument as `23G` on the context tab, settled the same way.

  It exists because of a session that asked a copy of itself three questions and got back the
  copy's deliberation with all three answers cut off the end. 32,000 bytes is right for the four
  other things `introspect` does and wrong for a fork, and there was no way to say so without
  restarting - so watching a result arrive shortened left a choice between living with it and
  losing the session.

  It changes the *next* call and says so, because the one already shortened is recovered a
  different way and always could be: its whole is archived beside the copy the model was shown,
  and one `space` on the context tab sends that instead. That is safe rather than merely possible,
  because the projector answers one call with one result: the whole claims the call and the short
  copy drops out with a repair line saying why.

- The conversation says which of itself the model is still being shown. A turn that has been
  excluded, archived, elided or superseded keeps its place and its words, and takes a rule down its
  left with a line above it naming the item and saying why it is out in the projector's own words -
  `~ [2] superseded: replaced by item 3`, and then the turn.

  Marked rather than hidden, which is the whole decision. The conversation is the record of what
  happened and the context is what will be sent; a chat that quietly dropped the turn would let
  somebody see what the model sees and lose what they did to it. Both halves of a turn are marked,
  what it thought as well as what it said, and the rule runs the length of the block rather than
  sitting on its first row.

  The reading asks the projection and not the item's state, for the reason `Going` exists at all:
  an item the projector repaired away is `Active` and is not in the request. The mark and the
  reason come out of the context tab rather than being assembled again here, so the two screens
  cannot end up giving different accounts of the same item.

  `app::Entry` grew `item` and `was` to carry this, and its fields are public, so anything
  constructing one literally will need them. A line nothing attributed shows unmarked: `None` there
  means nothing knows, not "not going".

### changed

- A session is named for when it started, in UTC, and that name is also its two files:
  `/tmp/kamchatka/2026-09-08T06-45-17Z.jsonl`. It was `kamchatka-1788849917`, written into a
  directory called `kamchatka` - so half of every filename repeated the directory it was in, and
  the other half said nothing whatever to somebody reading a list of them. The same name is what
  the last line printed on the way out says, what `Event::SessionStarted` carries and what a fork
  hangs `#fork` off, because it is one identity rather than a filename with a label beside it.

  `App::session_stamp` is public and does the calendar itself, in Howard Hinnant's
  `civil_from_days` - five lines of integer arithmetic that get the leap years right for every
  year rather than for the ones a test happened to try, and no dependency for a filename. UTC, and
  the name says `Z`, because a local time needs the timezone database to work out and would mean
  something different depending on where it was written. Still to the second, so two sessions
  started inside one second collide exactly as they did before.

- A permission question stands in the prompt's place on the chat tab instead of being an overlay
  over the middle of the screen, and the prompt is on the chat tab only. The two go together: a
  question was modal, so while one was up nothing else worked, and being asked whether `amend` may
  elide item 22 meant deciding about item 22 with the box asking the question covering the list
  that says what item 22 is. `App::about` exists because of that - it copies the items' labels into
  the question, because they could not be reached any other way - and it is a convenience now
  rather than the only route. The chat tab goes red on the strip while one waits, so the other
  three say what the session is waiting for.

  Dropping the prompt from the other three tabs is what makes the keys unambiguous. It was under
  all four so that a message could be sent from anywhere, and the cost was a mode: every letter on
  those tabs was a key or a character depending on where the focus had got to, and `space` after
  sending a message typed a space instead of cycling the row somebody was looking at. Now
  context/trace/permissions have no prompt and no mode, `tab` from any of them is the way back to
  typing, and `Focus::Body` on the chat tab means the waiting question.

  The settling window is gone with it, and so is the failure it patched. A question used to take
  every key on arrival and hand back the ones that were not answers, with a 300ms timer deciding
  which - one live session granted `shell` for the rest of it with the `a` of "what". Nothing is
  timed now: a question appears without asking for the keys at all, and `tab` is what gives them
  to it. So does coming back to the chat tab while one waits, because that is what the trip was
  for - go and read the item, come back, one key.

  It takes the prompt's rows rather than sitting above them, which is the second half of the same
  decision and was the second half of the same bug. Stacked, the two disagreed on any window
  shorter than about fifteen rows: the question needs the room, so the prompt gave way - and went
  on holding the keys, and whatever had been typed into it, from off the screen. That is a session
  waiting on an answer nobody can give it without first pressing a key nothing on the screen
  mentions, and the box that says `· tab` is the one telling you to press it. Now the box holding
  the keys is always the box on the screen, at every window size.

  What was typed is not lost. The prompt is not drawn rather than cleared, so answering hands its
  place back with the draft still in it and the keys already on it - no second `tab`. What it costs
  is that a message cannot be *sent* while a question waits, which is the honest shape of "answer
  this first": the queue a message typed into a running turn goes into is still there, and a turn
  that has stopped to ask is not running.

  `App::locked_key` is the guard, and it is a guard rather than a consequence of the layout. Until
  `tab` is pressed the only keys that do anything are the ones that scroll the conversation, since
  reading is not answering and the whole reason the question is not modal is so somebody can go and
  look at what it is about. Everything else is swallowed - the answers, because they are bare
  letters and that is the `a` of "what" again, and `enter`, because a prompt that still took it
  would send the half-written message the question interrupted and start a turn on the way to
  answering. The panel says so where it is read: until it has the keys it carries a `[tab] puts the
  keys here, and then:` line above the answers, because listing `[y] once` beside a `y` that is
  being deliberately ignored is a screen promising a key it has not got.

  A blank row separates the answers from what the tool was asked to do, since `path: /etc/hosts`
  and `[y] once` on consecutive rows read as one list of things rather than as a question and the
  ways of answering it - and the header was already separated from the arguments this way, so the
  answers were the odd ones out. It is a row of the layout rather than a line of the answers, which
  is what makes it the first thing to give way: in the answers it would be the top line of the one
  region that gets its rows before anything else, so a panel with a single row to spare would have
  spent it on a blank and pushed `[y] once` off the bottom.

  It is also one less thing to keep in step: the panel is drawn from `pending_permissions()` every
  frame rather than from an `Overlay::Permission` that had to be opened and closed, so it cannot be
  up with nothing to answer or absent with something waiting.

  This takes public API away, so the next release is a minor: `app::SETTLING` and `App::open` are
  gone, and so is the `Overlay::Permission` variant. `App::asked`, `App::prompted` and
  `App::question_scroll` are what replaced them. Nothing in this workspace sits above `kamchatka`,
  so no other crate has to follow and nothing is forced today - but a `0.5.1` published with this
  in it would be resolved by every `^0.5` requirement out there and break at the match.

- The permissions tab says which policy is deciding, and what it answers about everything the list
  does not mention: `Careful · anything it has not been told about: ask`, above the rows and there
  whether or not there are any. The tab was every answer somebody had given and no account of what
  was deciding in between - so the first question a screen of permissions raises was the one thing
  not on it, and answering it meant reading `/seams` for the name and the source for the behaviour.

  Both halves come out of the policy rather than being written into the screen. The name is what
  the kernel answers when asked, which is the same answer `/seams` gives and the one that would
  notice if the policy were ever swapped; `Careful::untold` is new, and is the value the two arms
  of `Careful::stance` fall back to, so a sentence describing this policy cannot come to disagree
  with what it does. `App::policy_name` shortens the path - `PermissionPolicy::name` defaults to
  the implementing type's own, which is right for a panel whose subject is which types are plugged
  in and spends thirty columns of a list saying `kamchatka::tools::Careful`.

  With nothing decided the tab now says the emptiness is not permission - `nothing has been decided
  yet, which is why this list is empty rather than permissive` - and goes on to the part that is
  not guessable: a fresh policy holds a rule for each of a handful of paths that are credentials by
  convention, those are questions too and so are not rows either, and they begin to earn their keep
  the moment a capability is answered `always`, because the strictest thing consulted wins and a
  rule can only tighten what a capability allows. It names no paths. Three of the eleven read as
  the list, and the count along the bottom is already the honest answer to how many there are.

- An item's page says why it is in the context, which is now a sentence that exists. `enter` on a
  context row shows what the model gets and what the item stores; the `as stored` page now opens
  with `included_because` where there is one, which is the same line `introspect`'s own item view
  has printed all along. What fills it in is the runtime keeping a shortened tool result's pointer
  to its whole half somewhere a state change cannot wipe - so `space` on either row no longer
  loses which item holds what.

- `/budget` says how much of the last request the provider served from its cache. Both dialects
  have reported it all along - `prompt_tokens_details.cached_tokens` and
  `cachedContentTokenCount` - and nothing read it out to anybody. It belongs beside the real cost
  because it is the figure that prices a *change* rather than a request: the front of a request is
  the tool definitions and the oldest messages, so anything that rewrites them is paid for in full
  on the next one. A session reading `20,000, 18,000 of it (90%) served from the provider's cache`
  is being told what a rewrite up there would cost, which is the number that settles most questions
  about whether one is worth making.

- `amend`'s `note` says what it is for, which is the question it kept prompting: how is writing a
  note different from thinking? Four ways, and the description and the code now say them. Thinking
  belongs to the turn that produced it, so pruning the turn prunes the thought; it has no
  identifier, so it cannot be revised, pinned, or protected from a compactor; it is not reliably
  carried back at all - this program's OpenAI-compatible dialect has never put reasoning on the
  wire and cannot - and it is not a row on the context tab with a reason beside it. A note is an
  item: numbered, projected into every request from then on, pinnable, and visible to the person.
  It is the one thing in a context that is there because the agent judged a finding worth keeping.

- A fork leads with the answer and puts the thinking after it. An output limit cuts from the end,
  and on a reasoning model the thinking is the bulk of a fork: measured on one real 34,287-byte
  fork, 68% thinking against the answer's 30%, sitting last. So the limit ate the answer and kept
  the deliberation about how to answer, which is the one part nobody asked for. The section is
  still labelled, and now says the reasoning came before the answer above it, so the order is a
  decision about what survives a limit rather than a claim about what the copy did.

- An edit reads where the turn was. It used to leave the turn it replaced sitting in the
  conversation with a note underneath saying the numbers had changed, and never show the words the
  model had actually been given; now the lines move onto the item that replaced them and the row
  above says what happened - `~ [2] → [3] · edited here, 13 tokens replaced · enter on [3] reads
  what it said`.

  Saying the new text instead is the obvious version and it is wrong. `say` appends, so a turn
  edited twenty exchanges ago lands after everything that followed it and the only account of the
  session is then in an order no request ever had. It reads fine for the turn just taken and lies
  about every older one, which is backwards: the older the edit, the more the screen has to be
  trusted.

  Only the line that showed what the turn *said* takes the new words. An edit carries the kind over
  whole, so the calls and the thinking are unchanged and the lines showing them are still true;
  what they need is the new identifier, so that excluding the edited turn later takes them out with
  it. Nothing is hidden by this - `commit_edit` already filed the old content under the *new*
  identifier and `faces` builds it into a `v1` page, which is what the row now points at.

  Replaying a saved session is deliberately left alone: `retell` is handed every item including the
  superseded ones, and a session read back off disk is a record rather than a conversation.

### fixed

- Nothing this crate's test suites write goes in `/tmp` any more. Every one of them cleared its
  directory on the way in rather than on the way out - a test that fails is a test whose leavings
  you want to look at - and the name carried the process identifier, so a fresh directory arrived
  with every run and none of them ever left. Thirty runs had put eight hundred and ten of them in
  `/tmp`. `tests/common::scratch` hands out a directory under `CARGO_TARGET_TMPDIR`, which cargo
  provides for exactly this, is inside `target/`, and `cargo clean` sweeps. The names lost the
  identifier with the prefix, since one only has to be unique within its own suite and a stable
  one is what somebody debugging a failure can find.

  One path stays in the temp directory and has to: the claim it checks is that the temp directory
  is *not* opened up even though a writable directory inside it is handed to the command, and a
  path under `target/` would be testing something else. It leaves nothing behind, because the
  write it makes is refused.

- Two sandbox tests stopped leaving a confined command's scratch directory behind. The directory
  is named after the command's own process so that whoever spawned it can find it again - the
  command cannot remove it, `/tmp` not being writable under the ruleset - and these two called
  `output()`, which consumes the child, and then removed `scratch_for(std::process::id())`: the
  *test's* identifier, naming a directory that never existed. So each run of that file left two
  behind for good, which is the sixty `kamchatka-<pid>` directories that were not from the suite's
  own workspaces. They spawn and wait now, the way this file's own `run` helper does and documents.

- A repair the request needs every time is said once in the conversation rather than after every
  message. A projection is built afresh for every request, so a projector that dropped an orphaned
  call last turn drops it again this turn and honestly reports doing so - which is right of the
  projector and wrong of the screen: one tool result taken out at the terminal put
  `the request was repaired: dropped the call ... from item 4` under every answer for the rest of
  the session, for a decision made once and unchanged since.

  All four kinds behave this way, which is what makes it worth fixing rather than special-casing:
  an orphaned call, an orphaned result, a flattened ordered turn and a result held back until its
  call arrives all last exactly as long as the state that caused them. So the conversation says
  what is being repaired when the set of repairs changes, and the count it gives is the whole of
  it rather than what is newly so, because that is the number `ctrl+p` will show.

  The wording moved to the present tense - `the request is repaired, and will be while this
  stands` - because the past tense reads as something that happened to this one request, which is
  exactly what somebody then goes looking for a cause of in a turn that has nothing to do with it.

  The trace is the other way round and stays that way: it keeps every one of them, because
  `model.requested` really did carry that repair each time and a log that hid a repeated entry
  would be the wrong thing entirely. Nothing about any of this ever reached the model - a repair is
  an `Event`, and `Event::ModelRequested` names the items a request was built from rather than
  carrying its messages, so the sentence exists on the screen and the log and nowhere on the wire.

- The chat tab looks as open as the other three. The window border went yellow when the keys were
  on the tab's body, and on the chat tab they never are: `Focus::Body` there means the pinned
  question, which has a box of its own. So the tab most of a session is spent on was the one window
  that could not light up, and it had nothing else yellow on it either - an unfocused frame reads as
  "this is not where you are", which of the four screens it is the least true of.

  The border is the frame of the open window now, drawn in the same yellow the open tab's name
  already wears on the strip above it. That is the one thing it is agreeing with, and there was
  nothing else left for it to say: what has the keys *within* a window is said by the box that has
  them, and on the two list tabs by the selected row, which is reversed under the keys and
  underlined without them. Both of those sit beside the thing they describe, which a border a whole
  window away does not.

  So the prompt is what answers "where does what I type go?", and it now answers in the same yellow
  the pinned question uses rather than in white - which against grey is a difference in brightness
  rather than in hue, the weaker of the two signals and the first to go on a pale theme. An edit was
  yellow whether it had the keys or not, which was that colour doing a second job; what says the
  prompt is not composing a message is its title, which spells the whole of it out, and which now
  gains `· tab` when the keys are elsewhere the way the other two titles do.

- An edit that has been undone comes off the conversation with the item it named. The chat's
  account of an edit was written into the transcript when the edit was made, and `undo` takes the
  replacement item back out of the context without telling the screen which line had been moved
  onto it - so the conversation went on showing the new words beside a row offering `enter on [3]`
  for an item that no longer existed, while the context tab beside it had the original answer back.
  Showing somebody a conversation the model is not in is the one thing this program exists not to
  do.

  It is a reading now rather than a copy, which is the same move the withheld mark made for the
  same reason: an edit is a fact about the context, not about the transcript. `Entry::text` keeps
  what was said at the time and `App::said` asks the item what it says now, so the line follows an
  undo and a `redo` both - and a row that cannot be opened is never drawn. `App::said` and
  `App::edit_of` are public, beside `Entry::was` which they read.

- `cargo doc` builds again. Four intra-doc links added with `Entry::item` and `Entry::was` name
  private methods from public documentation, which rustdoc refuses under `-D warnings` - so the
  lint job was red and the two commits that added them did not run it. They are plain code spans
  now: a reader of the public docs could not have followed them anyway.

- `Trim` does not ask for a result that is pinned. `ContextState::sends_content` says yes to a
  pinned item - it is in the request, that is what the state is for - so the candidate filter took
  one, and the kernel then refused it, as it must: a pin is a promise. That refusal is not free.
  The plan was still a plan, a plan carries a summary, and one pinned result bigger than the target
  keeps the context over the threshold for the rest of the session, so the pass is asked again
  before every request and refuses again every time. Measured against a real endpoint: three turns,
  three summaries saying a result had been elided when none had, three undos spent, and the request
  climbing 2,637 → 2,716 → 2,788. Not naming what it may not take is what makes the plan `None`
  instead. The kernel no longer banks a summary for a pass that moved nothing either, which is the
  same hole from the other side and closes it for any compactor.

- `/budget`'s account of what the counter has learned drops its percentage. It read "so it was
  reading 54.3% low", computed from `scale - 1`, which is the error as a fraction of the counter's
  own guess - while "reading 54.3% low" is read as a fraction of the truth. On the same pair of
  numbers those are 54.3% and 35.2%, so the sentence asserted one and meant the other. The guess,
  the charge and the scale between them are what somebody came to the line for, and all three were
  already on it.

- `amend`'s list of the five moves does not claim a difference the budget does not make. `archive`
  read "keep it, do not send it, and stop counting it against the budget" - three things `exclude`
  does as well, so the clause could only mean something by implying that an excluded item is still
  charged for. It is not: measured against a real endpoint the two produce the same request to the
  token, 3,451 active and 2,219 either way, with the same figure held back. What separates them is
  what the person reading the pane is meant to conclude - one is set aside, the other is done with
  - and that is what the line says now.

- `Trim` pays for the marker it leaves behind. Eliding a tool result does not recover what the
  result was costing: the projector puts `[... <the pass's reason> ...]` where the content was, and
  the kernel makes that reason the note on every item in the pass, so each elision buys back the
  content *less* one copy of the same sentence - about 21 tokens of it. The pass subtracted the
  whole item and stopped as soon as its own arithmetic said it had reached the target, which on a
  context of small results is a target it never reached at all. Twenty `write` confirmations of
  seven tokens each, elided for twenty-one tokens apiece: the plan reported 140 tokens recovered
  and took the request from 852 to 1,190 - through the 1,000-token limit the pass exists to keep it
  under, having spent one of the person's undos to get there.

  It now credits itself with the net of each elision and skips any result no bigger than the marker
  that would replace it, which is a floor rather than a refusal to work - one result worth eliding
  among twenty that are not is still elided, and the twenty are left alone. A pass with nothing
  worth doing returns `None`, which is the same answer the 0.5.0 fix arrived at from the other
  direction. The marker's cost is estimated from the reason's own length at four bytes a token, and
  is allowed to be an estimate: a counter that has learnt a different ratio moves the boundary by
  one small result, where crediting the whole item moved it by everything.

- A stream that stops arriving keeps what arrived. `parse_stream` returned the transport's error,
  which failed the turn and dropped every token the model had produced - and been billed for. One
  session died that way 148 seconds into its 22nd request, `error decoding response body`, having
  just asked a copy of itself the question the whole session was built around; nothing of the
  answer survived. Eleven lines above the line that did it, the interrupt path already had the
  right answer written down: *"whatever has been parsed is kept and the rest of the socket is
  abandoned"*. A dropped connection is that case without the consent, so it now gets the same
  handling under a name of its own - the turn ends at `cut off`, the status line says the model was
  cut off mid-answer and that what arrived was kept, and the calls that arrived whole are kept too,
  because the permission policy is still what decides whether they run and a provider quietly
  dropping them would be deciding that instead.

  Retrying was the other candidate and is worse: every attempt is billed, so an answer that
  reliably outruns an upstream's patience is paid for four times and fails anyway - and the loop
  that waits out a busy server retries only where nothing was generated. A complete answer whose
  trailing bytes were lost is not reported as cut off, and a stream that carried nothing at all
  still fails, because there is nothing to keep and the transport's own account is the best there
  is.

  Both dialects, and the case is in the shared conformance suite rather than in either of them, so
  the third provider was fixed by the same commit and a fourth cannot get this wrong quietly. That
  suite exists because this workspace has three providers and had been fixing one bug in one copy
  at a time; this is the fourth instance, and the first where the *note* had been fixed in one copy
  while the behaviour was fixed in none - `nachalnik-utils` carried a paragraph claiming it retried
  a body that stopped arriving, naming this exact error. It never did.

- A row the projector repaired away says what it is holding. Putting the whole of a truncated tool
  result back beside the copy the model was shown - which is the intended way to send the whole -
  made the row *below* it drop to `0`. The pair answer one call, so the whole takes the call and
  the short copy is dropped: correct, and the request was right the whole time. What was wrong is
  that three of the four places reporting on it disagreed. The row claimed to be sending its
  content, showed `0` for what that cost, and accounted for none of the 8,583 tokens it was
  holding; `/budget` and the status line said nothing was held back at all; and `f`, whose whole
  job is to hide rows that are holding something back, kept it.

  One conflation, for the third time: whether an item is going cannot be read off its *state*. An
  item a projector repairs away to keep a request valid is `Active`, holding everything it holds,
  and not in the request - a fourth way of not being sent, after excluded, archived and elided,
  and the only one `ContextState` cannot express. `App::costs` became `App::going`, which carries
  what each item costs *and* why each item that is not in the request was left out, both read off
  one projection; `Going::sends_content` is the question every column, reason, filter and figure
  now asks, so they cannot drift apart again. The reason on the row is `Projection::skipped`'s own
  words rather than a second copy assembled out here from the state and the note - which is what
  it was, and which had no answer at all for this case.

  One projection per frame, computed once in `draw` and handed to the three places that report on
  it, rather than each asking for its own.

- One word per mechanism, in `amend` and at the prompt. `amend` had a `prune` action with a
  `state` argument, which put the word for *one* move over five of them - `pin` and `restore`
  included, so "prune to pin it" was the documented way to protect something - and an item you
  pruned then read back as `archived` on every screen that lists it. Two live models in a row
  spent a call each asking for `restore` as an action, were told it was a state and not an action,
  and gave up. They were right and the levels were wrong: the five moves are actions now, each
  named for the state it leaves behind, and the `state` argument is gone. `/prune` becomes
  `/exclude` for the same reason - it only ever moved an item to `excluded` - and `/keep` becomes
  `/pin`.

  Nothing that used to work stopped working. `prune` with a `state`, `/prune`, `/keep`, `unelide`,
  `unpin` and `include` are all still accepted and none of them is documented, because taking a
  word somebody reached for costs nothing and refusing it costs them a turn. AGENTS.md carries the
  convention now, including that distinction: a synonym in an enum, a help line or a message is
  the bug, a synonym in a `match` is a kindness.

- The question about an `amend` says which items it would change. `ids: [22]` is a true account of
  the arguments and a useless one to be asked about: the tool rewrites and hides pieces of the
  context, the box asking covers the list those numbers refer to, and the answer is one key - so
  somebody asked whether item 22 may be elided had to already know what item 22 was, from a screen
  they could no longer see. The question now names each item the way the context tab does, and
  expands a `select` into what it matches, which is the argument least answerable without it. Only
  for the two tools this program installs itself, because `ids` on somebody else's tool is
  somebody else's vocabulary and a confident description of the wrong thing is worse than none.

- A leading `~` is refused in words instead of quietly becoming a directory called `~`. The three
  file tools run in process with no shell in front of them, so nothing has ever expanded it -
  `read` on `~/.gitconfig` joined it onto the working directory and came back
  `/w/~/.gitconfig: No such file or directory`. That is the `access(2)` trap in a second form: an
  error indistinguishable from the file being absent, which a model believes, so it concludes the
  home directory is empty rather than that its path was taken at its word.

  Not expanding it is the right default and it is kept - expanding here would have `--no-sandbox`
  hand over `$HOME/.ssh/id_rsa` for real, on a path a model wrote - so the refusal is a sentence
  naming what happened, where to write a path instead, and `./~` for a file really called that.
  It comes *before* the unconfined early return, because that is the case it matters most in. Only
  a leading `~` is refused: `notes.txt~` is a real file and `./~` is how a shell asks for a literal
  one.

  Said twice, the way the confinement is. `Reach::allows` is what lands, but it lands as a surprise
  unless the argument said so first, so the three file tools share one `PATH_ARG` describing the
  rule once. It costs 87 tokens across the tool definitions - 406 to 493 - which is a constant paid
  per request against a model that otherwise spends whole calls hunting for a home directory it
  cannot reach, and one such call is worth more than that.

- `amend` accounts for a change with the reason for *that* change. One sentence served all four of
  the things that can move the figure, and it named eliding as the cause - so a model that wrote a
  note, which makes the request bigger because that is what a note is for, was told its ten extra
  tokens were the marker of an elision it had not performed. The whole reason that sentence exists
  is that models read the two figures and did not work out which way they had gone; a wrong account
  of a number is worse than the bare number. Eliding still explains its marker, content coming back
  says it is content, and a note says nothing beyond the figures, which are the answer rather than
  a surprise in it.

- The `~` refusal closes the retry and names no other path. Two changes, both from watching models
  read the sentence added earlier in this release. It now says the same path will be refused again
  as it stands, because one that did not say so was sent back unchanged six times in a single turn
  by the same model - a refusal that does not close the retry is an invitation to retry, and after
  the change that model asked once and got it right. And the literal-`~` spelling has moved out of
  it into `PATH_ARG`: two models answered a refusal about `~/notes.txt` by reading `./~`, because
  every concrete path in a refusal is read as a path to try. A refusal is read under pressure to
  try something else; a schema is read while choosing, which is when a rare spelling is worth
  knowing and nobody is about to act on it.

- A move given a `label` instead of `ids` is told how to say what it meant. `label` is in the same
  schema - it names a `note` - and a model reaching for a way to say *which item* took it, which is
  a fair reading and a wasted call. The refusal now hands back the spelling: `select:
  "label:secrets.txt"`, which is a selector that works and was there all along.

- The status line's give-way ladder has the rung it was missing. The address gives way before the
  figures, in two steps, and there it stopped - so a long *name* pushed `F1 for the keys` off the
  right edge with the address already gone and nothing left to give.
  `dots-studio/dots-3-note-preview:free` is 36 columns and perfectly ordinary on OpenRouter, which
  is this program's default endpoint. The vendor prefix goes next, then the name is cut from the
  left, which is the opposite end from a host and for the same reason: the distinguishing part of
  `openrouter.ai` is at the front and the distinguishing part of a model id is at the back.

- A provider's notice reaches whoever is holding the `App`, not only this program's own loop.
  `take_notice` was drained on a tick in `main`, so "the model was cut off mid-answer; what had
  arrived is kept" - written for exactly the moment a person needs to know something is missing -
  went nowhere for any other caller, and could land after the turn it describes. `on_outcome`
  drains it first now, so it sits with that turn; the tick keeps draining it for the notices that
  belong to no turn.

- A compaction pass says what it moved. The line in the chat and the row in the trace both counted
  `report.removed` and nothing else - but removing and eliding are two mechanisms, and the
  compactor that ships here only ever elides, on purpose, so that the call each result answers
  keeps its answer. Every pass it has ever made therefore announced itself as `compacted: 0 items
  out` in the only account a person gets of a context changing under them. Third instance of one
  conflation, after `Trim`'s candidates and `/budget`'s held-back line.

- The blank lines a provider puts in front of a message are no longer read as content. Some
  providers send them - `inception/mercury` opens every message with two, and the recorded `gemini`
  sessions have none - so it is a habit of the provider rather than anything the runtime did. The
  item goes on keeping exactly what arrived, because a record of "what arrived, tidied up" cannot
  answer what arrived; the screen stops spending rows on it.

  Cosmetic in the conversation and not cosmetic in a tool result. The preview is the first six
  *lines*, so two blank ones in front cost a third of it and truncate it two lines early: on one
  real transcript four of the six rows were empty. The padding was eating the evidence.

  Leading blank *lines* rather than leading whitespace, which would take the indentation off the
  first line of a message that opens with a code block. The answer itself needed no fixing and gets
  none - it is rendered as markdown and the renderer already swallows them. The thinking, a tool's
  output and an item's pages are shown as the text they are, and those are where this was read.

## [0.5.0] - 2026-09-06

### fixed

- The budget no longer charges for thinking this dialect cannot send. `LinearProjector`'s
  `send_reasoning` is on by default - correctly, since a provider that does not want an assistant
  turn's reasoning ignores the field - but `to_wire` here has never put it on the wire at all, and
  cannot: most endpoints speaking the OpenAI-compatible dialect reject a message carrying a field
  they do not know, and there is no agreed name for that one. The budget is counted over the
  messages the projector produced, so every turn of reasoning in the context was in the estimate
  and in none of the requests. Measured on one ordinary turn with 3.4KB of thinking, the estimate
  charged 928 tokens where the request carried 73.

  `Calibrating` cannot take this out, which is why it went unnoticed for so long and why it looks
  worst where it is read most. The wedge grows with the number of reasoning turns the context is
  holding, while the correction is a single multiplier learned cumulatively over the whole
  session - so the scale is a blend dominated by the earlier, thinner requests, and a long session
  drifts steadily high while a short one looks fine. One reported at `~93,663 tokens` against
  `82,381 really` was out by roughly thirteen turns' worth.

  The projection is now the provider's own answer - `Endpoint::projection`, beside the `to_wire`
  that has to honour it - rather than a second decision made from the same flag in `main.rs`,
  which is how the two came apart. Gemini's answer is unchanged in substance and now says so in
  its own file: it sends the ordering *and* the thinking, as a part marked `thought`. Nothing
  changes on the wire for either. The turn keeps its reasoning in the record, on the context tab
  and prunable, exactly as before.

- `/load` puts every token figure on one scale. A snapshot carries what its counter had learnt,
  and reading one in moves the correction under everything already counted - but the load counted
  the items it brought *first* and applied the correction afterwards, which is the reverse of the
  ordering `Kernel::resume` documents and warns about. So every loaded item carried a figure from
  whatever scale this session happened to be on, while the budget beside it is projected live and
  was already on the loaded one: a context that really came to 3,998 tokens read 2,002, and the
  `held` column disagreed with the `sending` column on the same row by exactly the correction. It
  goes through `Kernel::recalibrate` now, before it counts: the front door applies the correction
  and recounts, which is what brings the items the load sets aside - and which `held back` adds to
  the loaded ones - onto the same scale, loudly, as `context.recounted`.

- `/budget`'s two halves answer the same question. The tokens it reports as held back come from
  `tokens_withheld`, which counts an elided item - it is in the request as a marker and is not
  sending what it holds - and the count beside them came from what is not *projected*, which does
  not. So the one command whose whole job is to say what the next request costs and what it does
  not read `held back: 9,004 tokens in 0 items the projector is not sending`: a count of nothing
  against the figure it was supposed to account for, and a clause that is untrue of an elided item
  besides. It is the same conflation `Trim` was making, in the place it is most read.

- `Trim` stops asking once there is nothing left to elide. Its candidates were the projected tool
  results, and an elided item *is* projected - as a marker - so every item a pass had already
  elided came back as a candidate on the next one. The plan was therefore never empty, never
  `None`, and its summary went into the context before every single request from then on: a
  compactor adding a line and burning one of the person's sixteen undos per request, growing the
  thing it exists to shrink, for as long as the session lasted. A pinned `--file` bigger than the
  target is enough to get there, and six passes over one took the context from three items to
  eight. It looks at what an item is *sending* now.

- A loaded session hands over the tool call identifiers it already used. `/load` pushes a
  snapshot's turns into the running kernel and dropped `used_calls` on the floor, so the kernel
  had never heard of the identifiers those turns carry - and a provider that numbers its calls
  from zero every turn, which is the reason the repair exists at all, would hand one straight
  back. Nothing would have repaired it and the next request would have answered one
  `tool_call_id` twice. `kamchatka` could not do anything about this from out here, so the
  runtime grew `Kernel::reserve_calls` for it.

- The `shell` tool's temporary directory is never made *through* whatever is already at its name.
  It has to be predictable - it is named after the confined process so that the one which spawned
  it can remove it afterwards - and `create_dir_all` was satisfied by anything it found there,
  including a symlink. The ruleset grants that directory everything a writable root gets and hands
  it over as `TMPDIR`, so a link left in `/tmp` by another account would have opened up whatever
  it pointed at. It is created exclusively now, at `0700`; something of this program's own left by
  a run whose process identifier has come round again is removed and remade, and something that is
  not cannot be unlinked, which leaves the command with no temporary directory rather than with
  somebody else's.

- `introspect` declares the `whole` argument it reads. The tool read it, its description told the
  model to use it, and both `look`'s last line and the marker in a sampled item ended by telling
  the model to ask for the `whole` of an item - while the schema declared four properties, none of
  them that one. A model following the schema could not pass it, and an endpoint validating
  against the schema would have refused the call.

- The policy's two per-call notes drop the oldest rather than all of them. Both the calls a person
  granted the network to and the reasons refusals were refused were emptied outright once they got
  past thirty-two, which throws away exactly the entry most likely to be wanted: each is written
  down when the policy answers and read when the call runs, so the live one is among the newest.

- `amend`'s `note` does not spend two of the person's undos to write one thing down. Pinning it
  was a second state change after the push, which is the arithmetic `revise` already keeps its own
  account out of the note to avoid; the item is pinned as it is written, and `because` was already
  carrying the reason.

- A long answer keeps its beginning. The transcript bounded a *still arriving* entry at eight
  thousand bytes and replaced whatever came before it with `[...]` - which is right for a `find /`
  and wrong for a message. A model writing a long answer had its first paragraphs eaten while it
  was still writing the last one, and nothing ever put them back: the finished item is read off the
  kernel only for a provider that did not stream, so what was lost stayed lost for the rest of the
  session. The bound is now on a tool's output and on nothing else. Nothing anybody said is
  shortened on the way to the screen, however long it is.

- Git is no longer killed outright by a configuration it cannot read. Under Landlock, `access(2)`
  still answers from the file's own permissions, so git asked whether `~/.gitconfig` was readable,
  was told yes, opened it, got `EACCES`, and took the *unreadable configuration* branch rather
  than the *no configuration* branch: `fatal: unknown error occurred while reading the
  configuration files`, exit 128, and every git command in a confined session dead - `git log`,
  `git diff`, `git status`, all of them. A missing file is fine and an unreadable one is not, and
  a command has no way to tell git which it has. A confined command whose global configuration is
  out of reach is now handed `GIT_CONFIG_GLOBAL` pointing at nothing, which is the case git
  handles. One that is in reach is left alone, so an identity and aliases that could be read still
  are, and a `GIT_CONFIG_GLOBAL` somebody set is never overwritten.

- A multi-byte character split across two reads of a stream is no longer destroyed. Both providers
  assembled the response by decoding each chunk off the socket as it arrived, lossily, so a
  character whose bytes straddled a chunk boundary was decoded twice - once with its tail missing
  and once with its head - and became two replacement characters. `zażółć` came back `za??ółć`,
  and it then went into the context, the transcript and the session log with nothing to say it had
  ever been anything else. It depends only on where the network happened to break the stream, so
  every language with diacritics and every typographic dash was a coin toss. The buffer holds bytes
  now and only whole lines are decoded.
- A confined command cannot truncate a file outside the working directory. The Landlock ruleset was
  built on ABI 1, and an access right a ruleset does not *handle* is not restricted at all -
  truncation has had a right of its own since ABI 3, because `truncate(2)` takes a path and never
  opens the file, so `WriteFile` does not cover it. `os.truncate('/home/you/.bashrc', 0)` came back
  with nothing to say and a file of nought bytes. GNU `truncate(1)` opens the file and so was
  refused all along, which is why nothing noticed. The ruleset asks for ABI 3, which brings `Refer`
  with it and so also allows a `mv` between two directories of the working directory.
- A credential rule is about the file that gets opened. The rules were matched against the raw
  argument by splitting it on `/` and taking the last piece, while the file was opened at a
  *resolved* path - and the two disagreed about the simplest thing there is. `read` with a `path`
  of `.env/` matched no rule and opened `.env`, so in a session where somebody had answered
  `always` to an ordinary read, the whole list of credential patterns came off with a trailing
  slash. The path is read as a `Path` now, so a trailing slash, a doubled separator and a `.` in
  the middle all name the file they name.
- The pattern matcher backtracks. It walked a pattern's literals with `find` and took the first
  hit, so `a*bc` refused `abcbc`: the `bc` it found was the one the star should have swallowed and
  there was no way back. A permission rule that silently fails to match is the worst way for one to
  be wrong, and `*credentials*.json` is not an exotic thing to write.
- The session written on the way out goes into a `0700` directory. It holds a whole conversation
  and every byte of output every tool produced, written without anybody asking for it, and under an
  ordinary umask that was a world-readable file in a directory everyone on the machine could list.

### added

- <kbd>ctrl+home</kbd> and <kbd>ctrl+end</kbd> go to the beginning of the conversation and to the
  end of it, the second one following the newest again from there. Control is held because
  <kbd>home</kbd> and <kbd>end</kbd> belong to the prompt, which is under every tab - a line editor
  whose <kbd>home</kbd> moved something else would be a trap.

- `--sandbox-read PATH` opens a path outside the working directory for reading and no more, next
  to `--sandbox-allow`, which opens one for reading and writing. What sends most people here is a
  toolchain: `$HOME` is not a system directory, `cargo` is a rustup shim that reads
  `~/.rustup/settings.toml` before it does anything at all, and a confined `cargo build` therefore
  failed with `could not read settings file: Permission denied` - which looks exactly like a
  missing compiler. A live model spent six calls hunting for one that was installed the whole
  time. `--sandbox-allow ~/.rustup` would have fixed it and handed the model the ability to
  replace the toolchain it was about to run; this is the flag that was missing. The three file
  tools honour it too: `read` reaches a read-only path and `write` and `edit` are refused with a
  message that says which of the two it is.
- A permission error from a confined command says when the confinement caused it. Landlock refuses
  an `open` with `EACCES`, which is the same thing the kernel says about a file that is somebody
  else's, so `Permission denied (os error 13)` gave a model no way at all to tell a boundary from
  a protected file - and the tool description saying so in general did not stop one spending six
  calls on it. A refused command that names a path outside its reach now gets a line naming that
  path and what the command can reach instead, directly under the status line, where an output
  limit cutting from the end cannot take it. A refusal that names only paths the command *can*
  reach gets nothing: `cat /etc/shadow` is refused with or without a sandbox, and hedging about it
  would send a model looking for a boundary that had nothing to do with it.

### changed

- `sandbox::confine` takes an `Option<&Path>` for the scratch directory, and `sandbox::make_scratch`
  is the thing that makes one. A path that cannot be opened makes the ruleset fail to build, which
  comes back `Unavailable` - a command running *unconfined* because its temporary directory was not
  there - so the one case where there is no scratch has to be sayable rather than inferred.
- `network: deny` is described as what Landlock actually refuses, which is TCP. `ConnectTcp` and
  `BindTcp` are its only two network access rights, so a confined command can still send a UDP
  datagram - enough to put bytes in a DNS query - and AF_UNIX needs a kernel from 2026 to reach at
  all. The screen, the tool's own description to the model and both readmes now say TCP rather than
  "the network". Nothing about the confinement changed; what it was described as did.
- `--requests 0` says in `--help` that it means no limit at all, which it has always done.

## [0.4.0] - 2026-09-05

### fixed

- A request that stalls is waited out, the way a busy server already was. A 429 or a 5xx got four
  tries and a doubling; a connection that timed out got `?` and took the session with it - which
  is the same event wearing different clothes. Eleven of fourteen runs against one upstream died
  this way while the same model answered a single request in six seconds; run one at a time they
  all passed, so what they had met was load, not a wall. A refused connection is deliberately not
  retried: an address with nothing behind it is an answer, and making a typo take four doublings
  to report helps nobody.

- A request that has not been answered yet is watched the way a stream already was: `esc` stops
  it, the silence is reported at ten seconds and then at intervals, and it gives up after 150.
  Only the *stream* was watched before, so all of that began at the first byte - and a server
  that accepted the connection and then went away never sent one. The status line read `asking`
  and nothing else could be done; the longest measured case held the terminal for eighteen
  minutes, waiting for the operating system to notice. Giving up now says which of the two
  happened, because a model that hung up mid-answer and one that never spoke are not the same
  problem.
  Google's dialect gets all of this too: its `send` was a bare `?`, so a stall there got neither
  the doubling nor the noticing, and ended the turn whenever the operating system got round to
  it.

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
