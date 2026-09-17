# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- `/compact`, which asks the compactor by hand and shows its answer before applying it: every item
  it would take, with the identifier, what it is and what it is holding, and then `y` or `n`. The
  question stands in the prompt's place like a tool's and is pinned rather than modal, so the
  context tab is a keystroke away while it waits and `p` there keeps a row out of the pass. Saying
  yes works the pass out again rather than applying the list that was shown, so a pin made while
  reading it is honoured rather than refused after the fact.

  It is also the only way out of a context too big to send. `context` and `amend` are the model's
  tools and reaching them costs a request - the request that is failing - so until now a session
  that had run out of room could only be pruned by hand, item by item.

  `--headless` prints the list and takes it, there being no key to press down a pipe. The opposite
  of what `--on-ask` does with a tool's question, because they are different questions: a tool's is
  the model asking to do something nobody vouched for, and this one is a line the operator typed.
- The context tab says how far over the limit the next request is, when it is over. The corner
  turns red and says the compactor runs first, and has no room for the figure that decides what to
  do next; the difference between "over" and "over by two thousand" is the difference between
  reading forty rows and taking one of them out.
- `grep` answers with the files its matches were in when the lines will not fit, rather than with
  the first few thousand bytes of them. A capped lines answer is filled from wherever the walk
  started and says so, which is why it already advised `files_only`; this takes that advice rather
  than printing it. The saving is the shape that prompted it: four broad searches in one turn, each
  capped at a hundred matches and each still filling its byte limit with context lines, put forty
  thousand tokens into a context in one step.
- A request the model refused for its length says what that length was and how much of it has to
  go. The sentence above it is the server's, and every vendor writes those two numbers in a
  different order; this says what they mean here. It matters because the only other figure in
  front of somebody at that point is the corner, which is the estimate that has just turned out to
  be wrong - and which the same refusal is correcting.

  A request refused *here* gets its own sentence, because it is a different fact. The endpoint's
  is the model's own tokenizer reporting on a request it read; this one is an estimate of a
  request nobody has seen, made by the counter whose being wrong is the reason any of this exists,
  and it says so. "The model read that request as" a number the model was never shown is the kind
  of confident wrong sentence this corner of the program exists to stop.
- `--send-oversized`, and `send-oversized` in a settings file: send a request that looks too long
  for the model anyway, and let the endpoint be the one that says no. Both halves of that check
  can be wrong - the figure is an estimate and the limit is whatever the endpoint advertised.
  `liquid/lfm-2.5-2.6b` is quoted at 65,536 tokens on OpenRouter and routes to a provider whose
  own window is twice that, so a session holding itself to the smaller number refuses requests
  that would have been answered. The flag costs a round trip and buys the endpoint's own count of
  the request, which is worth more than any guess made here.
