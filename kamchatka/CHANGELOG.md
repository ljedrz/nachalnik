# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### changed

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
  tab. The prompt and the status line are under all of them.
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
  its child and still answers the call, and the partial turn is an ordinary context item. Both of
  them wait on a heartbeat rather than on the next byte, so a model or a command that has said
  nothing at all is as interruptible as a chatty one.
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
