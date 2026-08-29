# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- **The `shell` tool runs under [Landlock](https://landlock.io)**, so the permission stances are
  enforced rather than reported: `network: deny` is refused by the kernel at `connect()`,
  `write: deny` makes the working directory read-only, and nothing outside that directory is
  readable or writable either way. It is applied by re-executing this program in a mode that
  confines itself and then *becomes* the command - Landlock restricts the calling thread, and a
  single-threaded helper is the shape that needs no thought about which one; the domain is
  inherited across the `exec`, so nothing is given up by leaving, and a command that is stopped is
  reached by the signal rather than sheltering behind a helper. A directory of the
  run's own is handed over as `TMPDIR`; `/tmp` itself is not opened up, and the directory is
  removed by the process that spawned the command, that being the only one of the two which can:
  unlinking a directory is a write to the one it sits in, and the confined process cannot write
  there.
- `read`, `write` and `edit` are held to the same boundary by their own code, resolving `..` and
  symlinks before comparing. Weaker in kind than a ruleset, and said to be.
- The binary that confines a command is settled once at startup rather than asked for per call.
  `current_exe()` reads `/proc/self/exe`, so a `cargo build` in the repository being worked on
  replaced it mid-session and every command came back `No such file or directory` with nothing on
  screen accounting for it. If it cannot confine, the shell runs unconfined and the tab says so.
- `--sandbox-allow PATH` opens up another path, `--no-sandbox` turns the whole thing off, and the
  permissions tab says which of `shell: confined` and `shell: a command can do any of these` is
  true here.

- Fenced code blocks in the model's answers are syntax-coloured, by token *name* rather than by
  theme: `synoptic` says which pieces are comments, strings, keywords, numbers and calls, and this
  program picks the colours. The fences are split out before the markdown renderer sees them,
  which is what makes the language, the whole block and the rule down its left all available at
  once - a block still streaming in is a block, and one in a language nothing recognises still
  gets the rule.
- A scrollbar down the right border of any tab holding more than fits, and of the overlays. It is
  drawn on the border rather than in a column of its own, so nothing gets narrower and a window
  with nothing to scroll looks exactly as it did. The thumb reaches the last row of the track when
  the content reaches its last row: `ScrollbarState` counts scroll *positions*, not rows, and these
  tabs stop at the last full page rather than scrolling the final row up to the top.

### added

- The permissions tab says `a shell command can do any of these` while a registered tool that
  runs commands is not refused outright. `Capability::Shell` subsumes every other capability, so a
  tab that listed five verdicts and said nothing about that was reporting four restrictions that
  are not there.
- A call the policy refuses on its own says which stance refused it - `shell: refused by
  `network`, which this command reaches for` - because the tool result records only `the call was
  not permitted`, and when the tool's own capability is `allow` that leaves a refused call with
  nothing on screen accounting for it.

### changed

- The permissions tab lists the answers somebody has actually given, and counts the rest along the
  bottom. `ask` is what the policy does when it has not been told anything, and a screenful of it
  buried the one or two lines that say what this agent can do without stopping. Cycling a row back
  to `ask` takes it off the tab, which is what taking a decision back looks like.

- The `network` stance starts at `ask` rather than `deny`. Refusing outright was a decision made on
  the user's behalf about something they may perfectly well want, and now that the sandbox enforces
  either answer, the answer is theirs to give.
- Permissions are finer than a capability where that is worth anything: `Careful` holds path rules
  as well as capability stances - `.env*`, `*.pem`, `id_rsa*`, `.ssh/` and a few more, all `ask` -
  and the strictest of everything consulted wins. Reading `src/main.rs` is silent; reading `.env`
  is a question. They bind `read`, `write` and `edit` and deliberately not `shell`, because a
  command names its files inside a string and a check over that string would refuse `cat .env`
  while waving `sed -n 1p .env` through.
- The permission question names everything the policy actually consults, and `[a] always` answers
  for all of it - including the calls already waiting behind it. A `yes, always` that answered only
  for the declared capability would ask again on the very next call, whether the question came from
  the network fold or from a path rule; and a model that asks for three commands in one answer
  produces three questions, all of them decided before the first is drawn, so an `always` that did
  not reach them went back on itself one keystroke later. Anything still waiting that the policy
  would now let through is let through; anything that needs something else is still a question.

- The `network` stance is consulted for a `shell` call whose command names a program that goes out
  to the network - `curl`, `pip install`, `git push`. No tool declares `Capability::Network`,
  because a model that wants the network writes `curl`, so the row read `deny` beside `nothing
  registered needs it`: a restriction that was not there. It now names the shell it reaches, and
  the policy's own documentation is plain about the heuristic being over the command as written
  rather than a sandbox.
- Everything a person is meant to read is a step lighter. `DarkGray` is the terminal's bright
  *black* and sits a shade off the background on many themes, and nearly every secondary thing on
  these screens was drawn in it - why an item is not being sent, what an event says, the context
  and permissions headers, the status line. Those are `Gray` now; `DarkGray` is left to the things
  that draw lines rather than words.
