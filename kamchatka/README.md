# kamchatka

[![crates.io](https://img.shields.io/crates/v/kamchatka.svg)](https://crates.io/crates/kamchatka)
[![docs.rs](https://docs.rs/kamchatka/badge.svg)](https://docs.rs/kamchatka)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**A terminal agent that shows you its context.**

Built on [`nachalnik`][nachalnik], and built to demonstrate it. Everything in here is
ordinary user code — the tools, the permission policy, the compactor, the drawing, and the two
providers next door in [`nachalnik-providers`][providers]. The runtime supplies the state machine,
the context and the paper trail.

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
> The `shell` tool runs under [Landlock](https://landlock.io): `network: deny` is a TCP `connect()`
> refused by the kernel — TCP being the whole of what Landlock can refuse, so a UDP datagram still
> goes out — and `write: deny` makes the working directory read-only. Nothing outside
> that directory is **writable**, and nothing outside it is readable either — with one deliberate
> exception, which is that the system directories (`/usr`, `/etc`, `/bin`, `/lib`, `/proc` and the
> rest) are readable, because a command that cannot read `/usr/bin` cannot be a command. So
> `cat /etc/passwd` works and `cat ~/.ssh/id_rsa` does not. The three file tools are held to the
> same boundary by their own code, and to a tighter one: they refuse `/etc/passwd` too. They also
> never expand `~` — there is no shell in front of them, so a path is taken at its word, and they
> say that in a sentence rather than reporting the file as missing.
> The permissions tab says which you have — `shell: confined`, or
> `shell: a command can do any of these` where Landlock is not available. `--sandbox-allow PATH`
> opens up more, `--sandbox-read PATH` opens it up for reading only, and `--no-sandbox` turns it
> off. Within the boundary the file tools are finer than
> a capability: `read` is allowed, and `read .env` is a question, because a path rule can tighten
> what a capability allows. It is one LSM, not a container; see
> [what it does and does not protect you from][protection].

## 👉 four tabs, one window

<kbd>ctrl+t</kbd> for the next one, or <kbd>alt+1</kbd> … <kbd>alt+4</kbd> directly. The status line
is under all of them, so the budget is always in view. The prompt is not: it belongs to the
conversation, and the other three tabs are read and operated rather than typed into. <kbd>tab</kbd>
is the way back to it from any of them.

```text
 asking ••• 42s · gemini-3.6-flash @ generativelanguage… · ~13,204 tokens, 10.3% (128k)
```

While the runtime is working, three dots move under the word for what it is doing, and after five
seconds they are joined by how long it has been. Which dot is lit comes from the clock rather than
from a frame counter, so it stops where it is if the screen stops being drawn — `asking` on its
own is the same word whether a request is in flight or the program is wedged, and the marker is
there to tell the two apart. It is absent while the runtime is resting, including when it is
waiting on **you**: nothing should suggest work is happening while a question sits unanswered.

**chat** is the conversation, and every terminal agent has one — this one also says which of it
the model is still being sent, and reads an edit where the turn it replaced was. Both of those are
read off the context every frame rather than written down when they happen, so a <kbd>u</kbd> that
takes an edit back takes it off here too. **context** is why this exists:

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│  id  label         kind               sending   held  what it says, or why it is not being sent            │
│  1 ▪ src/kernel.rs reference            1,045         pub struct Kernel;                                    │
│  2 ▪ q3.pdf        reference                6+        [application/pdf, 292.47kB]                           │
│  3 · user          user_message             6         what does the kernel do?                              │
│  4 · assistant     assistant_message        7         asked for read                                        │
│  5 - read          tool_result              0     15  excluded: at the terminal, by `tool:read`             │
│  6 · assistant     assistant_message        7         asked for shell                                       │
│  7 … shell         tool_result             11  9,004  compaction: compacted to make room                    │
│  8 · assistant     assistant_message       62         The kernel is a state machine with five states. …     │
│                                                                                                              │
└────────────────────────────────────────────────────────────────────────────── 8 items, 2 not going, 1 elided ┘
```

That is not a summary and not a debug view. It is the list of items the runtime is holding, in
order, with what each one costs, whether it is going into the next request — and the column that
matters most, what the model will actually read of it.

Row 2 is the other thing this column is for. `6+` is not six tokens: the `+` says the counter
would not put a figure on part of that item, so six is a **floor**. Nothing here has a
tokenizer for a PDF, and a bare `0` would have made the largest thing in the request read as the
cheapest row in the pane you opened to decide what to delete.

**sending** is what an item puts into the next request; **held** is what it is keeping out of one.
For most rows the first is everything and the second is blank. The two that differ are the ones
worth finding: item 6 holds nine thousand tokens and spends eleven of them on the marker that
replaced it, and item 4 spends nothing at all. The `sending` column adds up to the figure on the
status line, and the `held` column to the `held back` beside it — which they did not, when the row
reported what an item held under a heading that said what it cost. Item 4 is marked `-` and says on its own
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
| <kbd>f</kbd> | list only what the next request carries, or everything again |
| <kbd>/</kbd> | filter the rows: fuzzy, over the whole of what an item holds — see below |
| <kbd>enter</kbd> | read the whole of it — see below |
| <kbd>←</kbd> / <kbd>→</kbd> | move between its pages, while it is open |
| <kbd>u</kbd> / <kbd>U</kbd> | undo / redo the last change to the context |
| <kbd>23G</kbd> | go to the item numbered 23 — the number `/exclude` takes |

An oversized tool result is held as *two* items: the truncated copy the model was shown, and the
whole of it beside it, marked `▫ archived` and not going. <kbd>space</kbd> or <kbd>p</kbd> on that
row is how you say **send the whole thing** — it is the only way to say it, and the token count in
the row is what it will cost you.

<kbd>enter</kbd> opens the item, and an item has more than one honest answer to *what is this?*
The box is paged, and <kbd>←</kbd> / <kbd>→</kbd> move between the pages:

```text
┌ [6] shell · tool_result · elided · 1,204 tokens ──────────────────────────┐
│  to the model │ as stored │ v2 │ v1                                       │
│                                                                           │
│ role: tool                                                                │
│ answers: shell                                                            │
│                                                                           │
│ [... compacted away ...]                                                  │
└ ← → the pages · 1–6 of 6 · any key closes ────────────────────────────────┘
```

**`to the model`** is what this item puts into the next request, taken from the projection of the
whole context rather than of the item alone — so a call the projector had to drop, or an ordered
turn it had to flatten, shows up here as the repair it really is. An item that is not going says
so, with the reason. That last one is not always visible from the row: a tool result whose call
has been taken out reads as `· active` and costs what it costs, and the projector drops it anyway,
because a result answering nothing is a request most providers reject. Its page says
`nothing: this item is not in the request`, and the box opens on that page rather than on the
text, since the projection rather than the state decides which of the two is the surprise. **`as stored`** is what the item itself holds, which for an elided or
excluded one is not the same thing at all; that gap is the reason the pages exist, and it is why
an item the model does not read in full opens on the first page instead of the second.

**`v1`**, **`v2`** and so on are what it said before somebody rewrote it, newest first. A terminal
edit supersedes, so the old text keeps a row of its own — but `amend` **replaces in place**, to
keep the number the model refers to the item by, and the old text then exists nowhere except the
`context.replaced` event. This reads it back off there, up to eight versions deep.

<kbd>e</kbd> is the verb the others were missing. `space` and `p` decide whether the model reads an
item; `e` decides **what** it reads. The prompt turns into an editor holding the item's text, and
committing supersedes the old one rather than overwriting it:

```text
  1 ~ ledger.py    reference    469  superseded: replaced by item 8
  8 ▪ ledger.py    reference    477  """A running-balance ledger.
```

The original is still there, still readable, still one <kbd>u</kbd> from coming back — on both
screens, since the conversation reads the edit out of the context rather than keeping its own copy
— and the next request carries only the edit, because a superseded item is not projected. Trimming
a 2,000-line file down to the function that matters is two keystrokes and a delete.

**trace** is every event the runtime emits, as it happens, in the same names the session log is
made of:

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│── 2026-09-12                                                                                                 │
│14:22:07         model.requested       6 messages, 4 tools, ~1579 tokens                                      │
│14:22:13   +6.4s context.added         [7] assistant, 72 tokens                                               │
│14:22:13         model.finished        EndTurn, 1522 in / 19 out (reported)                                   │
│14:22:13         tool.requested        shell                                                                  │
│14:22:13         permission.requested  shell (3)                                                              │
│14:22:13         state.changed         requesting → deciding                                                  │
│14:22:13         context.recounted     1425 → 1377 tokens                                                     │
│14:22:24  +11.0s permission.decided    shell: allow, answered when it was asked about                         │
│14:22:24         state.changed         deciding → ready                                                       │
│14:22:24         state.changed         ready → executing                                                      │
│14:22:24         tool.started          shell                                                                  │
│14:22:25   +1.3s tool.output           shell, 12 bytes                                                        │
│14:22:25         context.added         [8] shell, 23 tokens                                                   │
│14:22:25         tool.finished         shell, 23 tokens                                                       │
└──────────────────────────────────────────────────────────────────── 41 events · /save keeps them all ────────┘
```

Every transition of the state machine is in there, and so is everything either side of it: what
was requested, what was decided, what was added to the context and what it cost. That goes down to
the wiring — plugging in a provider, a policy and each tool is itself an event, and the screen
subscribes before any of it happens.

Every event says what it carries, rather than just naming itself. `context.replaced` shows the
first line of what an item used to say, which is the one thing nothing else can recover.
`tool.repaired` names the identifier a provider reused, and what was wrong with it.
`tools.changed` lists the tools rather than counting them. There is a test that the tab draws every event the session recorded, and another
that none of them is a name with an empty line beside it.

**Two clocks, because neither answers the other's question.** The second column is the gap since
the line above, blank under a tenth of a second. Nearly everything in a session happens between one
frame and the next, so what is left with a number beside it is the interesting part: the model
thinking, a command running, and the eleven seconds somebody spent deciding whether to allow a
shell. That is the question people bring to a log — *which step was slow* — and here it is answered
without subtracting a column of timestamps.

The first column is when it happened, which no amount of adding up deltas will give you. Matching
the pane against a server log, a provider's dashboard, a ticket, or a memory of what happened
before lunch all need an absolute time, and so does a search: *the hour it broke* is a query,
*seven hundred milliseconds after the line above* is not. The date is a rule across the pane rather
than a column, drawn only where it changes — a session can outlast a day, and `00:15` against two
different Tuesdays says nothing — and it carries the zone, or says `UTC` where the local one could
not be determined. Both columns give way before the event names do: a narrow window spends its
space on what happened rather than on when.

It is the same stream `/save` writes to a `.jsonl`, and reading it is how you find out that a
permission question became a decision became a state change became a call. <kbd>up</kbd> reads
back through it — the keys are already on the tab, because there is no prompt on it to share them
with.

The one thing it does not draw a line per is a *fragment*. The model's streamed text and a running
command's output arrive dozens of times a second, and a line each would push the rest of the trace
off the screen before anybody could read it — a `cat` of a thousand lines really did erase the
whole of it.

So tool output is one line that counts up (`tool.output  shell, 12,004 bytes so far`), and the
model's text is on the chat tab as it arrives. The pane keeps the last few hundred lines. `/save`
keeps every event there was, including the one line no subscriber can ever catch: the kernel's own
`session.started`, emitted while it is still being constructed.

**<kbd>/</kbd> filters either of those two panes.** Eight hundred events is not a log anybody
reads; it is a log somebody scrolls past looking for one line, and the way to find it used to be
<kbd>g</kbd> and a lot of <kbd>pgdn</kbd>. <kbd>/</kbd> opens a one-row box where the prompt would
be — these panes are read and operated rather than typed into, so it is not taking anything — and
what you type filters the rows, fuzzily, counting what it found beside the query. It is
[`nucleo-matcher`](https://crates.io/crates/nucleo-matcher), which is Helix's, because that is
where the expectation of what fuzzy *feels* like comes from: `mreq` finds `model.requested`.

A context row matches on the whole of what the item holds rather than the one line the row has
space for — the filename you are looking for is almost never on the first line — and a trace row
matches on the name, the detail and the clock, so an hour or a date finds what happened in it. The
arrows and the paging stay with the pane, deliberately: the point of filtering eight hundred events
down to nine is to read the nine, and a box that swallowed the scroll keys would mean closing the
search, and so losing the filter, to look at what it found. <kbd>esc</kbd> closes it, and closing
clears it — a filter that outlived its box would leave a window quietly showing four rows of eight
hundred with nothing on screen saying why. Changing tabs clears it for the same reason.

And from anywhere, <kbd>ctrl+p</kbd> prints the request those items add up to — the kernel's own
rendering of it, not a description, with a header naming everything the projector left out and
why:

```text
12 item(s) in, 4 out:
  [13] left out: an assistant turn with no content and no answered calls
  [14] left out: archived: the whole output; the model was shown a truncated copy
  [15] left out: excluded: at the terminal, by `tool:shell:latest`
  repaired: dropped the call `call_301842` (shell) from item 13: its result is not
            in the projection
```

"why is that not in there?" is the question this whole runtime is for, and the JSON on its own can
only answer the other one. `/payload` goes one further and prints what the provider will put on
the wire, byte for byte.

## ✍️ what the model writes

Models answer in markdown, so the chat tab reads it as markdown. Headings and emphasis get weight
and colour, inline code is told apart from prose, and list continuations hang under their bullets.
A fenced block gets a rule down its left rather than a slab of background — a slab being the one
thing a terminal cannot draw without knowing what colour the theme is.

Tables are drawn here rather than by the renderer, and for the same reason as the fenced blocks:
the renderer lays one out at whatever width its contents want, and a row wider than the window was
then wrapped like a sentence — so half a border arrived on the next line and the table came apart.
A table is a fixed shape. What gives when it does not fit is the columns, widest first, and the
cells wrap inside them with their inline styling intact; the colons in the delimiter row decide
which end of its column a cell sits at.

The parsing is [`tui-markdown`](https://crates.io/crates/tui-markdown); only the styling is this
crate's. Nothing else on the screen is treated as markdown: a tool's output is what the tool said,
and running that through a renderer would be inventing structure it never had.

## ⌨️ the rest of the keys

| key | what happens |
| --- | --- |
| <kbd>enter</kbd> / <kbd>alt+enter</kbd> | send / a new line |
| <kbd>pgup</kbd> / <kbd>pgdn</kbd> | scroll the conversation |
| <kbd>ctrl+home</kbd> / <kbd>ctrl+end</kbd> | the beginning of the conversation / the end of it |
| <kbd>home</kbd> / <kbd>end</kbd> | the prompt's own, as in any other line editor |
| <kbd>ctrl+e</kbd> | follow the newest again |
| <kbd>tab</kbd> | move between the prompt and the open tab |
| <kbd>ctrl+t</kbd> | the next tab; <kbd>alt+1</kbd> … <kbd>alt+4</kbd> for one in particular |
| <kbd>esc</kbd> | close an open search box; otherwise stop what is running, and keep what arrived |
| <kbd>ctrl+c</kbd> | stop what is running either way, and again to leave |
| <kbd>F1</kbd> | the keys, opened at the tab you are on; also <kbd>?</kbd> on any tab but the chat one |

**<kbd>F1</kbd> answers for where you are standing.** The panel is one page per tab, plus one for
the slash commands and one for the keys that mean the same thing everywhere, and it opens at the
page for the tab it was pressed from — so asking what the keys are on the trace answers with the
trace's six, not with a hundred lines of which four fifths are about somewhere else.
<kbd>←</kbd> and <kbd>→</kbd> turn the pages, and the strip along the top names all of them: the
rest is one key away rather than hidden. A seventh appears while a tool is waiting to run, and is
the one it opens at, because that is what you are being stopped by.

Where you leave the conversation is where it stays. A turn that writes four hundred lines used to
pull the window down to the newest of them on every fragment, so anything it had said thirty
seconds earlier was unreadable until the turn ended; now nothing but your own message moves it,
and the line along the bottom says how much has arrived underneath. <kbd>ctrl+e</kbd> goes back to
following, and so does scrolling down to the end. <kbd>ctrl+home</kbd> and <kbd>ctrl+end</kbd> are
the two ends of it in one key; control is held because <kbd>home</kbd> and <kbd>end</kbd> belong to
the prompt, and a line editor whose <kbd>home</kbd> moved something else
would be a trap.

**Nothing said is shortened to fit.** However long an answer is, the whole of it is on the chat tab
and can be scrolled back through. A tool's output while it is still running is the one exception,
and only while it is running: a command that produces megabytes is bounded on screen, the whole of
it goes into the context, and the finished result is shown as its first few lines with the rest one
keystroke away on the context tab.

Stopping is cooperative rather than a killed process. The provider notices between fragments and
returns the text it has; the shell tool kills the command — and everything the command started,
since it runs in a process group of its own — and still answers the call it was given. The partial
turn ends up in the context like any other, where it can be read, pruned, or left alone.

A message sent while a turn is running **waits for the end of it**, and then goes in and gets a
turn of its own. It says so when you send it: until the turn ends it is on the screen but not yet
in the context, and that is the one moment those two disagree.

It cannot go in any earlier. The answer the model is still writing would land after it, leaving
the next request ending with the model talking rather than with your question — and mid-loop it
would land between a tool call and that call's result, where a request cannot have a user message. So a message typed to steer a turn is answered after that turn rather than during
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
│Careful · anything it has not been told about: ask                                                            │
│                                                                                                              │
│  capability or path     answer      the tools it covers                                                      │
│  read                   allow       read                                                                     │
│  write                  deny        write                                                                    │
│  shell                  allow       shell                                                                    │
│  network                allow       shell, when the command reaches for it                                   │
│  .env*                  deny        edit, read, write                                                        │
│                                                                                                              │
└──────────────── shell: confined · 12 more it will ask about · space cycles · a allow · n never · r ask again ┘
```

The line along the top is the policy in force and what it answers about everything the list does
not mention. It is there because a screen of permissions raises exactly one question before any of
the rows — *which policy is this, and what is it doing?* — and reading `/seams` for the name and
the source for the behaviour is not a screen. Both halves come out of the policy itself, so neither
can come to describe a `Careful` that has since changed its mind.

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

`allow` runs with no question. `deny` never runs and never asks: the model gets a tool result it
can read and work around, rather than a call that silently vanished. The transcript says which
stance did it — ``shell: refused by `network`, which this command reaches for`` — because "the
call was not permitted" beside a `shell: allow` is true and useless.

The model is told the same thing, and told which *kind* of refusal it was, which is the only part
it can act on:

```text
the call was not permitted: refused by the rule for `**/.env`. That is a standing rule
rather than an answer to this one call, so making the same call again will be refused
the same way.
```

```text
the call was not permitted: this call was refused when it was asked about. That is an
answer to this call rather than a standing rule, so a different approach may well be
allowed.
```

A model that cannot tell those apart does the wrong thing with either: it rephrases the same call
at a rule that will never move, or it abandons an approach that was only refused once. The
sentence comes from `Careful` and reaches the model through `PermissionPolicy::why`, so the
person and the model read the same reason instead of the person reading it alone.

### the question itself

It stands in the prompt's place on the **chat** tab, rather than being laid over the middle of
the screen:

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────┐
│> check whether example.com is up                                     │
│                                                                      │
└───────── alt+1 chat · alt+2 context · alt+3 trace · alt+4 permissions ┘
┌ a tool wants to run · tab ───────────────────────────────────────────┐
│ shell wants: shell, network                                          │
│                                                                      │
│ cmd: curl -s https://example.com                                     │
│                                                                      │
│ [tab] puts the keys here, and then:                                  │
│ [y] once   [a] always, for shell and network   [n] no                │
│ [i] the exact JSON   [d] drop it                                     │
└──────────────────────────────────────────────────────────────────────┘
```

The prompt is not underneath it, and that is the point: the box holding the keys is the box on the
screen. Stacked, the two disagreed on any window shorter than about fifteen rows — the question
needs the room, so the prompt gave way, and went on holding the keys and whatever had been typed
into it from *off* the screen. A session waiting on an answer nobody can give it without first
pressing a key nothing mentions.

**A question you cannot investigate is a question you cannot answer.** It used to be a box over the
middle of the screen, and while it was up nothing else worked — so being asked whether `amend` may
elide item 22 meant deciding about item 22 with the list saying what item 22 *is* underneath the
box asking. Here it takes none of that away: <kbd>ctrl+t</kbd> to the context tab, read the item,
come back, answer. The **chat** tab goes red on the strip while one is waiting, so the other three
tabs say what the session is waiting for.

**And it never takes the keys by itself.** <kbd>tab</kbd> is what gives them to it, and until it is
pressed none of the answers does anything — nor does <kbd>enter</kbd>, which is most of why this is
a gate rather than a layout. The answers are bare letters: a question that grabbed the keys on
arrival once read the <kbd>a</kbd> of "what" as `always, for shell` and kept it for the rest of a
live session, and an <kbd>enter</kbd> that still reached the prompt would send the half-written
message the question interrupted, starting a turn on the way to answering. What stays working
before <kbd>tab</kbd> is what scrolls the conversation, because reading is not answering. Whatever
was in the prompt is there again when the question has gone, with the keys back on it and no second
<kbd>tab</kbd> to press.

It names **everything the policy consulted**, not just what the tool declared, and <kbd>a</kbd>
answers for all of it. That includes any calls already queued behind this one: a model that asks
for three things at once produces three questions, and an "always" that did not reach them would
go back on itself one keystroke later. <kbd>y</kbd> is this call only — a `curl` allowed once runs
with the network open for that command and no other.

Arguments longer than the box get their own scrolling region between the header and the answers,
with <kbd>pgup</kbd> and <kbd>pgdn</kbd> moving them; the answers stay where they are. An `amend`
carrying a rewritten tool result is as long as the result was, and a question whose answers had
been pushed off the bottom of the screen is one nobody can answer.

Typing does not answer it, and it never takes the keys off you. A question arrives on its own
schedule, in the middle of whatever you happen to be typing, and its keys are ordinary letters —
`a` grants a capability for the rest of the session and is also the third letter of "what", which
is how one live session granted `shell` for good. So the question appears without asking for the
keys, and <kbd>tab</kbd> is how it gets them. Your letters go where you aimed them: into the
prompt. <kbd>enter</kbd> sends what is in the prompt, and it waits, exactly like a message sent
into a running turn does.

Coming *back* to the chat tab while a question is waiting does put the keys on it — that is what
the trip was for. So the whole gesture is: <kbd>ctrl+t</kbd>, look at the thing, <kbd>alt+1</kbd>,
<kbd>y</kbd>.

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

## 🤖 the same program, without a screen

`--headless` drives the session from lines on stdin instead of from keys. A line is what a line
typed at the prompt is: a message, or a command if it starts with `/`. It is implied when stdout
is not a terminal, and it says so rather than deciding quietly.

```console
$ printf 'what is 2+2? answer with just the number\n/budget\n' \
    | kamchatka --headless -m mercury-2 > session.jsonl
4
--- the budget ---
the next request: ~571 tokens, 12 of context and 559 of tool definitions

anchored on the last response: ~570 tokens - the provider's own 569 for the request it
answered, plus what the context has done since. This is the figure in the corner
…
```

**stdout is the session log**, one JSON record per line — the same bytes `/save` writes, so
`jq 'select(.event.event == "tool.requested")' session.jsonl` is the whole of reading a run back.
**stderr is a person's half**: what the model said, what a tool was asked to do, and what any
command you sent answered. Every verb is there, because a command was never the keyboard's to
begin with.

Nothing can be asked at a prompt that is not there, so the answers are given in advance:

| flag | what it does |
| --- | --- |
| `--allow read,shell` | answer `allow` for a capability, or a path rule: `--allow 'src/**'` |
| `--deny write,.env*` | the same, refused; the strictest of everything consulted still wins |
| `--on-ask deny` | what happens to a question nobody answered in advance. The default |

`--on-ask deny` rather than `allow` is the one default worth arguing about, and it is deliberate:
a run nobody is watching should not be able to do a thing nobody has allowed. The model is told,
and told that it was *this call* rather than a standing rule — so it works around it rather than
retrying:

```text
⟩ shell({"cmd":"echo hello"})
· shell: deny, because nobody is here to be asked
· shell: 48 tokens, an error
I’m not able to execute shell commands directly, but the command you asked about is
straightforward. …
```

Somebody else's tools are given the same way. Every tool an MCP server offers arrives carrying
`mcp:<name>` and nothing else, so `--mcp files=… --allow mcp:files` is the whole of granting one
server — the same subject the permissions tab writes when somebody answers **always** at the
prompt, given before the server has been spawned or said what it offers. Without it the tools are
there and every call is refused, which is the right way round: a server named on a command line is
not thereby trusted to run.

Nothing else can stop a run nobody is watching, so three things can. `--deadline 300` interrupts
whatever is in flight and leaves by the ordinary door — what arrived is kept and the session is
written out, which a killed process cannot say. <kbd>ctrl+c</kbd> does the same once, and leaves
at once if pressed again.

`--spend 50000` is the third, and it is there because time is not the only thing one of these can
spend: a model that has found a loop — a tool that fails the same way, a question it keeps
re-asking — will stay inside any deadline you were willing to give it. The unit is tokens, `input
+ output` as the provider reports them, because nothing here carries a price list and a figure in
money would be one. It is a stopping rule rather than a cap, since what a response cost is known
only once it has arrived:

```text
· spent 2,264 tokens of 2,000; stopping. `/spend N` raises the ceiling
```

It belongs to the *session* rather than to this loop, which is why it is on
[`wiring::Setup`](#-embedding-it) and not on the headless driver: what stops the turn that crossed
the line is `App`, and so is what refuses the next one — so a screen session, a piped script and a
host with a loop of its own are held to the same number, and none of them can get round it by not
asking. `/spend` says what has been spent and against what, `/spend N` raises it, and `/spend 0`
takes it away, which is the way back for whoever set it too low.

An endpoint that reports no usage at all says so, once, rather than holding a ceiling that nothing
will ever reach — a limit quietly never met is worse than no limit, because whoever set it is
reading the run as bounded. `--deadline` is the one that needs nobody's cooperation.

A line is read only while the runtime is resting, which is the one place this differs from a
person at a prompt and is what makes a piped script mean what it says: the lines of a script
cannot overtake the turns they belong to. `--no-default-features --features mcp` builds this and
nothing else — no screen compiled in, 88 crates lighter, and the same `--headless` behaviour
whether or not the flag is given.

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

So is how much of each tool's output the model is shown. `/limit` lists it — numbered, and the
number is one the command takes, so `/limit read 64000` and `/limit 3 64000` are the same
instruction — and either changes one from its next call onward. The result that has *already* been
cut is recovered a different way: its whole is archived beside the copy the model was given, and <kbd>space</kbd> on
it sends that instead — the projector answers one call with one result, so the whole takes the
call and the short copy drops out.

## 📎 putting a file in, and asking about it

`/attach` takes a path and then whatever you want to ask about it, so the file and the question
go out as one request:

```text
/attach ~/reports/q3.pdf what is the headline number, and what is it compared against?
```

What goes in depends on what the file is. Source, markdown, logs, CSV — anything this program has
no media type for — goes in as **text**, which is countable, readable on the context tab and
compactable like everything else. A PDF, an image or a recording goes in as **bytes**, and the
endpoint is told what they are. The extension decides, and only for the ten types in the table;
sniffing the content instead gets the interesting case wrong, because an uncompressed PDF is valid
UTF-8 for pages at a time and would be sent to the model as PDF source. A file that is neither a
listed type nor readable as text is refused rather than guessed at — a media type is a claim about
what the bytes are, and inventing one buys you an error message about a shape instead of one about
a file.

`-f` at startup is the same thing at a different moment — one function, so `kamchatka -f
diagram.png` works the same way — and with no question after the path, `/attach` just puts it in.

The one difference between them is the pin, and it follows from what the two acts are. A file
named on the command line is part of how the session was set up and is meant to still be there at
the end, so `-f` pins it. One attached at the prompt is something brought into a conversation, as
ordinary as a message, and it gets old the same way — so `/attach` does not, and <kbd>p</kbd> on
the context tab is there for the one that is meant to last.

Nothing here has a tokenizer for a picture, and it says so rather than putting a `0` where a
number should be:

```text
· [1] q3.pdf (file) [application/pdf, 292.47kB], 6 tokens and 1 piece(s) nothing here can price
```

That is not a rounding. In the live test that pins this, a 535-byte one-page PDF goes to Gemini
through OpenRouter: the counter puts the whole request at **19 tokens** and says one piece of it
has no number on it, and the provider charges **540**. Dividing the base64 by four — the thing the
counter refuses to do — would have said 179, which is not the answer either. The row on the
context tab reads
`0+` for the same reason, `/budget` counts how many pieces are in that state, and the figure in
the corner stops being a floor the moment the request has gone out once — because from then on
the provider's own number has the document inside it. If you want the estimate to be right
*before* that, `Kernel::set_counter` takes a counter that knows your vendor's formula, and
[`pricing_a_picture.rs`][pricing] in the runtime is forty lines showing one.

[pricing]: https://github.com/ljedrz/nachalnik/blob/master/nachalnik/examples/pricing_a_picture.rs

## 🔎 letting the agent read and manage its own context

`--introspect`, or `/introspect` at any point, offers two more tools. They are off by default,
because a model that can rewrite its own context is a decision rather than a default. There are
[write-ups](https://ljedrz.github.io/nachalnik/) of three sessions driven from these two tools:
one where an agent found a false note in its own context and corrected it, one where it took back
a hallucination of its own by rewriting the two turns it had made things up in, and one where it
answered *why do you think that* by forking itself and running the ablation rather than by
introspecting. Two more transcripts are up there in which I do the editing instead, through the
keys rather than through these.

**`introspect`** reads. `look` lists every item it is carrying — what each one is, what it costs,
whether it is going into the next request and why not if it is not — and reads any of them back,
block by block, including what it was thinking when it produced them. A long one comes back as its
start and its end: reading an item copies it into the context, so seeing all of a 9,000-token tool
result in order to decide whether to keep it costs about what keeping it costs. `whole: true` asks
for it anyway. `request` shows the
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

**`amend`** changes things. `elide`, `exclude`, `archive`, `pin` and `restore` move items between
the same states the <kbd>space</kbd> key does, and each action is named for the state it leaves —
which is the word you will read back on the item afterwards:

* `elide` — for a tool result that has served its purpose. The call stays answered, and the result
  stops costing what it holds.
* `exclude` — take it out altogether.
* `pin` — protect it from compaction.

Items are named by `ids`, or by `select`, which takes the same selector language `/exclude` does. So
"the tool results I am done with" is one call rather than twelve numbers read off a listing.

`revise` rewrites what an item says. `note` writes something into the context — a plan, a
conclusion, a thing not to try again. A note is attributed to `agent`, so the context pane can say
who put it there, and it can be pinned so compaction cannot take it. Saying the same thing out
loud in a turn is not a promise about anything; a pin is.

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

Every session is written out when it ends, whether or not it ended well, and the last thing
printed is where:

```text
2026-09-02T18-25-10Z · 9 events recorded
9 records in /tmp/kamchatka/2026-09-02T18-25-10Z.jsonl, and a session in
/tmp/kamchatka/2026-09-02T18-25-10Z.json
`kamchatka -r /tmp/kamchatka/2026-09-02T18-25-10Z.json` carries on from it
```

A session's name is when it started, in UTC, and it is also the name of its two files. It used to
be `kamchatka-1788373510`, which repeated the directory it was about to be written into and then
said nothing whatever to somebody reading a list of them.

The condition used to be "somebody typed `/save`", which is exactly backwards: a session that
ended badly is the one worth reading afterwards, and it was the one that left nothing. Nine runs
against a provider that timed out left empty files and no way to see how far any of them had got.
A resumed session keeps its name, so carrying on writes back to the same pair rather than
scattering a lineage across files. It is a temporary directory because this is a safety net and
not an archive — `/save PATH` is still how a session goes somewhere it will be next week — and
`--no-record` turns it off for anyone who would rather a transcript did not outlive the terminal.
The directory is `0700`: what goes in it is a whole conversation and every byte every tool
produced, written without anybody asking, and under an ordinary umask that would be a
world-readable file on a shared machine.

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

`/params KEY JSON` sets one model parameter and `/params` shows them — with what else this model
takes, where the endpoint publishes it. The reason it does the second thing is that a parameter a
model does *not* take is not refused: it is sent, ignored, and nothing anywhere says so, which
makes a `seed` set for a reproducible run buy no reproducibility and look exactly like one that
worked. Two models one session apart differed by eight of these. The runtime invents none of them
— only what you set is sent — and a listing that publishes nothing is read as silence rather than
as a prohibition, because ollama and a bare proxy both say nothing here:

```text
parameters: {"seed":42}
upstage/solar-pro4 does not list seed: sent, and ignored
upstage/solar-pro4 also takes: frequency_penalty, include_reasoning, max_tokens,
presence_penalty, reasoning, response_format, structured_outputs, temperature, tool_choice,
tools, top_p
```

## 📏 the number in the status line is a guess, and says which kind

Nothing here has the model's tokenizer, so the figure the status line leads with is an estimate —
it is written `~2,460` for that reason. The percentage beside it names the total it is a
percentage of (`0.9% (128k)`), because a fraction of an unstated number is not something anybody
can act on, and it turns yellow past 70% and red past 90%. Then comes what the provider actually
charged for the last request, and `/budget` is where all of it is reconciled:

```text
the next request: ~20,125 tokens, 19,953 of context and 172 of tool definitions

anchored on the last response: ~20,188 tokens - the provider's own 20,063 for the
request it answered, plus what has changed since

the limit: 1,048,576, which the next request would fill 1.9% of

the last request really cost 20,063, as the provider counted it

the counter has learned from 2 request(s) and scaled itself by 1.131: its own guesses
came to 35,447 tokens where the provider counted 40,073
```

The first line is the counter estimating the whole request from scratch. The second is the one the
corner shows once there has been a response to anchor on, and it is a different method rather than
a better guess: it starts from what the provider charged, adds what the context estimates *now*,
and subtracts what the estimator says the items that figure already covered would cost now. An
item that has not moved appears in both estimates and cancels — so it contributes its measured
cost and no error at all, and only what has changed since the last request is being guessed at.
The error is a few percent of the change instead of a few percent of the context, which is the
difference between a thousand tokens and thirty on a context of a hundred thousand.

It also absorbs, exactly and for nothing, the three things the counter is structurally blind to:
per-message framing, the tool schemas, and any payload it refuses to price. A PDF the counter
cannot put a number on is inside the provider's figure the moment it has gone out once.

That correction on the last line is the runtime's `Calibrating` counter: every response tells it
what the request it just estimated really cost, and it adjusts. Over a real session against Gemini
it went from 13% low to within 0.3%. A budget nobody can check is a decoration.

Before any response, on an endpoint that reports no usage, and after a change of model until the
next answer, the corner falls back to the plain estimate. And where something in the context has
no number on it at all, `/budget` says how many pieces — a figure that is a floor is never shown
as one that is complete.

`/budget` is about the next request; `/spend` is about the session. It adds up what the provider
charged for every response — measured, never estimated — and says it against the ceiling, if one
was set with `--spend` or with `/spend N`. The session stops at that ceiling, which is what it is
for; see [the guards on a run nobody is watching](#-the-same-program-without-a-screen).

The compactor shortens the oldest tool results to a marker once the context passes `--compact`
(0.8 by default) of the limit. It does not summarize them — it never read them — and it touches
nothing that is pinned, because the kernel refuses. Every one of them is still on the context tab,
marked `…`, still holding every byte it held, one <kbd>space</kbd> from coming back.

A marker costs something too — it is a line of text where the content was — so the compactor
counts what each elision actually recovers and leaves alone any result no bigger than the marker
that would replace it. Eliding a `wrote 412 bytes to …` makes the request *bigger*, and a pass
that took twenty of them is how that was found.

## 🧩 embedding it

The program is a library with a loop on top, and both halves are yours. `wiring::Setup` assembles
a session — the kernel, the policy, the four tools, the sandbox and the `App` around them — in the
order they have to go in, and hands back the two receivers a loop needs:

```rust
let wired = kamchatka::wiring::Setup {
    introspect: true,
    allow: vec![Subject::parse("read")],
    system: Some("you are working in a Rust workspace".into()),
    spend: Some(50_000),
    ..Default::default()
}
.wire(kamchatka::provider::connect("mercury-2").await?)?;
```

Two of those steps are not guessable and are the reason this exists rather than a page of
instructions: the event subscription has to happen *before* anything is plugged in, or the trace
is missing the wiring that set the session up; and the introspection tools hold a **weak** handle
to something the caller has to keep alive.

From there, `App::submit` takes a line — a message or a command — and answers with what it did,
what it said, and any page it opened:

```rust
let reply = wired.app.submit("/budget").await;
```

`headless::Headless` is one loop over that, and the program's own is the other. A host with an
event loop of its own wants neither: it holds the `App`, pumps `wired.events` into `on_event`, and
hands in a line whenever it has one.

`spend` above is the one thing a host gets whether it asks or not, which is why it is here rather
than on the headless driver where it started. `on_event` is the door every loop comes through, so
that is where the provider's own figures are added up; once they pass the ceiling the turn in
flight is interrupted and `App::start_turn` refuses the next one, so a host that keeps handing in
lines is told rather than quietly billed. `App::spent`, `App::spend` and `App::set_spend` are the
figure, the ceiling and the way to move it.

## 📦 installing

```console
$ cargo install kamchatka                     # from the registry
$ cargo install --git https://github.com/ljedrz/nachalnik kamchatka
$ cargo install --path kamchatka              # from a clone
```

Rust 1.88 or newer, and that is the whole list: no system libraries, no `pkg-config`, nothing
to install first. The TLS is `rustls` over `ring`, which builds its own cryptography rather than
looking for yours.

Two features, both on by default. `--no-default-features --features tui` drops MCP support and
the `--mcp` flag with it. `tui` is the other one, and it is the screen and the keys: without it
you get the same program, headless, 88 crates lighter.

**The sandbox is Linux-only.** The `shell` tool is confined with [Landlock](https://landlock.io),
which is a Linux LSM. Everywhere else the program builds and runs, but the shell is unconfined:
the permissions tab says `shell: a command can do any of these` rather than `shell: confined`, and
the stances are answers you were asked for rather than a boundary anything enforces.

On Linux it also wants a kernel new enough to have Landlock — 5.13 for the filesystem rules, 6.2
for the one that refuses `truncate()`, and 6.7 for `network: deny`. The tab says how much of it the
kernel took: `confined`, or `partly confined` where some of it is older than the machine.

### 🧰 a toolchain lives in your home directory

`$HOME` is not a system directory, so a confined command cannot read it — and most toolchains keep
their real installation there. `cargo` is a rustup shim, rustup reads `~/.rustup/settings.toml`
before it does anything at all, and a model asked to build a Rust project therefore gets

```text
error: could not read settings file: '/home/you/.rustup/settings.toml': Permission denied
```

which looks exactly like a missing compiler. Hand it the toolchain, for reading and no more:

```console
$ kamchatka --sandbox-read ~/.rustup,~/.cargo -m …
```

Read-only rather than `--sandbox-allow`, because a model that can *replace* the toolchain it is
about to run is not the trade anybody meant to make. The same goes for `~/.nvm`, `~/.pyenv`,
`~/.rbenv` and the rest. Both flags take a comma-separated list and may also be repeated, so a
checkout elsewhere and a scratch directory are one flag: `--sandbox-allow /srv/repo,/tmp/work`.

**Git needs no flag.** It used to: under Landlock `access(2)` still answers from the file's own
permissions, so git asked whether `~/.gitconfig` was readable, was told yes, opened it, got
`EACCES` and took the *unreadable configuration* branch — `fatal: unknown error occurred while
reading the configuration files`, and every git command in the session dead. A confined command is
now handed `GIT_CONFIG_GLOBAL` pointing at nothing when its configuration is out of reach, so git
gets the *no configuration* case, which it handles. Pass `--sandbox-read ~/.gitconfig` if you want
your identity and aliases in there too.

**A permission error says where it came from.** When a confined command is refused a path outside
its reach, the tool result names it:

```text
exit: 1 (the command reported a failure)
[/home/you/.rustup/settings.toml is outside what this session reaches, so the permission error
below is the confinement rather than the file's own permissions. …]
```

A refusal that names only paths the command *can* reach gets no such line: `cat /etc/shadow` is
refused with or without a sandbox, and hedging about it would send a model looking for a boundary
that had nothing to do with it.

**And it says where the session does reach**, which is the other half and was missing from the
three tools that run in process: their refusal named the working directory and called it as far as
this session goes, so a path opened up with `--sandbox-allow` was one the model then never tried.
It names all of it now, in the same words the `shell` tool's description uses:

```text
/home/you/.ssh/id_rsa: outside what this session reaches, which is /home/you/proj read-write,
/tmp/work read-write, /home/you/.rustup read-only. Work where it does, or ask for this path to
be opened up and say what you need it for.
```

## 🗂️ a settings file

`--config-file kamchatka.json` stands in for the arguments you would otherwise type every time.
JSON, because a project's settings are a handful of strings, numbers and lists and there is a
parser for that in the tree already:

```json
{
  "model": "mercury-2.5",
  "system": "you are working in a Rust workspace; run `cargo test` before saying anything is done",
  "mcp": ["files=npx -y @modelcontextprotocol/server-filesystem /srv"],
  "sandbox-read": ["~/.rustup", "~/.cargo"],
  "allow": ["read", "mcp:files"],
  "spend": 200000
}
```

Every key is optional, every one is named after the argument it stands in for, and **anything
given on the command line wins** — including a value that happens to be the default, because
`--requests 8` is somebody saying eight rather than somebody saying nothing. A list on the command
line *replaces* the file's rather than adding to it: one rule for every key is the only kind worth
predicting, and the other way round there is no way to ask for fewer. `--model` is the one setting
with a variable behind it, so the order there is command line, then `KAMCHATKA_MODEL`, then the
file.

A leading `~` in `sandbox-allow` and `sandbox-read` is your home directory. That is the one place
this program expands one, and the exception is narrower than it looks: every other way of giving
those paths has a shell in front of it that expanded `~` before the program saw anything, and a
file has nothing in front of it. The tools still refuse a leading `~` rather than expanding it,
because those paths are written by a *model*.

A key nothing reads is an error naming it, not a line that quietly does nothing:

```console
$ kamchatka --config-file kamchatka.json
Error: kamchatka.json: unknown field `modle`, expected one of `model`, `gemini`, `system`, …
```

What it deliberately does not carry is anything belonging to one invocation rather than to the
project: a message, `-r`, `-f`, and `--headless`, which decides for itself from whether stdout is a
terminal. There is no search for a file either — a settings file that applies because of where you
are standing is one that surprises you, so it is named or it is not read.

**A starting point ships with the crate**, as `kamchatka.json` beside this readme: every setting
there is, so you edit rather than remember, and every one of them at the program's own default.
Copying it wholesale changes exactly one thing — it sets a spend ceiling of 200,000 tokens, which
is the only thing a file adopted sight-unseen can safely offer. It grants nothing: `allow` is
empty, both sandbox lists are empty, `on-ask` is `deny`, and none of that is an oversight. A
default that pre-granted `read`, or opened up `~/.cargo` so that `cargo` works, would be this
program deciding on your behalf the one kind of thing it exists not to decide on your behalf —
and `~/.cargo` holds a registry token. The suite holds the file to naming every key, so a setting
added later cannot quietly go missing from it.

## 🎛️ options

```text
kamchatka [OPTIONS] [MESSAGE]...

  -m, --model <MODEL>       the model to talk to            [env: KAMCHATKA_MODEL]
                            [default: openai/gpt-4o-mini, or gemini-3.6-flash
                            with --gemini]
  -f, --file <PATH>         put a file in the context, pinned; a PDF or an image
                            goes in as itself. May be repeated
  -s, --system <TEXT>       a system instruction; the runtime ships none of its own
  -r, --resume <PATH>       carry on from a session written by /save
      --mcp <COMMAND>       an MCP server to run, as `[name=]command`; may be repeated
      --requests <N>        how many requests one turn may make; 0 is no
                            limit at all                                  [default: 8]
      --compact <FRACTION>  how full the context may get; 1 never compacts [default: 0.8]
      --parallel            run the model's tool calls at the same time
      --gemini              talk to Google's own API rather than an OpenAI-compatible
                            one, so a turn keeps the order it was produced in
      --introspect          offer the model the two tools that read and manage its own
                            context; /introspect turns them on and off while it runs
      --headless            drive the session from lines on stdin: the session log to
                            stdout, one JSON record a line, and what the model says to
                            stderr. Implied when stdout is not a terminal
      --allow <SUBJECT>     answer `allow` in advance for a capability or a path rule,
                            as read, shell, mcp:files, .env* ; comma-separated, may be
                            repeated
      --deny <SUBJECT>      the same, refused
      --on-ask <ANSWER>     what a question nobody is there to answer gets, in a
                            headless run: deny or allow               [default: deny]
      --deadline <SECONDS>  stop a headless run after this long, keeping what
                            arrived and writing the session out as usual
      --spend <TOKENS>      stop the session once the provider has charged this many
                            tokens for it, in and out; /spend changes it as it runs
      --sandbox-allow <PATH> a path outside the working directory the tools may also
                            read and write; comma-separated, may be repeated
      --sandbox-read <PATH> a path outside the working directory the tools may read
                            but not change; comma-separated, may be repeated
      --no-sandbox          run the shell tool unconfined, reaching whatever you can
      --forget-truncated    drop the whole of a shortened tool output instead of
                            keeping it as an archived item you can still read
      --config-file <PATH>  a JSON file of settings, for the ones you would otherwise
                            type every time; anything given here wins over it
      --no-record           do not write the session out when it ends; it goes to a
                            temporary directory otherwise, named on the way out

Environment:
  KAMCHATKA_API_KEY        the key; or OPENROUTER_API_KEY, or OPENAI_API_KEY
  KAMCHATKA_BASE_URL       where the requests go, e.g. http://localhost:11434/v1 for
                           ollama; OpenRouter by default, or Google's own v1beta
                           with --gemini
  KAMCHATKA_CONTEXT_LIMIT  the model's context size, for a provider that will not say
  KAMCHATKA_NO_ATTRIBUTION set to stop naming this program to OpenRouter
```

That is `--help`, which lists the environment too rather than leaving three settings for the
readme alone to mention.

`/save` writes two files: a `.jsonl` of every event that happened, and a `.json` snapshot of the
context. The snapshot has two ways back in.

`kamchatka -r PATH` starts a fresh session from it, which is the faithful one: the item numbers,
the model parameters and what the token counter had learned all come back exactly as they were,
because `Kernel::resume` is a constructor and builds the session around them.

`/load PATH` brings it into the session you are already in, which is the useful one. It is a
context operation and it plays by the same rule as the rest of them — nothing is destroyed. What
was in the context is **archived**, keeping its numbers and its contents; anything **pinned**
stays where it is, because a pin is you saying so and `--system` is pinned; the loaded items come
in as new items with new numbers, and the conversation they were is read back onto the chat tab.
<kbd>u</kbd> twice puts the whole thing back.

That makes a checkpoint out of a file. `/save good`, let the agent go somewhere useless, `/load
good`, and carry on from where it was still working — without losing the detour, which is sitting
in the context marked `▫` if you want to read it.

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

The other half is the program, and it is tested without a screen at all: the policy's own
questions, real commands under a real Landlock ruleset, the two introspection tools through the
real loop, a whole session driven by lines, somebody else's MCP server spawned as a child process,
and the settings file. `cargo test -p kamchatka --no-default-features --features mcp` runs those
and nothing else — which is also the check that the screen really is optional, since a suite that
only ever compiled with it could not tell you.

`cargo test -p kamchatka --test live` is the third kind and wants a key: it asks whether a request
these keys produced is one a real API accepts, and whether the sentences these tools write are ones
a model can act on. A scripted provider agrees with every refusal it is handed.

## 🎸 the name

`nachalnik` is an homage to KINO's *Nachalnik Kamchatki*. Kamchatka was the boiler room Viktor
Tsoi shovelled coal in; this is the one where the work actually happens.

## licence

MIT.

<!-- crates.io resolves a relative link against the directory this readme was published from,
     which is not where the repository root is. Links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[providers]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-providers
[nachalnik-mcp]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-mcp
[protection]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/README.md#-what-it-does-and-does-not-protect-you-from