- `X-OpenRouter-Categories: cli-agent,programming-app` beside the referer and title, which is what
  puts an app in the [marketplace](https://openrouter.ai/apps) rather than only in the rankings.
  Sent only to OpenRouter; `KAMCHATKA_NO_ATTRIBUTION` turns all attribution off. An unrecognised
  category is dropped silently by OpenRouter, so nothing here checks the spelling.

### fixed

- A call asked permission for the one thing it does again, rather than for everything its tool can
  do. `context` reading its own items declared all thirteen of its subjects, so `look` at three
  items asked to be allowed `revise` and `elide` too, and `--allow context:look` on its own could
  not look. The same reading fault had the policy miss the two arguments it consults: a `curl` was
  no longer judged against `net:reach`, and a path rule no longer matched the path a call named.
  Both failed towards allowing more, and neither is visible from anywhere but a live session.

- The permissions tab says what a rule covers. `--allow fs` writes a rule about a whole domain, and
  its row read "nothing registered needs it" while the five `fs:*` rows above it each named `fs`; a
  server rule read the same, with every tool it covers sitting above it. A row was only filled
  where a tool declared that exact capability, and no tool declares a bare domain or a server name.
- A networked command allowed in a headless run is granted the network. Everything answering a
  permission question means beyond the decision itself lived in the key handler, and that loop has
  no keys - so `--on-ask allow` let a `curl` through and then ran it with TCP cut. Both drivers
  answer through one place now.
- Nothing a tool returns is uncapped. Every tool this program ships has a row in the limits table
  and reads it; a tool from an MCP server has neither, and fell through to a kernel default of *do
  not truncate* - so a session started with `--mcp` had no ceiling at all on what somebody else's
  server could put in its context. It is cut at the same 32,000 bytes as everything else, though
  not yet by a row `/limit` can change.

- An argument the action a call named does not read is refused, rather than ignored. `log` has
  held its arguments to this since it was written; `fs` and `context` now do too, per *operation*
  rather than per tool, because the mistake actually made is an argument that belongs to a sibling:
  `old` on a `read` is `edit`'s, and `ids` on a `note` reads as the item to annotate when `note`
  writes a new one. An ignored argument comes back as a real answer — the answer to the call
  without it — so a read meant to be narrowed arrives as the whole file with nothing saying so.
  The refusal names the operation the argument belongs to when exactly one does; `ids` is eleven of
  `context`'s thirteen, and naming the first would report the order of a table as a fact about the
  argument.
- `glob` and `grep` obey a `.gitignore` outside a git repository, which is what `fs` has been
  telling models they do. The walker honours one only inside a repository by default, so outside
  one a session was handed build output while its tool definition said it had been spared it. The
  test that covered this was inside this repository, where the walker finds a `.git` two
  directories up; the second one is under `/tmp` and asserts that nothing above it is a repo.
- `/limit` is keyed by subject and now says so wherever it is spelled out: the error for a subject
  with no number asked for a `<tool>`, and `/help` offered `/limit ID BYTES` under a line about
  each tool's output. A limit stopped being a tool's the day one tool did five things.
- The line after `/model` or `/provider` waits for the switch it asked for. Both hand the round
  trips to a task, because a screen should not stop while a new endpoint is asked what it holds -
  and nothing was waiting for that task, so `/provider URL ID` followed by `/model` answered with
  the *old* model, and a message on the next line could be asked of whichever of the two won the
  race. At a keyboard it usually resolved in the gap before somebody typed; down a pipe there is
  no gap, so a script got the losing side as a matter of course. The wait is in `App::submit`, the
  one door every typed line goes through, so a frame drawn mid-switch is still a frame.
- A capability is the whole of its tool, and a rule finer than one is about the part it names.
  `--allow amend` allows amend; `--allow amend:note` allows a note and says nothing about the
  rest; `--allow amend --deny amend:revise` is everything but that one. Both halves were wrong
  before. Four of `amend`'s actions shipped pre-seeded as questions, so allowing the tool left an
  `exclude` still asking - `--allow amend` meant something other than `amend`, with nothing in
  the words to say which four were the exceptions. And naming one action put the rule beside the
  tool's capability rather than in front of it, so `--allow amend:note` was read against an
  `amend` nobody had answered about and granted nothing at all, which is the worst way for a
  permission rule to be wrong: it reads as given. The one thing a finer rule still cannot do is
  overrule a refusal, because the strictest of everything consulted wins and `--deny` is the last
  word.
- `--compact` below a third asked for a compaction and got nothing. The compactor took its target
  twenty points under its threshold with a flat floor of ten percent, so `--compact 0.15` started
  at fifteen percent of the limit and aimed at ten - and a context between the two was already
  under the target. The pass fired before every request, found nothing worth taking, said so, and
  left the context exactly where it was, for as long as it stayed in that band. The floor is now a
  fraction of the threshold rather than a constant, which cannot rise above it; `Trim::under` is
  the pair and the reason it has to be ordered, in one place instead of at the call site.
- Everything that counts items for a person counts one of them as `1 item`, and every token
  figure beside one is written with its thousands separator: the context tab's own line, a
  resumed or loaded session, and an undo or a redo. `1 items` is the corner of a screen quietly
  saying it is not looking, and somebody who notices has no way to tell whether the figure beside
  it is approximate too - `6 items, ~16342 tokens` was both at once.
- `/limit` marked every `fs` row `not offered` in a session that was offering `fs`, and told
  anybody setting one that the tool had no next call until something added it. The rows are keyed
  `fs:read` and `fs:grep` because what a whole file costs and what a repo-wide search costs are not
  one number; the check against the registry was comparing that key with a tool id, and five of the
  ten rows have not been tool ids since `fs` became one tool.
- `/load` takes every spelling `/save` does. A session is two files, so `/save notes.jsonl` writes
  `notes.json` beside the log it was named after - and `/load notes.jsonl` took its argument at its
  word and went looking for `notes.jsonl.json`, which nothing had ever written. Both go through one
  function now, and it reads the suffix without regard to case the way `attach` reads an extension,
  so `notes.JSON` names the session on a filesystem that does not care how a name is spelled. The
  stem is left exactly as typed, because that half really does name a different file where it does.

### changed

- **An operation declares its own arguments.** Each of these tools described itself to the model as
  one property bag holding every argument any of its operations takes, with the applicability
  written into the prose: nineteen descriptions opened `for `grep`:` or `required by the nine that
  change:`. `required` was `["action"]` on five of the six, so a `read` passing `old` was a
  well-formed call right up until it ran. Now there is a branch per shape, each carrying its own
  arguments and its own `required`, and twenty-three arguments are named as required where one was.
  The applicability moves out of the prose and into the schema: inside a branch there is nobody
  else for an argument to be confused with, so each one says what it is and stops.

  The arguments go inside a `call` object, because the root of a schema may not itself be a union.
  Passing them flat, the way the old schema asked for them, is still understood; passing them in
  both places at once is refused, because reading either would drop half of what was asked.

  Operations that read the same arguments are one branch under an `action` of several words, so
  `context`'s five moves are declared once rather than five times, and a tool whose operations all
  read the same arguments gets no union at all.

  It costs about eight hundred tokens a request - the tools section goes from ~2,690 to ~3,500 -
  and that is the price of saying in the schema what nine of `context`'s thirteen operations
  previously had to be told at run time, one wasted turn at a time.
- **`context` is one tool over one object**: four operations read the context and nine change it.
  It was `context` and `amend`, on the argument that a `ToolSpec` declares its capabilities once,
  so one tool would have meant that answering *always* to "may it look at its own items?" also
  answered "may it rewrite a tool result?". That hazard is real and it stopped being a reason for
  two tools the day a subject became `<domain>:<operation>` and `Tool::needs` let a call say which
  one it is: `context:look` and `context:revise` are separate rows on the permissions tab
  whichever tool they arrive under, and `--allow context` is how you answer for the lot. What two
  tools cost was the part nothing was measuring - two descriptions in every request, most of each
  spent saying which of the two the other one was.
- **`fork` is its own tool**, with `draft` and `ask`. It was two operations of `context` and it is
  neither a reading nor a change: it stands up a copy of the session and pays a provider for an
  answer. Letting something read its own items should not be letting it buy another request, which
  the subjects had said for a while - `fork:draft` and `fork:ask` were in a domain of their own
  before the tool was.
- **Every tool takes an `action`**, `shell` and `log` included, and each has exactly one:
  `shell` does `run` and `log` does `read`. A word costs nothing beside the rule it completes, and
  the tool that is the exception is the one a model gets wrong - a live session called
  `log {action: "look"}`, got the summary back and cited it as the answer to a question it had not
  asked. That call is now refused by name, and the paragraph of advice written to catch it is gone.
- **`context` takes thirteen words and no other spellings.** `prune` with a `state` argument, and
  `unpin`, `unelide`, `unexclude`, `elided`, `excluded`, `active` and `include`, are all gone. They
  were there on the reasoning that accepting a word somebody reached for costs nothing, which was
  true of the word and not of the program: the schema advertised thirteen operations, and every
  place that had to answer "which operation is this call" needed a second table of the words that
  are not in it. One list, in the schema, is the whole of the vocabulary.
- **An output limit is keyed by subject**, which is the same string the permissions table is keyed
  on: `/limit fs:grep 8000` and `--allow fs:grep` name the same thing, and `Tool::limit` is one
  line in every tool - the limit for a call is the limit for the subject that call needs. It was
  keyed by tool id, which stopped meaning anything the day one tool did five things of five
  different sizes, and `/limit` was quietly marking every `fs` row `not offered` in a session that
  was offering `fs`. `setup tools` reports them as the figure they share and then whichever ones do
  not, rather than as a column that would have had one number standing for `fs:read` and `fs:grep`
  alike.
- **The tool definitions are 5.5% smaller**, and two of them were wrong. `fs`'s `glob` argument -
  which is what `grep` filters files by - carried the glob *grammar* as its description and never
  said what it was for, so the schema explained the same syntax twice and neither copy said which
  of the two arguments filtered anything. `context` said `archive` puts an item away "for good"
  three sentences before saying every item can be restored, and said all nine of its changing
  operations are named for the state they leave, which is true of five of them. The rest of the
  saving is duplication: sentences in a description that the argument beside it already said, and
  claims a tool was making about how well it reports itself. Every argument now says which
  operation it belongs to the way `fs`'s do - `for `note`: a short name` rather than `note: a
  short name`, which read as a remark.
- **Every tool is offered by default**, the four that read and manage the session among them.
  `--introspect` is gone: it was a flag for those four, answerable only before the session started,
  and what it was really being used for was a fact about a project rather than about an
  invocation. What that costs is the six schemas in every request - 2,738 tokens where `fs` and
  `shell` alone are 810 - and the two settings below are how a session that does not want to pay it
  says so.
- `/tools toggle ID` replaces `/tools drop ID` and `/introspect`, and works on every tool there is,
  including the ones an MCP server brought. It goes both ways, which is the half that was missing:
  a dropped tool used to be dropped, and `/introspect` could only put back the one group of four -
  by building new ones, which threw away whatever `amend` was remembering. A tool turned off now is
  kept rather than rebuilt, so the one that comes back is the one that went away, still holding
  what it pinned and what it could still walk back. `/tools` marks the ones that are off, since
  their names are exactly what somebody needs to get them back.
- `tools` in a settings file says which of them a session starts with, by id; left out, all of
  them. `["fs", "shell", "context", "log", "setup"]` is the whole set minus `fork`, which is how a
  project says *do not go buying extra requests*. An empty list offers none of
  them. A name that is not a tool stops the program and says which there are, for the reason an
  unknown key does. The ones left out are still built and can be offered with `/tools toggle` - and a
  `shell` offered that way is confined exactly as one offered at startup would have been.
- `wiring::Setup`'s `builtin_tools` and `introspect` are one field, `tools: Option<Vec<String>>`,
  with the same meanings: `None` is every tool, `Some(vec![])` builds none at all.
- `budget` names the three held-back states that are the model's (`excluded`, `archived`, `elided`)
  instead of counting them off as "the first three". Models read the ordinal as item ids.
- `amend` says in words which way the request figure went - smaller, larger or unchanged - and that
  the `from ~` figure is what the request cost when the change found it, not what an earlier
  `budget` reported. Models compared against a remembered figure and read drops as growth.
- The status line ends at the figures; `F1 for the keys` is off it. `esc stops it` stays. The live
  suite no longer tests how the line narrows - `edges.rs` pins that offline.
- A release archive is the binary and `kamchatka.json`, and nothing else. The licence and the four
  documents are out; they are a link away and always current.
- One fewer direct dependency: `async-trait` is the runtime's, and every `#[async_trait]` here is
  already written against `nachalnik`'s re-export. Depending on it twice let the two drift.
- `ratatui` is taken with three of its five default features. `macros` is out because `ui` builds
  its layout from calls rather than from a declaration, and `all-widgets` because its only member
  is the calendar.

## [0.11.0] - 2026-09-15

### added

- `grep` takes `files_only`: every file that matched and how many matches it has, most first,
  instead of the lines. Against this repository a broad search costs 3,223 tokens of lines and never
  reaches the file in question; `files_only` costs 894 and sees all 205 files. A capped answer names
  it first among the things to try.
- `grep` and `glob`, declaring `read` rather than `shell` - so a read-only session can search. Built
  on ripgrep's engine (`grep-searcher`, `grep-regex`, `ignore`, `globset`) linked in, so no `rg` on
  the machine and no second process for the sandbox.

  The cut is at matches, not bytes: a hundred matches, lines cut at two hundred characters, and a
  first line saying it stopped and what to do. The answer reports files searched and a `skipped:`
  line naming each reason, so "it is not there" can be told from "nothing was opened". A path rule
  that is not `allow` stops the walk opening that file. `.gitignore` is obeyed and `.git` always
  skipped; hidden files *are* searched. Results are sorted, so two identical searches are one
  context item. A symlink is read where it points inside the working directory.
- `/note TEXT` puts a fact in the context without starting a turn. Goes in as `ContextItem::memory`,
  so the model sees `note:` in front of it and `/exclude memories` names every one. Not pinned.
- A shell result's exit line is coloured: green for success, red for failure, yellow where the
  command never reported - stopped by the person, killed by a signal, or an unreadable status. On
  Windows a killed child arrives as an ordinary exit code, so only this program's own stop is yellow
  everywhere. The reading lives in `tools::shell` as `Exit`, beside the writing of it.
- A resumed session reads back what its items used to say. A `Snapshot` carries items, not events,
  so `-r` came back with every `v1` page empty; `App::recall` now walks the sibling `.jsonl`'s
  `context.replaced` records. Best effort - a missing or truncated log costs a page, not a session.
  It does not carry lineage forward: a resumed session's log starts at `session.resumed`.
- `"border": "#7aa2f7"` in the settings file sets the window frame and everything else that is
  yellow to mean *the keys are here*. Left out or `null`, it stays the terminal's own yellow. It
  does not touch the vocabulary: `ask`, a full budget bar, a killed command and a pinned row stay
  yellow, and red is left alone. A value that is not six hex digits stops the program, headless too.
- A release attaches a static `x86_64-unknown-linux-musl` binary, built from a `kamchatka-v*` tag,
  with a `sha256`. Static musl, so there is no glibc floor; the whole suite including the Landlock
  tests passes against that target. `panic = "abort"` is deliberately not set - a panicking tool is
  reported and the turn carries on.
- A permission question draws an `edit` as a diff: `old` red, `new` green. By argument name rather
  than by tool, so an MCP tool using those names reads the same way.

### changed

- `--no-sandbox` is documented as turning off every tool's confinement, not just the shell's. It
  sets `Reach::confined` as well as dropping the Landlock ruleset, so `read` of `~/.ssh/id_rsa`
  becomes a file rather than a refusal.
- `--compact` says the oldest tool results are *elided*, not dropped.
- The tool descriptions were rendered as they go out (3,356 tokens per request) and corrected. One
  was false: `grep` and `glob` claimed to skip symlinks. One was unusable advice: `read` pointed at
  the shell, which a read-only session refuses; it names `grep` now.
- `grep`'s `context` argument says so when the answer is capped below what was asked for, instead of
  clamping silently.
- `Going` is `#[non_exhaustive]`. It gained `holds`, and a struct of public fields cannot gain one
  without breaking literal construction. `Going::of` is where it comes from.
- `grep` reads a quoted `"3"` or `"true"` as the number or boolean it plainly is, and refuses
  anything else by name rather than falling back to the default. The default for `files_only` is the
  expensive answer, so a swallowed argument cost three thousand tokens.
- <kbd>up</kbd> in an empty prompt puts the last message back - the one waiting for a running turn
  to end, or else a copy of the last line sent. A waiting message is taken, a sent one copied. One
  deep, and only from an empty prompt. <kbd>down</kbd> puts a recalled line away again.
- <kbd>←</kbd> and <kbd>→</kbd> move a cursor within a `/` query, and <kbd>delete</kbd> works. The
  arrows, paging, <kbd>home</kbd> and <kbd>end</kbd> stay with the pane.
- <kbd>ctrl+l</kbd> clears this program's own `·` notes off the chat. The conversation stays,
  because the conversation *is* the context; a line still arriving stays too.
- An edit at the terminal rewrites the item through `Kernel::replace` instead of superseding it, so
  there is one row with the number the item always had, its existing state, and `v1` under
  <kbd>enter</kbd>. The item records `revised: {by: user}` on its `meta`. `Kernel::supersede` keeps
  its place in the runtime.
- <kbd>/</kbd> on the context tab matches the kind column too, via `ContextKind::name`, whether or
  not that column is drawn. The state stays out: `/prune state:excluded` already asks that.

### fixed

- The compaction marker says what to do rather than only what happened: *reading it again would put
  the same tokens back into a context that had no room for them - ask for the part you need
  instead*. Before it, a model read one file ten times; after, once.
- A session no longer writes its record over another's. Two runs starting inside one second share a
  stamp, and a resumed session keeps the name of the session it resumed - so `-r` overwrote the file
  it had just read. The name stays and the file moves to `…Z-2.jsonl`, claimed with `create_new`.
- The `held` column, the corner figure and `/budget` count what the request does not carry per item
  (`item.tokens` less its projected message), rather than the whole of an item that is not in it. An
  assistant turn whose reasoning the endpoint will not take showed blank while holding 25,903
  tokens. It is a figure about the endpoint, not about reasoning, and not a suggestion to prune.
- `context: budget` ranks the expensive items by what they *send*, not what they hold, and the
  column is headed `sending`. A turn holding 25,903 tokens and sending 1,035 stood at the top,
  offering an elision that would free a thousand. `look` gained the second column for the same
  reason, and says that giving a row up frees what it sends and none of what it holds.
- `Going` counts what an item holds and what its message costs at the same moment with the same
  counter. `Calibrating` moves the scale on every response and items keep their figure until a turn
  ends, so every reading taken inside a turn compared two scales - and the `context` tool is only
  ever called inside one.
- <kbd>e</kbd> declines an item a prompt cannot hold - a call-only turn, a picture, or a turn
  recorded in block order - and says why in a panel over the row. It used to open an empty box and
  commit a sentence onto a turn whose call it had not touched.

## [0.10.0] - 2026-09-14

### added

- `amend: note` says when the label is already taken, and names `revise`. A label finds an item
  again rather than standing for one, so five notes under one name left five live assertions that
  compaction could not clear. Unlabelled notes, and archived or excluded ones, do not count.
- Permission rules can name one tool action: `<tool>:<action>`, shipped for `amend:elide`,
  `amend:exclude`, `amend:archive` and `amend:revise`. Each is `ask` until answered, whatever
  `amend` itself says, so `--allow amend,amend:note` lets notes through while `exclude` asks. An
  action rule can only *tighten*. A tool whose actions nobody has an opinion about is judged by its
  capability as before.

  **`--allow amend` no longer covers those four** - the one thing here that will surprise an
  existing settings file.
- `context: request` reports items left out by their own state separately from items the projector
  dropped. `restore` fixes the first and does nothing for the second.
- `setup`, a fourth introspection tool, for what the session is running *with* rather than what
  happened. `model` (model, parameters, context limit, and whether the session was **resumed from a
  snapshot**), `tools`, `permissions`, and `policy` (compactor and projector). Pending permission
  requests are listed. It reads `Careful`'s table rather than calling `evaluate`, which would evict
  entries from the bounded queue of refusal reasons. Declares `Capability::Custom("setup")`, output
  limit 32,000 bytes, 255 tokens of spec; the four introspection tools total 1,846.
- `context: search` reads the archive without copying it back. Answers with how many lines match,
  what taking them would cost, and which items they are in; `take` shows that many. It never returns
  the item. Case is ignored.
- `log`, a third introspection tool: the session's record from the inside - what an item used to
  say, which permissions were answered how, which tools came and went.

  Every answer opens with the true total, which is what makes it safe to hand a model: a bare call
  returns no records, only how many there are, of what kinds, and what taking them would cost.
  `take`, `ids`, `since` and `kinds` narrow, against a header stating what exists rather than what
  matched, so truncation cannot read as absence. A malformed filter is refused rather than answered
  with an empty list, and a filter matching nothing lists the kinds the session does hold.

  Records come back raw, named the way the kernel names them, detailed by the same function that
  writes the trace pane. Declares `Capability::Custom("log")`, so it is separately grantable and
  revocable. No runtime change: `Kernel::with_history` was already there.

### changed

- `introspect` is called `context` - the tool id, the capability and the struct. The module, the
  `--introspect` flag and the `/introspect` command are unchanged.

  **A permission rule naming `introspect` stops matching** rather than failing, since nothing
  declares that capability any more. Spell it `context`; no alias is accepted. The tools are off by
  default, so a session that never passed `--introspect` is unaffected.
- A tool call on the trace is cyan, matching the chat tab. `.failed` still matches first.
- The UDP wording names the `landlock` crate rather than the LSM. ABI 10 (Linux 7.2) added the two
  UDP rights; the crate stops at ABI 9 and is `#[non_exhaustive]` over a sealed trait, so they
  cannot be requested from here. Nothing about the confinement changed. A test asserts the hole and
  is there to fail when the crate catches up. `AccessNet::ConnectTcp | AccessNet::BindTcp` stays
  spelled out rather than `from_all`, which would silently start handling UDP on a `cargo update`.

### fixed

- `/save sessions/` writes into the directory under the session's own name instead of creating
  `sessions/.json` and `sessions/.jsonl`.
- `/limit` marks rows for tools the session is not offering, instead of listing six in a session
  with four. The rows belong there: a limit set before a tool arrives is what it declares on
  arrival.
- `amend` reports a re-request of the state an item is already in as a rewritten reason rather than
  a move, and no longer puts a no-op step in its journal.
- `context: search` refuses a `take` it cannot read, rather than silently falling back to the
  summary, and `take: 0` no longer prints a heading with nothing under it.
- `context: search` counts what it actually read. `ids: [99]` in a session with no item 99 reported
  having searched it and found nothing; ids naming nothing are now named, including on an answer
  that found something.
- A second failure with the same wording is no longer swallowed. The dedup compared against the last
  *loose* line, which outlives its turn; it is scoped to the running turn now.
- Five things found by a live model resuming a session under a *second* model and being asked
  whether it wrote a turn the first one wrote:
  - `log` refuses arguments it does not take, with a sentence for `action` in particular. It used to
    answer `log {action: "look"}` as though nothing were wrong.
  - `log ids:[n]` reports an absence explicitly, in the zero-match case too. Five true
    `model.requested` rows were read as proof the model had written the item itself; what settled it
    was the `context.added` that was not there.
  - `since` is documented as exclusive, and `0` is named as the way to ask for everything. A model
    wrote `since: 1` and skipped `session.resumed`, the record that answered its question.
  - An empty filter list is accepted - it constrains nothing.
  - `look` opens with which items were already in the context and points at `setup model`. A
    restored item has no field marking it, so the listing read as though the model wrote all of it.
- The `amend` warning about hiding items with nothing written down no longer claims the content is
  destroyed. An elided item keeps every byte and only *projects* as a marker; a live session read
  the old wording and told its user the content was unrecoverable.
- A fork says when it left nothing out, so a question asking a model to disregard something is not
  reported as an ablation. `context`'s description also says its actions are actions of that tool -
  a model called `fork(...)` as a bare tool, then excluded the item from its own live session
  instead.
- A drained log says how many records went through it and where the next one starts, rather than
  "this session's log is empty, which is not the same as a log you have not been shown".
- A compaction record carries `CompactionReport::reason`, so a pass can be told from a pass that
  should not have happened.
- Answers no longer name tools the session has had taken away.
- Four things found by reading the real output: a filtered `log` header built from one prefix that
  fitted only half the sentence and reported the total as a match count; `short` took the last `::`
  segment, naming `Calibrating<BytesPerToken>` as `BytesPerToken>`; `setup` printed a full Rust path
  and listed eleven `ask` path rules at ninety tokens.
- `/introspect` and `--introspect`'s help name all four tools. The screen test now reads the
  announcement beside the registry.
- `log`'s "there are no actions here" reads the registry rather than a fixed list of siblings.
- Prose four changes had made false, in three notes, the module and crate docs, the README and
  `examples/recorded.rs`.

## [0.9.0] - 2026-09-12

### breaking

- `app::Traced` grew `wall: SystemTime` and `after_a_person: bool`. It has no private fields and is
  not `#[non_exhaustive]`, so `Traced { name, detail, at }` no longer compiles and there is no
  `Default` to spread from.
- `ui::HELP` is gone. `help::SECTIONS` is one page per tab and `help::everything()` is the whole of
  it as one string. `ui` re-exports both; `help` is a public module, so a screenless build can reach
  the text.

### changed

- F1 opens at the page for the tab it was pressed from - one page per tab, plus the slash commands
  and the keys that mean the same thing everywhere, plus a seventh while a tool is waiting. It was
  eight sections and about a hundred lines wherever it was pressed. Pages rather than a filter, so
  nothing is lost. A headless `/help` prints `help::everything()`.
- The trace's gap column is blank on the line that ends a wait for a person, so the largest figure
  in it is no longer how long somebody spent reading. `Traced::after_a_person` carries it, set where
  the program learns somebody acted rather than where the waiting began. The clock itself is
  untouched.

### added

- `/` filters the context and the trace, fuzzily, via `nucleo-matcher`. One row high, in the
  prompt's place, counting what it found beside the query. It takes `esc` before the arm that reads
  it as "stop the turn"; `ctrl+c` still interrupts. Arrows and paging stay with the pane. Closing or
  changing tabs clears it. A context row matches on the whole item, not the one line it shows.
- The trace shows when each event happened as well as the gap. An event carries two clocks: `at`
  stays an `Instant`, immune to the system clock being set mid-session, and `wall` is a `SystemTime`
  and is what gets rendered. The date is a rule drawn only where it changes, with the zone named.
  `time` is no longer optional, so a headless build pays for it; the local offset is read in `main`
  before the runtime is built, because `time` refuses once a program is threaded, and falls back to
  UTC and marks it.

### fixed

- `?` answers on an empty context or permissions tab. Both handlers return early with no rows, and
  `?` was inside the skipped part. It is answered beside F1 now, guarded by `Focus::Body`.
- Each pane says which of its empties it is in. The trace claimed a search was filtering a session
  that had not started; the context blamed `f` for rows a query had hidden, over a count that was
  both filters added together. No count is given while a search is on.
- `~` in a settings file expands on Windows: `HOME`, then `USERPROFILE`. The home is an argument
  rather than something `expanded` reads, and the separator goes through `std::path::is_separator`.
  The old test read the same variable to find its expected answer, so it panicked before asserting
  on the one platform where the function was wrong.

## [0.8.0] - 2026-09-11

### added

- A spend ceiling: `Setup::spend`, `--spend TOKENS` and `/spend`. It adds up `input + output` per
  response and stops the session by the ordinary door - the turn in flight is interrupted, what
  arrived is kept, the session is written out. `--deadline` bounds time, which a model stuck in a
  loop will stay inside while spending the whole of it.

  It belongs to the session: `App::on_event` counts and `App::start_turn` refuses, so an embedder
  cannot get round it. In tokens, because a figure in money would be a price list. A stopping rule
  rather than a cap, since a response's cost is known only once it has arrived. `/spend N` raises
  the ceiling, `/spend 0` removes it, and an endpoint reporting no usage is said so once.
- `--config-file PATH`, a JSON file for the settings otherwise typed every time, each key named
  after its argument and all optional.

  The command line wins, including over a value that happens to equal the default - the merge asks
  clap which arguments were *typed*. A list on the command line replaces the file's rather than
  adding to it. `--model` reads command line, then `KAMCHATKA_MODEL`, then the file. An unknown key
  is an error naming the ones that exist. A leading `~` in the two sandbox lists is expanded, and
  that is the only place in this crate that expands one. It carries nothing belonging to an
  invocation - a message, `-r`, `-f`, `--headless` - and there is no search for a file.
- `kamchatka.json` ships beside the readme: every setting at its default. It grants nothing -
  `allow` empty, both sandbox lists empty, `on-ask: deny`. The one non-default value is a spend
  ceiling, which is a tightening. `Settings` serializes too, so the suite holds the shipped file to
  having a key for every field.
- `--headless`: the same program driven by lines instead of keys. A line of stdin is a message or a
  `/` command; the session log goes to stdout one JSON record per line and the model's words to
  stderr. Implied when stdout is not a terminal, and says so.

  Records come from `Kernel::history_since`, so they are the same bytes `/save` writes, including
  `session.started` and `session.finished`. `--allow`/`--deny` answer permissions in advance and
  `--on-ask` covers the rest, defaulting to **deny**.

  A line is read only while the kernel rests, so lines cannot overtake the turns they belong to; a
  question is answered only once the kernel rests; the model's words end their line before anything
  else writes one; and the session is ended by the loop rather than its caller.
- `headless::Headless`, that loop, over any `AsyncBufRead` and two `Write`s. `--deadline 300`
  interrupts what is in flight, records it, and leaves by the ordinary door; `ctrl+c` does the same
  once and leaves at once if pressed again. Both are the driver's rather than a `timeout` around it,
  since a dropped future never finishes the session. `Headless::stops_on_ctrl_c` is off by default.
- `wiring::Setup`: a session assembled, with a `Default`, and `wire(provider)` returning the `App`
  and two receivers. Two of its steps are not guessable: the subscription must happen *before*
  anything is plugged in, and `introspect::install` returns a handle the caller must keep. It does
  not reach the network or read the environment.
- `mcp::attach`, which was `main.rs`'s own. The part that is not obvious is the name: it prefixes
  the server's tools and is what `always, for mcp:<name>` grants.
- A `tui` feature, on by default, holding the screen and keys. The binary declares
  `required-features = ["tui"]`. `cargo tree -e normal` goes from 267 crates to 179 with MCP on.
  Three of the six test suites run without it.
- The headless suite drives the binary against a socket, which is what lets the rest be tested
  without a key. It closed: a second `ctrl+c` against a tool that will not stop (an MCP server that
  never answers - a kernel interrupt lands between steps, and a call in flight is not); the first
  press stopping a `shell` command; `--spend` through the command line; the session written when
  nobody said `--no-record`; and a screenless build at a terminal, under a pty.
- A live test that a PDF goes out as a document in Google's native dialect. The OpenAI-dialect test
  cannot be pointed there - the shim answers `400 Invalid content part type: file`. With it,
  `cargo test -p kamchatka --test live` passes in full against a single Google key.

### changed

- `App::submit` returns a `Reply`: what the line did (`Did::Asked`, `Did::Queued`, `Did::Ran`), the
  lines it said, and the page it opened. Breaking. The page had no other way out - it came back by
  `take()`ing the overlay, so a command that opened no page reported the last one that did.
- `App::submit` and `App::interrupt` are `pub`. Every verb is reachable only through `submit`, and
  while it was `pub(super)` the only way in from outside was to synthesize a key press.
- `ui::HELP` moves to a private `help` module with the selector listing; `thousands` and `charged`
  move to `app::text`. The public path `ui::HELP` is unchanged.
- The chat-against-the-request property drives a generated sequence of moves rather than six chosen
  ones, and carries its own reachability check over the nine-move alphabet. Drawing an archived item
  on the chat is caught by this and nothing else.
- `--sandbox-allow` and `--sandbox-read` take a comma-separated list, like `--allow` and `--deny`.
  Repetition still works and survives a path with a comma in it.
- The live test about a turn carrying its thinking asserts that *wherever* a summary appears it is
  carried in the turn, in order, and findable through `thinking()`. Measured 2026-09-11, a response
  that makes a call carries no thought summary at all.
- Requires `nachalnik` 0.5.0, whose `PermissionPolicy::why` takes a `PermissionRequest`. `Careful`
  keeps its own record of the last sixty-four refusals, because its second reader is the permissions
  tab, which asks by identifier off an event carrying no arguments.

### fixed

- Every early stop in a headless run left the process hung. `tokio::io::stdin` reads on a blocking
  thread, a blocking read on an open pipe does not return, and dropping a runtime waits for its
  blocking threads. Only reachable with stdin still open, which no piped test is. The runtime is let
  go with `shutdown_background` as the last statement.
- A build with no screen panicked when run in a terminal. Having no screen was a notice beside a
  decision never made: the mode was `--headless` or a piped stdout, and neither is true of somebody
  typing at a terminal. Every test of this binary pipes stdout.
- A refusal from the three file tools names every path the session reaches, and a write refused for
  a read-only path names where it may write instead. It named only the working directory, which
  stopped being true the moment anybody passed `--sandbox-allow` or `--sandbox-read`.
- A line said while an answer was still arriving reaches a headless reader. The driver marked its
  place in `App::loose` by length, and that list is not append-only - a recorded turn takes back
  every line that streamed out of it. The spend ceiling's own stopping line went missing this way.
- `/spend` counts whether or not a ceiling is set, so `/spend N` part way through a session does not
  start from zero.
- A resumed headless run says how it is driven, not only what it picked up.
- `Entry` derives `Debug`.
- A headless run exits non-zero when its *last* turn failed. A turn that failed and was carried on
  from is a session that recovered.
- A suite for `--mcp` with `--headless`. They meet at a question: MCP tools declare `mcp:<server>`
  and a headless run has nobody to ask, so every call is refused unless `--allow mcp:<server>` was
  given in advance - recorded before the server has been spawned or said what it offers. A real
  child process, skipping without a Python interpreter. What it pins about a dead server is that a
  call fails rather than hangs.

## [0.7.0] - 2026-09-10

### added

- `/attach PATH [TEXT]` puts a file in the context and asks about it in one act. Text goes in as
  text; a PDF, image or recording goes in as `Content::Blob`. It *is* `-f`, one function reached two
  ways, so `-f report.pdf` no longer fails with a decoding error.

  The extension decides, for the ten types in `attach::TYPES` - sniffing gets the interesting case
  wrong, since an uncompressed PDF is valid UTF-8 for pages at a time. A file that is neither a
  listed type nor valid text is refused rather than guessed at. The path travels with the payload as
  a text block, because `LinearProjector` labels a reference by prepending to its text. Not pinned,
  where `-f` is.
- The corner figure is anchored on what the provider charged: reported cost, plus what the context
  estimates now, less what the estimator says the items that figure covered would cost now. An item
  that has not moved cancels and contributes its measured cost, so only what changed is estimated.
  It absorbs per-message framing, the tool schemas, and any blob already sent. Falls back to the
  plain estimate before any response, on an endpoint reporting no usage, and after a model change.
- `App::drafted` counts a message being typed, and the status line adds it to the anchored figure. A
  slash command counts as nothing.
- `/budget` says which of the two figures the corner shows and what the anchored one is built from.
- The context pane marks a row the counter would not price as `0+` rather than `0`. "Measured, and
  free" and "nothing priced this picture" were the same cell.
- A `Content::Blob` item draws as `[image/png, 12.05kB]`. This program renders no pictures.
  `/request`, `/payload` and `/raw` name it rather than printing megabytes of base64 - by shape
  rather than by length, so a long tool result is not cut.

### changed

- The chat is derived from the context rather than accumulated beside it. `App::transcript` is gone;
  `App::conversation` reads the context every frame and `App::loose` holds only what that cannot
  account for - mid-stream fragments and chrome.

  Gone with it: `Entry::item`, `Entry::was`, `App::attribute`, `App::attribute_waiting`,
  `App::resay`, `App::edit_of`, `App::said`, `App::retell`, `App::last_turn`. All answered one
  question - which line goes with which item - that a derived conversation never asks. `App::replay`
  now says only what a resumed session picked up.
- The chat shows the conversation the model is in: an item that is not projected is not on it. This
  reverses a decision - the context tab is already the record of what happened, so the chat answers
  "what is going" and the tab answers "what happened". An elided item stays, marked, because it *is*
  in the request; so does a turn whose tool call has not been answered yet.
- An elided turn reads as the projector's own marker, taken out of the projection rather than
  assembled again.
- An edit reads where the turn it replaced was. `App::in_order` places an item that replaces another
  in the other's place; `commit_edit` records which on `meta`, so a resumed session draws it there
  too. An edit supersedes, so otherwise a correction to the first question read last.
- A turn rewritten in place reads as it is now - there is no copy of the words taken on arrival.
- The rewrite line is `~ [id] · rewritten here, N earlier version(s)`, drawn for any rewrite.
- The chat drops the lines naming what a tool call cost, what the output limit took, and the stub
  above a rewritten turn - each is a fact about an item and a column on the context tab.
  `{tool} reported an error` survives.
- `App::ask` says, pushes and attributes a message as one act. `--message` did the first two, so the
  opening line of every `-m` session was unreachable by any context change.
- A reference's chat line names what the item carries and what nobody could price, derived off the
  item - which is why `/attach` says nothing for itself.
- `-f` takes any file through `attach::attached`, not only a text one.
- `Trim` takes a tool result carrying a blob first, and the size arithmetic gets no say about one.
  Every counter puts a blob at `0` tokens, and the pass runs on two rules that both read that
  figure. Size decides nothing in either direction: an eight-pixel PNG is a hundred bytes and 255
  tokens at a vendor charging 85 plus 170 a tile.
- `Trim::should_compact` answers yes to anything in the request the counter would not price. Without
  it a context that is mostly pictures never reached the threshold. `plan` still answers `None` when
  there is nothing it may take.
- The pass's summary names the blobs it took.
- `/budget` says how many pieces of content the counter would not price, and names the counter.
- `provider::connect` and `provider::gemini::connect` read `KAMCHATKA_API_KEY`,
  `KAMCHATKA_BASE_URL`, `KAMCHATKA_CONTEXT_LIMIT` and `KAMCHATKA_NO_ATTRIBUTION` and pass them in.
  The providers read no environment at all now. Nothing changes for anyone running the program.
- Two fewer direct dependencies: `reqwest` and `rustls` belong to the providers.

### fixed

- Past the limit the corner says `· the compactor runs first`. A pass runs when a request is
  *built*, so a tool loop left the corner at 270% in red while the request that followed cost 1,100.
  With no compactor it claims nothing.
- A file put in the context is announced once. The derived line is the one that cannot go stale.
- `/budget` no longer puts a Rust type path in the middle of a sentence.
- The compactor no longer fills the context it is clearing. Every pass wrote a summary and nothing
  took one back out, since a summary is a `Reference` and the pass only considers tool results - at
  a 6,000-token limit, twenty-one identical summaries and a quarter of the budget. Each pass now
  supersedes the last one's.
- A call waiting on a decision is not drawn as something the model is not being shown. The projector
  leaves a turn out while one of its calls has no result, so the chat drew "an assistant turn with
  no content and no answered calls" above the very call being authorised.
- `/model` and `/provider` drop the anchor. `App::anchored` documented that fallback and nothing
  implemented it.
- The corner figure cannot fall to `~0` and stay there. `Anchor` subtracted the whole of what each
  item *holds* from the provider's figure, so an elided item contributing one line had twelve
  thousand tokens taken off. It records which items' content was in the request and what the markers
  came to, separately.

### removed

- `kamchatka::provider::OpenAiCompatible`, `kamchatka::gemini::Gemini`, `provider::Endpoint`,
  `provider::same_model`, `provider::NOT_A_STREAM` and the `gemini` module. Both providers are now
  [`nachalnik-providers`](https://crates.io/crates/nachalnik-providers); nothing they do has
  changed. They moved because nobody else could use them - the runtime ships no provider by design,
  and the only two complete implementations were locked inside a terminal program.

## [0.6.1] - 2026-09-09

### added

- `/params` warns when a parameter would make the answer unreadable. Inception's `diffusing` sends
  the whole answer again at each denoising step, so a 120-character answer arrived as 1,443
  characters of drafts. A warning and not a refusal: parameters go to the provider verbatim.

### fixed

- What a turn cost to generate is on the screen, with the reasoning as a share of it:
  `1,412 out, 1,139 of it reasoning`, through one renderer in all four places. A model charged for
  reasoning it does not send back is said so once.
- The Gemini dialect's `output_tokens` includes the thinking. `candidatesTokenCount` alone
  understated a thinking-heavy turn fifty to one.
- A model listing is read under either name the dialects publish: `supported_parameters` and
  `supported_sampling_parameters`. The narrower name lists sampling knobs only, so
  `Endpoint::lists_every_parameter` says which kind of list is behind an answer and `/params` words
  an incomplete one as a note rather than an error.
- Thinking sent as a finished summary is read: `{"content": ..., "status": "complete"}` on the chunk
  rather than in the delta. More than one arrives per turn, each covering the reasoning since the
  last, so they are appended. A streamed request needs `reasoning_summary_wait: true` beside
  `reasoning_summary: true`, or the stream ends before any summary exists.
- A refused request is reported by the sentence inside it, including when `error.message` is a list
  rather than a string. Where the wording names nothing, `loc` is prepended.
- One failure is one red line. The guard was on the event and not on the outcome wrapping it.

## [0.6.0] - 2026-09-08

### added

- `/limit`, which is how much of each tool's output the model is shown, and now changeable.
  `/limit read 64000` moves one from that tool's next call onward; `Tool::spec` is called afresh for
  every request, so it lands without a restart. The limits live in one shared `Limits` table. Rows
  are numbered and the number is one the command takes. The already-shortened call is recovered a
  different way: its whole is archived beside the copy the model saw.
- The conversation marks which of itself the model is still being shown. An excluded, archived,
  elided or superseded turn keeps its place and words, with a rule down its left and a line naming
  the item and why it is out. Marked rather than hidden. The reading asks the projection, not the
  item's state. `app::Entry` grew public `item` and `was`.

### changed

- A session is named for when it started, in UTC, and that name is its two files:
  `/tmp/kamchatka/2026-09-08T06-45-17Z.jsonl`. `App::session_stamp` does the calendar in Howard
  Hinnant's `civil_from_days` - no dependency for a filename. Still to the second, so two sessions
  inside one second collide as before.
- A permission question stands in the prompt's place on the chat tab rather than as a modal overlay,
  and the prompt is on the chat tab only. Being asked whether `amend` may elide item 22 used to mean
  deciding with the box covering the list saying what item 22 is.

  The settling window is gone with it: a question used to take every key on arrival with a 300ms
  timer deciding which, and one session granted `shell` for the rest of it with the `a` of "what".
  `tab` is what gives a question the keys now. It takes the prompt's rows rather than sitting above
  them, so a short window cannot leave the prompt holding the keys off-screen. What was typed is not
  lost - the prompt is not drawn rather than cleared.

  `App::locked_key` is the guard: until `tab`, only the keys that scroll the conversation do
  anything. The panel carries `[tab] puts the keys here, and then:` rather than promising a key it
  has not got.

  This removes public API: `app::SETTLING`, `App::open` and `Overlay::Permission` are gone, replaced
  by `App::asked`, `App::prompted` and `App::question_scroll`.
- The permissions tab names the policy and what it answers about everything not listed:
  `Careful · anything it has not been told about: ask`. Both halves come from the policy -
  `Careful::untold` is what `Careful::stance` falls back to. With nothing decided, the tab says the
  emptiness is not permission.
- An item's `as stored` page opens with `included_because` where there is one.
- `/budget` says how much of the last request the provider served from cache. It prices a *change*
  rather than a request: the front of a request is the tool definitions and oldest messages, so
  anything rewriting them is paid for in full next time.
- `amend`'s `note` says what it is for. Thinking belongs to the turn that produced it, has no
  identifier, is not reliably carried back, and is not a row on the context tab. A note is an item:
  numbered, projected into every request, pinnable, visible.
- A fork leads with the answer and puts the thinking after it, since an output limit cuts from the
  end. On one 34,287-byte fork the thinking was 68% and sat last.
- An edit reads where the turn was, with the row above saying what happened. Saying the new text
  instead is wrong: `say` appends, so an old edit lands after everything that followed it.

### fixed

- Nothing this crate's test suites write goes in `/tmp`. Each cleared its directory on the way in
  rather than out and the name carried the process id, so thirty runs left 810 directories.
  `tests/common::scratch` hands out one under `CARGO_TARGET_TMPDIR`. One path stays in the temp
  directory, because the claim it checks is that the temp directory is *not* opened up.
- Two sandbox tests stopped leaving a confined command's scratch directory behind: they called
  `output()`, which consumes the child, then removed a directory named for the *test's* own id.
- A repair the request needs every time is said once rather than after every message, in the present
  tense, and the count is the whole of it. The trace keeps every one.
- The chat tab's border lights like the other three. It went yellow when the keys were on the tab's
  body, which on the chat tab they never are. The prompt now answers "where does what I type go?" in
  the same yellow the pinned question uses.
- An undone edit comes off the conversation with the item it named. It is a reading now rather than
  a copy: `Entry::text` keeps what was said and `App::said` asks the item what it says now.
- `cargo doc` builds again - four intra-doc links named private methods from public documentation.
- `Trim` does not ask for a pinned result. `ContextState::sends_content` says yes to one, so the
  filter took it and the kernel refused - and the plan still carried a summary, so three turns
  produced three summaries saying a result had been elided when none had, three undos spent, and the
  request climbing 2,637 → 2,716 → 2,788.
- `/budget` drops the percentage from its account of what the counter learned. It was computed from
  `scale - 1`, the error as a fraction of the guess, while the sentence reads as a fraction of the
  truth - 54.3% against 35.2%.
- `amend`'s list of moves no longer implies an excluded item is still charged for. Measured,
  `archive` and `exclude` produce the same request to the token.
- `Trim` credits itself with the net of each elision and skips a result no bigger than its marker.
  Each elision buys back the content less one copy of the pass's reason, about 21 tokens - so twenty
  seven-token confirmations "recovered" 140 tokens and took the request from 852 to 1,190, through
  the limit the pass exists to hold.
- A stream that stops arriving keeps what arrived, rather than failing the turn and dropping tokens
  already billed for. Retrying is worse: every attempt is billed. Both dialects, via the shared
  conformance suite.
- A row the projector repaired away says what it is holding. Whether an item is going cannot be read
  off its *state* - an item repaired away is `Active`, holding everything, and not in the request.
  `App::costs` became `App::going`, and `Going::sends_content` is what every column, reason, filter
  and figure asks. One projection per frame.
- One word per mechanism, in `amend` and at the prompt. The `prune` action with a `state` argument
  put one move's word over five, so "prune to pin it" was the documented way to protect something.
  The five moves are actions now, each named for the state it leaves behind; `/prune` becomes
  `/exclude` and `/keep` becomes `/pin`. The old spellings are still accepted and undocumented.
- The question about an `amend` names each item the way the context tab does, and expands a `select`
  into what it matches. Only for the two tools this program installs itself.
- A leading `~` is refused in words rather than becoming a directory called `~`. Not expanding is
  the right default and is kept; the three file tools share one `PATH_ARG` describing the rule, at
  87 tokens across the definitions.
- The `~` refusal says the same path will be refused again as it stands, and names no other concrete
  path - every concrete path in a refusal is read as a path to try. The literal-`~` spelling moved
  into `PATH_ARG`.
- `amend` accounts for a change with the reason for *that* change, so a note is not told its extra
  tokens are the marker of an elision it did not perform.
- A move given a `label` instead of `ids` is told the spelling: `select: "label:secrets.txt"`.
- The status line's give-way ladder gained the rung it was missing: after the address, the vendor
  prefix goes, then the name is cut from the left.
- A provider's notice reaches whoever holds the `App`, via `on_outcome`, so it sits with its turn.
- A compaction pass counts elisions, not only removals. The compactor here only ever elides, so
  every pass announced itself as `compacted: 0 items out`.
- Blank lines a provider puts in front of a message are not drawn. The item keeps what arrived.
  Cosmetic in the conversation and not in a tool result, whose preview is the first six *lines*.

## [0.5.0] - 2026-09-06

### added

- <kbd>ctrl+home</kbd> and <kbd>ctrl+end</kbd> go to the beginning and end of the conversation.
  Control is held because <kbd>home</kbd> and <kbd>end</kbd> belong to the prompt.
- `--sandbox-read PATH` opens a path for reading only, beside `--sandbox-allow`, which opens one for
  reading and writing. A confined `cargo build` fails on `~/.rustup/settings.toml` in a way that
  looks like a missing compiler; `--sandbox-allow ~/.rustup` would have fixed it and handed the
  model the ability to replace the toolchain.
- A refused confined command names the path outside its reach and what it can reach instead, under
  the status line where an output limit cannot take it. Landlock refuses with `EACCES`, which is
  also what the kernel says about somebody else's file. A refusal naming only reachable paths gets
  nothing.

### changed

- `sandbox::confine` takes `Option<&Path>` for the scratch directory and `sandbox::make_scratch`
  makes one, so "there is no scratch" is sayable rather than inferred.
- `network: deny` is described as refusing TCP, which is what Landlock's two network rights cover. A
  confined command can still send a UDP datagram. Nothing about the confinement changed.
- `--requests 0` says in `--help` that it means no limit.

### fixed

- The budget no longer charges for thinking this dialect cannot send. `send_reasoning` is on by
  default but `to_wire` has never put it on the wire and cannot. One turn with 3.4KB of thinking was
  charged 928 tokens against a request carrying 73. `Calibrating` cannot correct it, since the wedge
  grows with the number of reasoning turns while the scale is a single cumulative multiplier. The
  projection is `Endpoint::projection` now, rather than a second decision made in `main.rs`.
- `/load` puts every token figure on one scale, going through `Kernel::recalibrate` before counting.
  It counted first and corrected afterwards, so a context that came to 3,998 tokens read 2,002.
- `/budget`'s two halves answer the same question. The figure came from `tokens_withheld`, which
  counts an elided item, and the count beside it from what is not projected, which does not - so it
  read `held back: 9,004 tokens in 0 items`.
- `Trim` looks at what an item is *sending*. Its candidates were the projected tool results, and an
  elided item is projected as a marker - so every already-elided item came back as a candidate, the
  plan was never empty, and a summary went in before every request.
- `/load` hands over the tool call identifiers the snapshot already used; it dropped `used_calls` on
  the floor. The runtime grew `Kernel::reserve_calls` for it.
- The `shell` tool's temporary directory is created exclusively at `0700`. It has to be predictable,
  and `create_dir_all` was satisfied by anything already there, including a symlink left by another
  account.
- `introspect` declares the `whole` argument it reads. Three places told the model to use it while
  the schema did not declare it.
- The policy's two per-call notes drop the oldest past thirty-two rather than all of them.
- `amend`'s `note` does not spend two of the person's undos to write one thing down.
- A long answer keeps its beginning. The transcript bounded a still-arriving entry at eight thousand
  bytes and replaced what came before with `[...]`, and nothing put it back. The bound is on a
  tool's output and nothing else.
- Git survives a configuration it cannot read. Under Landlock `access(2)` answers from the file's
  own permissions, so git was told `~/.gitconfig` was readable, got `EACCES` opening it, and took
  the *unreadable configuration* branch - exit 128 for every git command in a confined session. Such
  a command is now handed `GIT_CONFIG_GLOBAL` pointing at nothing.
- A multi-byte character split across two reads is no longer destroyed. Both providers decoded each
  chunk lossily as it arrived, so `zażółć` came back `za??ółć` and went into the context, the
  transcript and the log. The buffer holds bytes and only whole lines are decoded.
- A confined command cannot truncate a file outside the working directory. The ruleset was built on
  ABI 1, and a right a ruleset does not *handle* is not restricted at all; truncation has had its
  own right since ABI 3, because `truncate(2)` takes a path and never opens the file. The ruleset
  asks for ABI 3, which brings `Refer`.
- A credential rule matches the resolved path rather than the raw argument split on `/`, so a `path`
  of `.env/` no longer opens `.env` unmatched.
- The pattern matcher backtracks - it took the first `find` hit, so `a*bc` refused `abcbc`.
- The session written on the way out goes into a `0700` directory.

## [0.4.0] - 2026-09-05

### added

- Every session is written out when it ends, to a temporary directory, with the path printed last.
  `--no-record` turns it off. The old condition was backwards: a session that ended badly is the one
  worth reading and the one that left nothing.
- `/params` shows what else the model takes and names any set parameter it does not. An unaccepted
  parameter is sent, ignored, and nothing says so, so a `seed` set for a reproducible run buys
  nothing and looks like it worked. Read from the listing the context limit already comes from.
- A `prune` that hides items while the agent has written nothing down says so, and names `note`. A
  run elided seventeen tool results in one call and then answered ten questions from a context
  holding none of it - wrong on every one, inventing a crate and seven enum variants.
- `look` returns a long item as its start and its end; `whole: true` asks for all of it. Reading an
  item copies it into the context, so a model inspecting a 9,000-token result to decide whether to
  keep it pays nearly what keeping it costs.
- Hiding an item says how to get it back, and names `undo`.
- `state` accepts any spelling for putting something back - `unelide`, `unexclude`, `unarchive`,
  `unpin`, `include`, `active` - deliberately left out of the schema's `enum`.
- The error for an unrecognised `state` says what each one does, not only what it is called.
- A prune that made the request *bigger* says so. An elided item's marker carries the reason for
  eliding it, which on a short item costs more than the content did.
- The requests say which program made them, where the endpoint ranks programs. This crate's
  directory and the word `kamchatka`, sent only to OpenRouter, matched on the authority so
  `openrouter.ai.example.com` is not it. `KAMCHATKA_NO_ATTRIBUTION` turns it off.
- <kbd>f</kbd> on the context tab lists only what the next request carries: it hides every row with
  a figure in the `held` column. Nothing is changed and nothing is logged. Asking for a hidden item
  by number says it is hidden rather than absent.
- `--forget-truncated` drops a tool's whole output once shortened rather than archiving it. The cost
  is in the file: one `grep` into `./target` put 11MB of build noise into every save. Keeping it is
  still the default.

### fixed

- A request that stalls is waited out, like a busy server. A connection that timed out used to take
  the session with it - eleven of fourteen runs against one upstream died that way while the same
  model answered a single request in six seconds. A refused connection is deliberately not retried.
- A request not yet answered is watched like a stream: `esc` stops it, silence is reported at ten
  seconds and then at intervals, and it gives up after 150. Only the stream was watched, so all of
  that began at the first byte; the longest measured case held the terminal for eighteen minutes.
  Google's dialect gets it too.
- Both readmes said the sandbox reaches further than it does. The system paths are readable on
  purpose, because a command that cannot read `/usr/bin` cannot be a command. Both now give the two
  commands that show the line: `cat /etc/passwd` works, `cat ~/.ssh/id_rsa` does not. The file tools
  refuse `/etc/passwd` by their own code and always did.
- A tool call numbered from one no longer leaves a phantom call at zero. The streamed `index` says
  which call a fragment belongs to and is not a position in a list; it is looked up now, so any base
  works and so does a provider that skips a number.
- An error arriving *mid-stream* is read too - it sends the object on its own with `message` at the
  top, where a refused request nests it under `error`.
- A refused request reports the server's sentence rather than its whole envelope. A spent daily
  quota came back as six hundred characters of JSON including the account's `user_id`, into the
  session log, which is a file people send each other.
- `Retry-After` is honoured where the server sends one, and past a minute this stops rather than
  sitting through four doublings. A per-minute limit answers `5`; a spent daily quota answers with
  the seconds until midnight.
- `amend` points a state-named action at the argument it belongs to, so a word it knows anywhere
  comes back with the call that would have worked.
- A figure too wide for its column no longer takes the columns from its neighbour. The `held` column
  is the one where an unbounded number can turn up; it is abbreviated when it will not fit.

## [0.3.0] - 2026-09-01

### added

- `Careful` hands its reason for a refusal to the model as well as the screen, through
  `PermissionPolicy::why`. The model used to read `the call was not permitted`, from which a
  standing `deny` and a one-off `n` are indistinguishable.
- A working marker: three dots under `asking` or `running`, joined after five seconds by elapsed
  time. Which dot is lit comes from the clock, so it stops where it is if the screen stops drawing.
- `/load [PATH]`, the other half of `/save`, as a context operation rather than a restart. Nothing
  is destroyed: the current context is archived, pinned items stay, the saved items arrive as new
  items with their parameters and calibration, and `u` twice puts it back. Refused while a turn is
  running or a call is waiting.
- <kbd>enter</kbd> on a context item opens a paged box moved between with `←` and `→`. `to the
  model` is what the item puts into the next request, read out of the projection of the whole
  context; `as stored` is what the item holds. It opens on the first.
- `v1`, `v2`, … pages: what an item said before it was rewritten, newest first, up to eight deep.
  `amend revise` replaces in place, so the old text exists nowhere but `context.replaced`. No
  runtime change.

### changed

- **Breaking, for the library:** `Overlay::Permission` is a struct variant carrying its own
  `scroll`, and `Overlay::Text` holds `pages` and `page` where it held one `body`.
- The trace says what its events carry. A third printed a dotted name against an empty line,
  including `context.replaced`. A test refuses a name with nothing beside it.
- The trace gains a gap column, blank under a tenth of a second. A log with no clock cannot answer
  which step was slow, and timestamps would make somebody subtract.
- The context pane has two token columns: `sending`, read out of the projection, and `held`. One
  column reported what an item held under a heading saying what it cost.
- The label column is as wide as the widest label rather than a fixed twenty-six.
- One word per mechanism: an output limit **truncates** and a compactor **elides**. Both were called
  "shortened", in the same pane on adjacent rows.
- The tool definitions are written for the thing that reads them - every argument says what it is
  for. `shell` says it is confined **when it is**, because a command stopped by Landlock returns an
  ordinary permission error and a model that cannot tell those apart tries `sudo`.
- Nothing a model reads is written in this program's own vocabulary - no `at the terminal`, which
  reads to a model like a state it should recognise.
- A path outside the sandbox tells the model what it can do instead, rather than suggesting a
  restart with `--sandbox-allow`.

### fixed

- A stopped `shell` command keeps the line saying so: it is on the exit line, first in the result,
  where an output limit cutting from the end cannot take it.
- A markdown table wider than the window is drawn rather than wrapped as prose: columns give widest
  first down to a floor, cells wrap with their styling, and the delimiter row's colons decide
  alignment.
- The greeting is `ui::GREETING`, with a test that the keys it names do what it says. Both halves of
  the old one were wrong.
- `ctrl+p` in an empty session says there is nothing in the context yet and what puts something
  there, rather than the runtime's sentence for a rule it is enforcing correctly. When the context
  is not empty and still sends nothing, the list of what was left out goes above the answer - it
  used to be thrown away, because the error returned before the list was built.
- A stalled request says so: silence for ten seconds is reported, again every thirty, and given up
  on at a hundred and fifty. The heartbeat only made a silent stream interruptible.
- The retry budget for a busy server belongs to the request rather than the session.
- The conversation stays where it is scrolled to. Every streamed fragment used to set the window
  back to following the newest line. `ctrl+e` follows again.
- A permission question whose arguments are longer than the screen keeps its answers. It is three
  regions now, with the arguments scrolling between the header and the answers. Both overlays
  remember how far they really scrolled rather than how many times a key was pressed.

## [0.2.0] - 2026-08-30

A second wire format, in which a turn keeps its order, and two tools an agent reads and manages its
own context with.

### added

- `--gemini` talks to Google's own API. `generateContent` answers with `content.parts[]` in the
  order they were produced, where the compatible shim flattens that into a `content` string beside a
  `tool_calls` array. Recorded as `Content::Blocks` and sent back the same way, with
  `LinearProjector::send_blocks` on.

  Signatures are the reason to bother: this API answers `400 Function call is missing a
  thought_signature` to a turn returned without one, and it signs text parts as well as calls.
  `finishReason` does not decide the stop reason - it says `STOP` for a turn that asked for three
  tools.
- `provider::Endpoint`, the half of a provider the person at the terminal drives: where the requests
  go, what is served, which model, what the last retry was about. `App` holds an `Arc<dyn Endpoint>`
  and never learns which wire format is behind it.
- `introspect` and `amend` read an ordered turn. The guard stopping `amend` excising the turn it is
  speaking in finds that turn by its calls wherever they are recorded.
- `--introspect`, and `/introspect` while running, offer two tools for reading and managing the
  model's own context. Off by default.

  `introspect` reads. `look` lists every item with its state, cost and the projector's reason for
  leaving it out, and reads any in full. `budget` is what a decision about what to give up is made
  from: the next request against the limit, split into context and tool definitions, what the last
  one really cost, the correction learned from the difference, and the most expensive items
  *actually* going in. `request` summarizes rather than quotes - the request *is* the context, so
  quoting would double every token being asked about. `draft` and `fork` snapshot the context,
  resume it as a second kernel with no tools and one request, and return only what it said. A fork
  can think and cannot act.

  `amend` manages. `prune` moves items between states, by `ids` or `select`. `revise` rewrites an
  item, recording the old text as `context.replaced`. `note` writes into the context, attributed to
  `agent`, pinnable. `undo` and `redo` walk this tool's own changes - deliberately not
  `Kernel::undo`, whose stack belongs to the person and whose top while a tool runs is the assistant
  turn that asked for the call. A reason is required on every change. A pinned item, a system
  instruction and the turn the model is speaking in are refused.

  Two tools rather than one with a mode argument: a `ToolSpec` declares its capabilities once, so
  one tool would make "may it read its own context?" and "may it rewrite a tool result?" the same
  question. They hold a weak handle to the `App`'s kernel, so `/introspect` taking them away really
  takes their reach away.

## [0.1.0] - 2026-08-29

The first release: a terminal agent built on `nachalnik`, and a demonstration of it.

### added

- Four tabs, each taking the whole window: `chat`, `context`, `trace`, `permissions`. `ctrl+t` for
  the next, `alt+1`–`alt+4` for one in particular, `tab` between the prompt and the open tab. A
  message sent into a running turn waits, says so, then gets a turn of its own. The prompt wraps at
  word bounds and grows to hold every row; a pasted block arrives as the lines it was pasted as, via
  bracketed paste. A scrollbar runs down the right border rather than in a column of its own.
- The context tab is a table: every item, its kind, cost, whether it is going into the next request,
  and what the model will read of it - or why not, in the projector's words. `space` cycles how much
  the model gets, `p` pins, `enter` reads the whole, `u` undoes, `23G` goes to item 23. The middle
  step is the one worth a key: taking a tool result out makes the projector drop the call that asked
  for it, and eliding leaves the call answered.
- `e` on a context item changes what it says, through `Kernel::supersede`.
- The trace tab is every event as it happens, in the session log's own names, two aligned columns,
  wrapped rather than cut off.
- A `permissions` tab: every capability the policy has an opinion about *and* every capability a
  registered tool declares, what the policy will answer, and which tools that covers. `space` cycles
  a row; `a`/`n`/`r` set one directly. The permission prompt writes to the same table. Cycling back
  to `ask` takes a row off the tab.
- **Every stance starts at `ask`**, `read` and `network` included. Both `read: allow` and
  `network: deny` would be answers given on somebody's behalf before they had been asked.
- Permissions are finer than a capability: `Careful` holds path rules as well as stances - `.env*`,
  `*.pem`, `id_rsa*`, `.ssh/` and a few more, all `ask` - and the strictest of everything consulted
  wins. They bind `read`, `write` and `edit` and deliberately not `shell`, since a check over a
  command string would refuse `cat .env` while waving `sed -n 1p .env` through.
- The permission question names everything the policy consults, and `[a] always` answers for all of
  it including calls already waiting. Arguments are shown as the lines they are; `[i]` shows the
  JSON verbatim. A question arriving while somebody is typing does not take the typing as an answer.
  `d` drops every waiting call with one reason, and the model is told.
- A call the policy refuses on its own says which stance refused it.
- Four tools (`read`, `write`, `edit`, `shell`) and a policy that asks about all of it. "Always"
  answers for a capability rather than a tool name. The `network` stance is consulted for a `shell`
  command naming a program that goes out to the network, because no tool declares
  `Capability::Network` and a model that wants the network writes `curl`. The heuristic is over the
  command as written rather than a sandbox, and says so.
- **The `shell` tool runs under [Landlock](https://landlock.io)**, so the stances are enforced
  rather than reported: `network: deny` is refused by the kernel at `connect()` and `write: deny`
  makes the working directory read-only. Applied by re-executing this program in a mode that
  confines itself and then *becomes* the command - Landlock restricts the calling thread, and the
  `exec` means the domain is inherited and a stopped call kills the command rather than a helper. A
  directory of the run's own is handed over as `TMPDIR`; `/tmp` itself is not opened up.
- `read`, `write` and `edit` are held to the same boundary by their own code, resolving `..` and
  symlinks before comparing. Weaker in kind than a ruleset, and said to be.
- `--sandbox-allow PATH` opens another path, `--no-sandbox` turns the whole thing off, and the
  permissions tab says which of `shell: confined` and `shell: a command can do any of these` is
  true. If it cannot confine, the shell runs unconfined and the tab says so.
- The model's answers render as markdown, via `tui-markdown` with this crate's own styling - the
  defaults put a coloured slab behind headings and code. Nothing else is treated as markdown: a
  tool's output is what the tool said.
- Fenced code blocks are syntax-coloured by token *name* via `synoptic`, split out before the
  markdown renderer sees them, so a block still streaming in is a block.
- The secondary things are `Gray` rather than `DarkGray`, which is the terminal's bright *black* and
  sits a shade off the background on many themes.
- `/step` performs exactly one transition instead of a whole turn, which is the only way to stand in
  `State::Ready`.
- `/seams` says what is plugged into each of the runtime's six parts, asked of the kernel.
- `/tools drop ID` stops offering a tool from the next request onward.
- `ctrl+p` heads the request with what the projector left out and what it repaired.
- `/budget`, and a `~` on the status line's estimate beside what the provider really charged.
- Cooperative stopping on `esc`: the provider returns what it streamed, the shell tool kills the
  command and everything it started, since it runs in a process group of its own, and still answers
  the call. Both wait on a heartbeat rather than the next byte.
- A compactor that elides the oldest tool results past `--compact` of the limit, refused anything
  pinned. It elides rather than removes, so each call keeps its answer. Nothing is deleted.
- MCP servers with `--mcp '[name=]cmd args'`, behind the default `mcp` feature. The name prefixes
  the server's tools and is what an "always" grant is for.
- `/model [ID]` and `/provider [URL [ID]]` show or change the model and address without restarting.
  Both are shown, because the same model name at a different address is a different model. The key
  is not changed with the address: a key typed at the prompt would be a key in the transcript.
- `/models [FILTER]` asks the endpoint what it serves and marks the one in use with `▸`. The ids
  belong to the address, so `/model` was a command you could only use if you already knew what to
  type.
- The status line carries the host beside the model name; where it does not fit, the address gives
  way rather than the figures.
- **The TLS is `rustls` over `ring`**, so building needs nothing installed first. In reqwest 0.13
  `default-tls` means rustls with `aws-lc-rs`: 1,659 C files and, on some platforms, cmake and NASM.
  `ring` is 17 C and assembly files and no system libraries.
- `--help` lists the environment as well as the flags. `KAMCHATKA_BASE_URL` and
  `KAMCHATKA_CONTEXT_LIMIT` are read directly and so appeared nowhere the program would tell you
  about.
- `/save PATH` writes the event log and a resumable snapshot beside it; `-r PATH` picks it back up
  in a fresh process. Both take a path you chose - there is no session id and no server.
- Tested by drawing the screen into a `TestBackend` and reading the characters back, against a
  scripted model.