- The selected row of a tab the keys are not on is underlined rather than backed by a slab of
  `Rgb(40, 40, 40)`, which was a shade of a background this program does not know it has.
- `/budget` asks whichever counter is installed what it has learned, through the kernel, rather
  than keeping a typed handle to one this program set up. A counter that never corrects itself now
  says so in a sentence instead of leaving the line out.

### 0.1.0

The first release: a terminal agent built on `nachalnik`, and a demonstration of it.

- The model's answers are rendered as markdown - headings, emphasis, inline code, lists and
  fenced blocks - because a terminal that printed the asterisks would be showing the punctuation
  instead of what it meant. `tui-markdown` does the parsing; the styling is this crate's, since
  the defaults put a coloured slab behind headings and code, which reads as a redaction on a dark
  theme and a bruise on a light one. Nothing else is treated as markdown: a tool's output is what
  the tool said.
- `/step` performs exactly one transition of the state machine instead of a whole turn, which is
  the only way to stand in `State::Ready` - the moment the model has said what it wants to do and
  none of it has run. A turn walks through that state without ever drawing it.
- `e` on a context item changes what it says, through `Kernel::supersede`: the original stays,
  marked `~`, naming the item that replaced it, and one `u` brings it back. `space` and `p` decide
  whether the model reads an item; this decides what it reads.
- `d` at the permission prompt drops every call the model is waiting on, with one reason, and the
  model is told - a call that silently vanished would leave it waiting.
- `/seams` says what is plugged into each of the runtime's six parts, asked of the kernel rather
  than restated from what this program set up: the provider, the tools, the policy, the projector,
  the counter and the compactor - or that no compactor is installed and nothing will ever be
  dropped to make room.
- `/tools drop ID` stops offering a tool from the next request onward, because the kernel's
  registry is live rather than fixed at startup.
- `/prune` with no selector prints the language rather than reporting that the empty string is not
  a selector, and `23G` on the context tab goes to the item numbered 23 - the number every note
  names and every selector takes.
- A `permissions` tab: every capability the policy has an opinion about *and* every capability a
  registered tool declares, what the policy will answer about each, and which tools that covers.
  `space` cycles a row through ask, allow and deny; `a`/`n`/`r` set one directly. The permission
  prompt writes to the same table, so "always" and the tab are one object rather than two - and a
  refusal is visible in advance rather than only when it fires.
- Four tabs, each taking the whole window: `chat`, `context`, `trace`, `permissions`. `ctrl+t`
  for the next, `alt+1` to `alt+4` for one in particular, `tab` between the prompt and the open
  tab. The prompt and the status line are under all of them. A pasted block goes into the prompt
  as the lines it was pasted as: bracketed paste stops a pasted newline being read as `enter` and
  sending half of what was pasted, and the carriage returns a terminal spells those newlines with
  are put back, or the whole of it arrives as one line with invisible characters in it.
- The context tab is a table: every item the runtime holds, its kind, what it costs, whether it is
  going into the next request, and what the model will actually read of it - or, for the ones that
  are not going, why not, in the projector's own words. `space` takes one out and puts it back,
  `p` pins it, `enter` reads the whole of it, `u` undoes the last change.
- The trace tab is every event as it happens, in the same names the session log uses, in two
  aligned columns, wrapped rather than cut off, and readable backwards.
- `ctrl+p` heads the request with what the projector left out and what it repaired, because "why
  is that not in there?" is the question somebody opens it to answer.
- `/budget`, and a `~` on the status line's estimate beside what the provider really charged: the
  runtime's counter corrects itself from the difference, and this is where that is visible.
- A tool's arguments are shown as the lines they are rather than as `\n` inside a JSON string,
  since the permission question is the moment somebody has to read them; `[i]` still shows the
  JSON verbatim.
- Four tools (`read`, `write`, `edit`, `shell`) and a policy that allows reading, refuses the
  network and asks about the rest. "Always" answers for a capability rather than a tool name, so
  it works for tools the program has never heard of.
- Cooperative stopping on `esc`: the provider returns what it had streamed, the shell tool kills
  the command - and everything the command started, since it runs in a process group of its own -
  and still answers the call, and the partial turn is an ordinary context item. Both of them wait
  on a heartbeat rather than on the next byte, so a model or a command that has said nothing at
  all is as interruptible as a chatty one.
- A compactor that drops the oldest tool results past `--compact` of the limit, says which ones,
  and is refused anything pinned. Nothing is deleted; items are excluded, and restoring one is a
  keystroke.
- MCP servers with `--mcp '[name=]cmd args'`, behind the default `mcp` feature. The name prefixes
  the server's tools and is what an "always" grant is for, so it is worth giving: taken from the
  program it would be `npx` for most of them.
- `/save PATH` writes the event log and a resumable snapshot beside it; `-r PATH` picks it back
  up in a fresh process. Both take a path you chose, on your disk - there is no session id, no
  server, and nothing to look up. Saving over files that already exist says which ones it
  replaced.
- Tested by drawing the screen into a `TestBackend` and reading the characters back, against a
  scripted model - including that an item taken out of the context really does leave the next
  request.
