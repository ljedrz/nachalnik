# kamchatka

[![crates.io](https://img.shields.io/crates/v/kamchatka.svg)](https://crates.io/crates/kamchatka)
[![docs.rs](https://docs.rs/kamchatka/badge.svg)](https://docs.rs/kamchatka)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**A terminal agent that shows you its context.**

Built on [`nachalnik`][nachalnik], and built to demonstrate it. Everything in here is
ordinary user code — the provider, the four tools, the permission policy, the compactor, the
drawing. The runtime supplies the state machine, the context and the paper trail.

```console
$ cargo install kamchatka
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
 done · gpt-4o-mini @ openrouter.ai · ~1,168 tokens, 0.9% (128k) · 1,102 really · 15 held back · F1 for the keys
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
> [what it does and does not protect you from][protection].

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
│  6 … shell                      tool_result          9,004  compaction: compacted to make room               │
│  7 · assistant                  assistant_message       62  The kernel is a state machine with five states. …│
│                                                                                                              │
└────────────────────────────────────────────────────────────────────────────── 7 items, 2 not going, 1 elided ┘
```

That is not a summary and not a debug view. It is the list of items the runtime is holding, in
order, with what each one costs, whether it is going into the next request — and the column that
matters most, what the model will actually read of it. Item 4 is marked `-` and says on its own
row why it is out, in the projector's words. Item 1 is `▪`, pinned, so the compactor will be
refused if it comes for it. Nothing disappeared: things changed state, and the state is on screen.

Item 6 is `…`, **elided**, which is the third answer between in and out. It is still in the
request — as the one line the row shows, saying it was compacted away — so the call on row 5 still
has an answer, and the model reads a conversation in which it asked for something and can no
longer see what came back. That is the truth. Dropping the result outright would have forced the
projector to drop the call with it, since a call with no result is a request most providers
reject, and the model would then be reading a conversation in which it never asked at all. What
an elided item holds is counted as held back rather than spent, and <kbd>space</kbd> spends it
again.

<kbd>tab</kbd> moves the keys between the prompt and the table:

| key | what happens |
| --- | --- |
| <kbd>space</kbd> | cycle how much of it the model gets: all of it → a `…` marker → nothing → back |
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

Every transition of the state machine is in there, and so is everything either side of it: what was
requested, what was decided, what was added to the context and what it cost — down to the wiring,
since plugging in a provider, a policy and each tool is itself an event and the screen subscribes
before any of it happens. There is a test that the tab draws every event the session recorded.

It is the same stream `/save` writes to a `.jsonl`, and reading it is how you find out that a
permission question became a decision became a state change became a call. <kbd>tab</kbd> then
<kbd>up</kbd> reads back through it.

The one thing it does not draw a line per is a *fragment*: the model's streamed text and a running
command's output arrive dozens of times a second, and a line each would push the rest of the trace
off the screen before it could be read — a `cat` of a thousand lines really did erase the whole of
it. Tool output is one line that counts up (`tool.output  shell, 12,004 bytes so far`) and the
model's text is on the chat tab as it arrives. The pane keeps the last few hundred lines; `/save`
keeps every event there was, including the one line no subscriber can ever catch — the kernel's own
`session.started`, emitted while it is being constructed.

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

A fresh session has no rows at all: everything starts at `ask`, and the tab fills up as you answer.
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

Four tools — `read`, `write`, `edit`, `shell` — and a policy that asks about all of it. Nothing is
allowed on your behalf before you have been asked, `read` included. Answering **always** answers for a *capability*, not a
tool name, which is what makes it work for tools this program has never heard of:

```console
$ kamchatka --mcp 'files=npx -y @modelcontextprotocol/server-filesystem /srv'
```

Those arrive through [`nachalnik-mcp`][nachalnik-mcp] carrying `mcp:files`, so "always, for
mcp:files" is one server and not the next one. The `name=` is worth giving: it prefixes the
server's tools and it is what the grant is *for*, and without it the name comes from the program,
which for most of the servers people actually run is `npx`.

The registry is live rather than fixed at startup: `/tools drop shell` stops offering it from the
next request onward, which is one call on the kernel and no restart. When a model has gone down
the wrong path entirely, <kbd>d</kbd> at the permission prompt drops *every* call it is waiting on
with one reason — and the model is told, rather than left waiting on calls that silently vanished.

## 🔎 letting the agent read and manage its own context

`--introspect`, or `/introspect` at any point, offers two more tools. They are off by default,
because a model that can rewrite its own context is a decision rather than a default.

**`introspect`** reads. `look` lists every item it is carrying — what each one is, what it costs,
whether it is going into the next request and why not if it is not — and reads any of them in
full, block by block, including what it was thinking when it produced them. `request` shows the
request about to go out, message by message, with what the projector left out and what it had to
repair.

`budget` is the one a decision gets made from:

```text
the next request is ~48,120 tokens of 128,000 (38% full, ~79,880 left)
  47,343 in the context, 777 in the tool definitions
~9,004 tokens are being held back - excluded, archived, or elided to a marker
the last request really cost 52,905 in / 214 out, as the provider counted it
the estimate is corrected by x1.09, learned from 6 request(s)

the 4 most expensive item(s) actually going into it:
  id  state       kind                  tokens  if all go  what it is
  31  active      tool_result           18,204    18,204  shell: cargo test --workspace…
  14  active      reference              9,880    28,084  src/kernel.rs: use std::…
   9  pinned      system                   240    28,324  system: You are working in… · not yours
```

Estimates are named as estimates, the provider's own figure sits beside them, and the list is
what the request *actually* carries — an orphaned tool result the projector repairs away costs
nothing however active it looks, and offering it as something to give up would be advice that
buys nothing. Items that are not the agent's to move say so, rather than costing it a refused
call.

`draft` and `fork` take a snapshot of the context, resume it as a second kernel with **no tools**,
ask it once, and hand back only what it said. `draft` is for reading your own answer before you
give it; `fork` is for asking whether a piece of context is what is leading you astray:

```text
⟩ introspect({"action":"fork","question":"am I overfitting to the first stack trace?",
              "without":[14,15]})

  a copy of you, asked `am I overfitting to the first stack trace?`, on 9 of your items,
  without 14, 15. None of this is in your context and nobody has read it; it is yours to
  use or drop.
```

A fork can think; it cannot act, and it cannot go on thinking after it has answered once. Nothing
it does reaches this session's context or its log. Forking needed no change to the runtime at all
— `Kernel::snapshot` and `Kernel::resume` already *are* that, and leaving an item out is one field
on a copy of the snapshot.

**`amend`** manages. `prune` moves items between the same states the <kbd>space</kbd> key does —
`elide` for a tool result that has served its purpose, which keeps the call answered and stops it
costing what it holds; `exclude` to take one out altogether; `pin` to protect one from compaction
— named by `ids` or by `select`, which takes the same selector language `/prune` does, so "the
tool results I am done with" is one call rather than twelve numbers read off a listing. `revise`
rewrites what an item says. `note` writes something into the context — a plan, a conclusion, a
thing not to try again — attributed to `agent` so the context pane can say who put it there, and
pinnable so that compaction cannot take it. Saying the same thing out loud in a turn is not a
promise about anything; a pin is.

`undo` walks back — deliberately *not* the kernel's undo stack. That stack is yours, bound to
<kbd>u</kbd>, and the top of it while a tool is running is always the assistant turn that asked
for the call: one step would erase the model's own question and orphan the answer it is waiting
for. So `amend` keeps a journal of what *it* did, and that is what it walks. A reason is required
on every change, and it is what you read in the context pane.

Three things are refused outright, with the refusal handed back to the model: a **pinned** item
(a pin is a promise, and it was not made to the model), a **system instruction**, and the
assistant turn it is currently speaking in. It may unpin what it pinned itself, and nothing else.

They are two tools rather than one with a mode argument, because a tool declares its capabilities
once for every call it will ever receive. One tool would mean that answering **always** to "may it
read its own context?" also answered "may it rewrite a tool result?" — a grant that delivers more
than it implies, which is the shape of thing this program exists not to do. So the permissions tab
has a row for `introspect` and a row for `amend`, and you can answer them differently.

## 🧩 two dialects, and why one of them keeps the order

`--gemini` talks to Google's own API instead of an OpenAI-compatible one. That is not a
convenience — it is the difference between seeing what the model did and seeing a rearrangement of
it.

`generateContent` answers with `content.parts[]`: a thinking part, a sentence, a `functionCall`,
in the order they were produced. The OpenAI-compatible shim in front of the same model flattens
that into a `content` string beside a `tool_calls` array, because the dialect it is imitating has
nowhere to put an order. Everything downstream then reads a turn that has been tidied up.

```text
▸ [4] assistant · assistant_message · from model · active · 178 tokens
    --- 3 block(s), in order ---
    [0] reasoning: The user wants the code word, and the tool is the only place it is…
    [1] text: Let me look that up.
    [2] call (signed): secret({})
```

That is the context pane, and `introspect` reads the same thing back to the agent. It is only worth
having because the order is really in there: the runtime records it as `Content::Blocks`, counts
it, prunes it and elides it like any other content, and `LinearProjector::send_blocks` sends it
back the same way.

Signatures are the other half. Gemini signs the parts of a turn and answers
`400 Function call is missing a thought_signature` to a request that returns one without it — and
it signs text parts as well as calls, which a message with three slots has nowhere to keep. Here
each part's own fields ride back out on the block they arrived on, unread.

Both providers answer one trait, `Endpoint`, so `/model`, `/models`, `/provider` and the status
line work the same against either and nothing above them knows which wire format it got.

```console
$ export KAMCHATKA_API_KEY=...        # a Google AI Studio key
$ kamchatka --gemini --introspect "what does src/kernel.rs do?"
```

## 🔀 the model, and the address it lives at

`/model` says which model this is talking to, where that is, in what dialect, and how much context
it has:

```text
· gemini-3.6-flash at https://generativelanguage.googleapis.com/v1beta/openai
  (openai-compatible), 1,048,576 tokens of context
```

`/model ID` switches the model and `/provider URL [ID]` switches the address — and the model with
it, because a model belongs to the address that serves it. Switching one and keeping the other is
how a session ends up asking the ollama on this machine for `gemini-3.6-flash`; given no model the
old name is kept and the new endpoint is asked whether it has one by that name, which is a notice
now rather than a 404 on the next request. Both matter for different reasons: comparing two hosted
models is one address and two names, while comparing a hosted model with the one running on this
machine is two addresses. A comparison that cannot see the address is a comparison of names.

`/models [FILTER]` is what makes `/model` usable, because the ids belong to the endpoint rather
than to the model: the same thing is `google/gemini-3.5-flash` at one address and
`gemini-3.5-flash` at another, and after a `/provider` there was no way to find out which without
guessing. It asks the endpoint, marks the one you are on with `▸`, and takes a filter because a
list of fifty-four is not an answer:

```text
┌  6 of 54 matching `flash-lite` · /model ID switches  ────────────────┐
│   gemini-flash-lite-latest                                           │
│   gemini-2.5-flash-lite                                              │
│   gemini-3.1-flash-lite-preview                                      │
│   gemini-3.1-flash-lite                                              │
│   gemini-3.1-flash-lite-image                                        │
│ ▸ gemini-3.5-flash-lite                                              │
└ 1–6 of 6 · any key closes ───────────────────────────────────────────┘
```

The key is *not* switched with the address. It is read from the environment once, at startup, and a
key typed at a prompt would be a key in the transcript — so `/provider` is for the addresses that
need no key or take the same one: a local model, a proxy, another base URL on the same account.

What is not switched either way is the context. The same items go to whatever answers next, which
is the whole of what makes the answers comparable. `/seams` names the six replaceable parts and
what is in each of them right now, asked of the kernel rather than restated from what this program
set up at startup.

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

The compactor shortens the oldest tool results to a marker once the context passes `--compact`
(0.8 by default) of the limit. It does not summarize them — it never read them — and it touches
nothing that is pinned, because the kernel refuses. Every one of them is still on the context tab,
marked `…`, still holding every byte it held, one <kbd>space</kbd> from coming back.

## 📦 installing

```console
$ cargo install kamchatka                     # from the registry
$ cargo install --git https://github.com/ljedrz/nachalnik kamchatka
$ cargo install --path kamchatka              # from a clone
```

Rust 1.88 or newer, and that is the whole list: no system libraries, no `pkg-config`, nothing
to install first. The TLS is `rustls` over `ring`, which builds its own cryptography rather than
looking for yours. Adding `--no-default-features` drops MCP support, and the `--mcp` flag with it.

**The sandbox is Linux-only.** The `shell` tool is confined with [Landlock](https://landlock.io),
which is a Linux LSM; everywhere else the program builds and runs, but the shell is unconfined,
the permissions tab says `shell: a command can do any of these` rather than `shell: confined`,
and the stances are answers you were asked for rather than a boundary anything enforces. On Linux
it also wants a kernel new enough to have Landlock — 5.13 for the filesystem rules, 6.7 for
`network: deny` — and the tab says which of the two it got.

## 🎛️ options

```text
kamchatka [OPTIONS] [MESSAGE]...

  -m, --model <MODEL>       the model to talk to            [env: KAMCHATKA_MODEL]
                            [default: openai/gpt-4o-mini, or gemini-3.6-flash
                            with --gemini]
  -f, --file <PATH>         put a file in the context, pinned; may be repeated
  -s, --system <TEXT>       a system instruction; the runtime ships none of its own
  -r, --resume <PATH>       carry on from a session written by /save
      --mcp <COMMAND>       an MCP server to run, as `[name=]command`; may be repeated
      --requests <N>        how many requests one turn may make            [default: 8]
      --compact <FRACTION>  how full the context may get; 1 never compacts [default: 0.8]
      --parallel            run the model's tool calls at the same time
      --gemini              talk to Google's own API rather than an OpenAI-compatible
                            one, so a turn keeps the order it was produced in
      --introspect          offer the model the two tools that read and manage its own
                            context; /introspect turns them on and off while it runs
      --sandbox-allow <PATH> a path outside the working directory the shell may also
                            read and write; may be repeated
      --no-sandbox          run the shell tool unconfined, reaching whatever you can

Environment:
  KAMCHATKA_API_KEY        the key; or OPENROUTER_API_KEY, or OPENAI_API_KEY
  KAMCHATKA_BASE_URL       where the requests go, e.g. http://localhost:11434/v1 for
                           ollama; OpenRouter by default, or Google's own v1beta
                           with --gemini
  KAMCHATKA_CONTEXT_LIMIT  the model's context size, for a provider that will not say
```

That is `--help`, which lists the environment too rather than leaving three settings for the
readme alone to mention.

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

<!-- crates.io resolves a relative link against the directory this readme was published from,
     which is not where the repository root is. Links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[nachalnik-mcp]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-mcp
[protection]: https://github.com/ljedrz/nachalnik/blob/HEAD/README.md#-what-it-does-and-does-not-protect-you-from
