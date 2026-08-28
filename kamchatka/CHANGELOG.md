# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### 0.1.0

The first release: a terminal agent built on `nachalnik`, and a demonstration of it.

- The model's answers are rendered as markdown - headings, emphasis, inline code, lists and
  fenced blocks - because a terminal that printed the asterisks would be showing the punctuation
  instead of what it meant. `tui-markdown` does the parsing; the styling is this crate's, since
  the defaults put a coloured slab behind headings and code, which reads as a redaction on a dark
  theme and a bruise on a light one. Nothing else is treated as markdown: a tool's output is what
  the tool said.
- Three tabs, each taking the whole window: `chat`, `context`, `trace`. `ctrl+t` for the next,
  `alt+1`/`2`/`3` for one in particular, `tab` between the prompt and the open tab. The prompt and
  the status line are under all three.
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
  its child and still answers the call, and the partial turn is an ordinary context item.
- A compactor that drops the oldest tool results past `--compact` of the limit, says which ones,
  and is refused anything pinned. Nothing is deleted; items are excluded, and restoring one is a
  keystroke.
- MCP servers with `--mcp '[name=]cmd args'`, behind the default `mcp` feature. The name prefixes
  the server's tools and is what an "always" grant is for, so it is worth giving: taken from the
  program it would be `npx` for most of them.
- `/save` writes the event log and a resumable snapshot; `-r` picks it back up.
- Tested by drawing the screen into a `TestBackend` and reading the characters back, against a
  scripted model - including that an item taken out of the context really does leave the next
  request.
