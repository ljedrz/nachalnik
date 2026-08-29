# kamchatka

[![crates.io](https://img.shields.io/crates/v/kamchatka.svg)](https://crates.io/crates/kamchatka)
[![docs.rs](https://docs.rs/kamchatka/badge.svg)](https://docs.rs/kamchatka)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**A terminal agent that shows you its context.**

Built on [`nachalnik`](../nachalnik), and built to demonstrate it. Everything in here is
ordinary user code — the provider, the four tools, the permission policy, the compactor, the
drawing. The runtime supplies the state machine, the context and the paper trail.

```console
$ export KAMCHATKA_API_KEY=sk-or-...
$ kamchatka -m qwen/qwen3-coder -f src/lib.rs "what does this crate do?"
```

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│> what does the kernel do?                                                                                    │
│                                                                                                              │
│⟩ read({"path":"src/kernel.rs"})                                                                              │
│                                                                                                              │
││ pub struct Kernel(Arc<InnerKernel>);                                                                        │
│  // ... 900 more lines                                                                                       │
│                                                                                                              │
│· read: 15 tokens                                                                                             │
│                                                                                                              │
│The kernel is a state machine with five states. `step` performs one transition and returns the state it       │
│produced; `turn` repeats it until the model stops asking for tools. Nothing in it decides what the model is    │
│told - that is the projector's job.                                                                           │
│                                                                                                              │
└──────────────────────────────────────────────── alt+1 chat · alt+2 context · alt+3 trace · alt+4 permissions ┘
┌ you ─────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ask for something, or /help                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 done · gpt-4o-mini · ~1,168 tokens, 0.9% (128k) · 1,102 really · 15 held back · F1 for the keys
```

> **The permissions are enforced, and it is still a demonstration rather than a hardened agent.**
> The `shell` tool runs under [Landlock](https://landlock.io): `network: deny` is refused by the
> kernel at `connect()`, `write: deny` makes the working directory read-only, and nothing outside
> that directory is reachable either way. The three file tools are held to the same boundary by
> their own code. The permissions tab says which you have — `shell: confined`, or
> `shell: a command can do any of these` where Landlock is not available. `--sandbox-allow PATH`
> opens up more and `--no-sandbox` turns it off. Within the boundary the file tools are finer than
> a capability: `read` is allowed, and `read .env` is a question, because a path rule can tighten
> what a capability allows. It is one LSM, not a container; see
> [what it does and does not protect you from](../README.md#-what-it-does-and-does-not-protect-you-from).

## 👉 four tabs, one window

<kbd>ctrl+t</kbd> for the next one, or <kbd>alt+1</kbd> … <kbd>alt+4</kbd> directly. The prompt and
the status line are under all of them, so a message can be sent from anywhere and the budget is
always in view.

**chat** is the conversation, and every terminal agent has one. **context** is why this exists:

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│  id  label                      kind                tokens  what it says, or why it is not being sent        │
│  1 ▪ src/kernel.rs              reference            1,045  pub struct Kernel;                               │
│  2 · user                       user_message             6  what does the kernel do?                         │
│  3 · assistant                  assistant_message        7  asked for read                                   │
│  4 - read                       tool_result             15  excluded: pruned at the terminal                 │
│  5 · assistant                  assistant_message        7  asked for shell                                  │
│  6 · shell                      tool_result             10  test result: ok. 175 passed; 0 failed            │
│  7 · assistant                  assistant_message       62  The kernel is a state machine with five states. …│
│                                                                                                              │
└──────────────────────────────────────────────────────────────────────────────────────── 7 items, 1 not going ┘
```

That is not a summary and not a debug view. It is the list of items the runtime is holding, in
order, with what each one costs, whether it is going into the next request — and the column that
matters most, what the model will actually read of it. Item 4 is marked `-` and says on its own
row why it is out, in the projector's words. Item 1 is `▪`, pinned, so the compactor will be
refused if it comes for it. Nothing disappeared: things changed state, and the state is on screen.

<kbd>tab</kbd> moves the keys between the prompt and the table:

| key | what happens |
| --- | --- |
| <kbd>space</kbd> | take an item out of the next request, or put it back |
| <kbd>p</kbd> | pin it, so that the compactor is refused if it tries |
| <kbd>e</kbd> | change what it says |
| <kbd>enter</kbd> | read the whole of what it says |
| <kbd>u</kbd> / <kbd>U</kbd> | undo / redo the last change to the context |
| <kbd>23G</kbd> | go to the item numbered 23 — the number `/prune` takes |

An oversized tool result is held as *two* items: the shortened copy the model was shown, and the
whole of it beside it, marked `▫ archived` and not going. <kbd>space</kbd> or <kbd>p</kbd> on that
row is how you say **send the whole thing** — it is the only way to say it, and the token count in
the row is what it will cost you.

<kbd>e</kbd> is the verb the others were missing. `space` and `p` decide whether the model reads an
item; `e` decides **what** it reads. The prompt turns into an editor holding the item's text, and
committing supersedes the old one rather than overwriting it:

```text
  1 ~ ledger.py    reference    469  superseded: superseded by item 8
  8 ▪ ledger.py    reference    477  """A running-balance ledger.
```

The original is still there, still readable, still one <kbd>u</kbd> from coming back — and the
next request carries only the edit, because a superseded item is not projected. Trimming a
2,000-line file down to the function that matters is two keystrokes and a delete.

**trace** is every event the runtime emits, as it happens, in the same names the session log is
made of:

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│model.requested       6 messages, 4 tools, ~1579 tokens                                                       │
│context.added         [7] assistant, 72 tokens                                                                │
│model.finished        EndTurn, 1522 in / 19 out (reported)                                                    │
│tool.requested        shell                                                                                   │
│permission.requested  shell (3)                                                                               │
│state.changed         requesting → deciding                                                                   │
│context.recounted     1425 → 1377 tokens                                                                      │
│permission.decided    shell: allow, by the User                                                               │
│state.changed         deciding → ready                                                                        │
│state.changed         ready → executing                                                                       │
│tool.started          shell                                                                                   │
│tool.output           shell, 12 bytes                                                                         │
│context.added         [8] shell, 23 tokens                                                                    │
│tool.finished         shell, 23 tokens                                                                        │
└──────────────────────────────────────────────────────────────────── 41 events · /save keeps them all ────────┘
```

It is the same stream `/save` writes to a `.jsonl`, and reading it is how you find out that a
permission question became a decision became a state change became a call. <kbd>tab</kbd> then
<kbd>up</kbd> reads back through it.

And from anywhere, <kbd>ctrl+p</kbd> prints the request those items add up to — the kernel's own
rendering of it, not a description, with a header naming everything the projector left out and
why:

```text
12 item(s) in, 4 out:
  [13] left out: an assistant turn with no content and no answered calls
  [14] left out: archived: the whole output; the model was shown a shortened copy
  [15] left out: excluded: pruned by `tool:shell:latest`
  repaired: dropped the call `call_301842` (shell) from item 13: its result is not
            in the projection
```

"why is that not in there?" is the question this whole runtime is for, and the JSON on its own can
only answer the other one. `/payload` goes one further and prints what the provider will put on
the wire, byte for byte.

## ✍️ what the model writes

Models answer in markdown, so the chat tab reads it as markdown: headings and emphasis get weight
and colour, inline code gets told apart from prose, list continuations hang under their bullets,
and a fenced block gets a rule down its left instead of a slab of background — which is the one
thing a terminal cannot do without knowing what colour the theme is.

The parsing is [`tui-markdown`](https://crates.io/crates/tui-markdown); only the styling is this
crate's. Nothing else on the screen is treated as markdown: a tool's output is what the tool said,
and running that through a renderer would be inventing structure it never had.

## ⌨️ the rest of the keys

| key | what happens |
| --- | --- |
| <kbd>enter</kbd> / <kbd>alt+enter</kbd> | send / a new line |
| <kbd>tab</kbd> | move between the prompt and the open tab |
| <kbd>ctrl+t</kbd> | the next tab; <kbd>alt+1</kbd> … <kbd>alt+4</kbd> for one in particular |
| <kbd>esc</kbd> | stop what is running, and keep what arrived |
| <kbd>ctrl+c</kbd> | the same, and again to leave |
| <kbd>F1</kbd> | all of them, including the slash commands |

Stopping is cooperative rather than a killed process. The provider notices between fragments and
returns the text it has; the shell tool kills the command — and everything the command started,
since it runs in a process group of its own — and still answers the call it was given. The partial
turn ends up in the context like any other, where it can be read, pruned, or left alone.

A message sent while a turn is running **waits for the end of it**, and then goes in and gets a
turn of its own — and says so when you send it, because until the turn ends it is on the screen and
not yet in the context, which is the one moment those two disagree. It cannot go in any earlier: the answer the model is still writing would land
after it, leaving the next request ending with the model talking rather than with your question —
and mid-loop it would land between a tool call and that call's result, where a request cannot have
a user message. So a message typed to steer a turn is answered after that turn rather than during
it.

A pasted block arrives as the lines it was pasted as: bracketed paste keeps a pasted newline from
being read as <kbd>enter</kbd> and sending half of it, and the carriage returns a terminal spells
those newlines with are put back.

## 🔑 the permissions tab

The other place the policy appears is the permission prompt — one call at a time, at the moment
you are least inclined to think about it. **permissions** is every answer you have given, in one
place, where it can be changed:

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│  capability or path     answer      the tools it covers                                                      │
│  read                   allow       read                                                                     │
│  write                  deny        write                                                                    │
│  shell                  allow       shell                                                                    │
│  network                allow       shell, when the command reaches for it                                   │
│  .env*                  deny        edit, read, write                                                        │
│                                                                                                              │
└──────────────── shell: confined · 12 more it will ask about · space cycles · a allow · n never · r ask again ┘
```

Rows are **decisions**, not defaults. `ask` is what this policy does about anything nobody has
mentioned, so a row per undecided thing would be a screenful of "it will stop and ask" burying the
one or two lines that say what this agent can do *without* stopping. What is not listed is counted
instead — `12 more it will ask about` — because a screen showing five decisions while standing for
seventeen answers would be a different kind of dishonest. A subject arrives here when somebody
answers a question about it, and cycling one back to `ask` takes it off again, which is what taking
a decision back looks like.

What that costs is worth saying plainly: you cannot refuse something here that has never come up.
Deciding in advance means answering the first question with <kbd>a</kbd> or <kbd>n</kbd>.

The line along the bottom comes first because it is the one thing on this tab that is not
negotiable. A registered `shell` that is not refused can read, write and reach the network whatever
the other rows say — so `shell: confined` (or `shell: a command can do any of these`) is what makes
the rest of the table mean anything.

Two kinds of row. A **capability** is what a tool declares, which is what makes "always" work for
tools this program has never heard of — including an MCP server's, which all carry `mcp:<name>`. A
**path rule** is finer than any capability: `read: allow` is a reasonable thing to want and
`read .env: allow` is not, and the difference is a property of the file rather than of the tool
that opened it. The strictest of everything consulted wins, so a rule can only tighten what a
capability allows. `network` is the odd one: no tool declares it, because a model that wants the
network writes `curl` — so the row says which shell it reaches, and when.

<kbd>space</kbd> cycles a row through **ask → allow → deny**, or <kbd>a</kbd>/<kbd>n</kbd>/<kbd>r</kbd>
directly, and it takes effect on the next call. Answering "always" at a permission prompt writes to
this same table — the prompt and the tab are one object, not two.

`allow` runs with no question. `deny` never runs and never asks: the model gets `the call was not
permitted` as a tool result it can read and work around, rather than a call that silently vanished.
When the policy refuses on its own, the transcript says which stance did it — ``shell: refused by
`network`, which this command reaches for`` — because "the call was not permitted" beside a
`shell: allow` is true and useless.

### the question itself

```text
┌ a tool wants to run ─────────────────────────────────────────────────┐
│ shell wants: shell, network                                          │
│                                                                      │
│ cmd: curl -s https://example.com                                     │
│                                                                      │
│ [y] once   [a] always, for shell and network   [n] no                │
│ [i] the exact JSON   [d] drop it                                     │
└──────────────────────────────────────────────────────────────────────┘
```

It names **everything the policy consulted**, not just what the tool declared, and <kbd>a</kbd>
answers for all of it — including any calls already waiting behind this one, since a model that
asks for three things at once produces three questions and an "always" that did not reach them
would go back on itself one keystroke later. <kbd>y</kbd> is this call only; a `curl` allowed once
runs with the network open for that command and no other.

Typing does not answer it. A question arrives on its own schedule, in the middle of whatever you
happen to be typing, and its keys are ordinary letters — `a` grants a capability for the rest of
the session and is also the third letter of "what". So a question waits for a pause in the typing
before it starts taking keys as answers, and until then your letters go where you aimed them: into
the prompt. <kbd>enter</kbd> sends what is in the prompt, and it waits, exactly like a message sent
into a running turn does.

## 🐢 one transition at a time

The loop is a state machine, and `/step` performs exactly one transition of it instead of a whole
turn. That is the only way to stand in `ready` — which the runtime documents as *a resting state
on purpose*, the moment the model has said what it wants and **nothing has happened yet**:

```text
> /step how many lines are in ledger.py?

⟩ shell({"cmd":"wc -l ledger.py"})

· step → ready: 1 call(s) decided, none of them run yet
      shell {"cmd":"wc -l ledger.py"}
```

The command is decided, permitted, and not running. From here you can read it, prune the context
it would have run against, drop it, or `/step` again to run it. A whole turn walks through this
state without ever drawing it, which is why every other agent's "approve this command?" is the
only checkpoint it has. Here the checkpoint is the state machine's own.

`/step` again for each transition — the tool runs, then the next request goes — or `/continue` for
the rest of the turn. While stepping, answering a permission does *not* quietly resume: you asked
to drive.

## 🔧 what it comes with

Four tools — `read`, `write`, `edit`, `shell` — and a policy that allows reading and asks about
everything else, including the network. Answering **always** answers for a *capability*, not a
tool name, which is what makes it work for tools this program has never heard of:

```console
$ kamchatka --mcp 'files=npx -y @modelcontextprotocol/server-filesystem /srv'
```

Those arrive through [`nachalnik-mcp`](../nachalnik-mcp) carrying `mcp:files`, so "always, for
mcp:files" is one server and not the next one. The `name=` is worth giving: it prefixes the
server's tools and it is what the grant is *for*, and without it the name comes from the program,
which for most of the servers people actually run is `npx`.

The registry is live rather than fixed at startup: `/tools drop shell` stops offering it from the
next request onward, which is one call on the kernel and no restart. When a model has gone down
the wrong path entirely, <kbd>d</kbd> at the permission prompt drops *every* call it is waiting on
with one reason — and the model is told, rather than left waiting on calls that silently vanished.

## 📏 the number in the status line is a guess, and says so

Nothing here has the model's tokenizer, so the figure the status line leads with is an estimate —
it is written `~2,460` for that reason. The percentage beside it names the total it is a
percentage of (`0.9% (128k)`), because a fraction of an unstated number is not something anybody
can act on, and it turns yellow past 70% and red past 90%. Then comes what the provider actually
charged for the last request, and `/budget` is where the two are reconciled:

```text
the next request: ~20,125 tokens, 19,953 of context and 172 of tool definitions

the limit: 1,048,576, which the next request would fill 1.9% of

the last request really cost 20,063, as the provider counted it

the counter has learned from 2 request(s) and scaled itself by 1.131: its own guesses
came to 35,447 tokens where the provider counted 40,073, so it was reading 13.1% low
```

That correction is the runtime's `Calibrating` counter: every response tells it what the request
it just estimated really cost, and it adjusts. Over a real session against Gemini it went from 13%
low to within 0.3%. A budget nobody can check is a decoration.

The compactor drops the oldest tool results once the context passes `--compact` (0.8 by default)
of the limit and leaves a note saying which ones went. It does not summarize them — it never read
them — and it removes nothing that is pinned, because the kernel refuses. Every removal is an
excluded item you can put back.

## 🎛️ options

```text
kamchatka [OPTIONS] [MESSAGE]...

  -m, --model <MODEL>       the model to talk to            [env: KAMCHATKA_MODEL]
  -f, --file <PATH>         put a file in the context, pinned; may be repeated
  -s, --system <TEXT>       a system instruction; the runtime ships none of its own
  -r, --resume <PATH>       carry on from a session written by /save
      --mcp <COMMAND>       an MCP server to run, as `[name=]command`; may be repeated
      --requests <N>        how many requests one turn may make            [default: 8]
      --compact <FRACTION>  how full the context may get; 1 never compacts [default: 0.8]
      --parallel            run the model's tool calls at the same time
      --sandbox-allow <PATH> a path outside the working directory the shell may also
                            read and write; may be repeated
      --no-sandbox          run the shell tool unconfined, reaching whatever you can
```

```text
KAMCHATKA_API_KEY       or OPENROUTER_API_KEY, or OPENAI_API_KEY
KAMCHATKA_BASE_URL      e.g. http://localhost:11434/v1 for ollama; OpenRouter by default
KAMCHATKA_CONTEXT_LIMIT the model's context size, for a provider that will not say
```

`/save` writes two files: a `.jsonl` of every event that happened, and a `.json` snapshot that
`-r` picks the session back up from.

## 🧪 the tests

They draw the screen and read it back, against a scripted model:

```rust
harness.tab(Tab::Context);               // over to the context
harness.press(KeyCode::Home).await;      // the first item
harness.press(KeyCode::Char(' ')).await; // out

let after = harness.app.kernel.preview_request().unwrap();
assert!(!format!("{:?}", after.messages).contains("hunter2"));
```

A terminal program whose tests only checked its own state would be testing the half nobody looks
at.

## 🎸 the name

`nachalnik` is an homage to KINO's *Nachalnik Kamchatki*. Kamchatka was the boiler room Viktor
Tsoi shovelled coal in; this is the one where the work actually happens.

## licence

MIT.
