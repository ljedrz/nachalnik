# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

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
