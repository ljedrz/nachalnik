# using kamchatka

The screen, the keys, the permission prompt, and the tools an agent reads and manages its own
session with. [The readme](README.md) says what the program is; this says how to drive it, and
[RUNNING.md](RUNNING.md) covers running it without a screen, configuring it and embedding it.

---

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
the model is still being sent, and reads a turn as it now stands rather than as it arrived. Both
of those are read off the context every frame rather than written down when they happen, so a
<kbd>u</kbd> that takes an edit back takes it off here too. A shell result opens with what the
command's exit said, and that line is drawn in the colour of what it says: **green** where the
command reported success, **red** where it reported a failure, and **yellow** where it never got
to report — stopped at your request, killed, or a status that could not be read at all. What a
`1` from `grep` means is not guessed at: telling *no match* from a fault means knowing what the
command was. The output under the line stays quiet, which is what makes the line worth looking
at. **context** is why this exists:

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│  id  label         kind               sending   held  what it says, or why it is not being sent            │
│  1 ▪ src/kernel.rs reference            1,045         pub struct Kernel;                                    │
│  2 ▪ q3.pdf        reference                6+        [application/pdf, 292.47kB]                           │
│  3 · user          user_message             6         what does the kernel do?                              │
│  4 · assistant     assistant_message        7         asked for read                                        │
│  5 - read          tool_result              0     15  excluded: at the terminal, by `tool:read`             │
│  6 · assistant     assistant_message        7         asked for shell                                       │
│  7 … shell         tool_result             11  8,993  compaction: compacted to make room                    │
│  8 · assistant     assistant_message       62  4,102  The kernel is a state machine with five states. …     │
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
For most rows the first is everything and the second is blank. The rows where they differ are the
ones worth finding: item 7 holds nine thousand tokens and spends eleven of them on the marker that
replaced it, item 5 spends nothing at all, and item 8 sends what it said while holding what it
thought. The `sending` column adds up to the figure on the status line, and the `held` column to
the `held back` beside it — which they did not, when the row reported what an item held under a
heading that said what it cost. Item 5 is marked `-` and says on its own
row why it is out, in the projector's words. Item 1 is `▪`, pinned, so the compactor will be
refused if it comes for it. Nothing disappeared: things changed state, and the state is on screen.

Item 7 is `…`, **elided**, which is the third answer between in and out. It is still in the
request — as the one line the row shows, saying it was compacted away — so the call on row 6 still
has an answer, and the model reads a conversation in which it asked for something and can no
longer see what came back. That is the truth. Dropping the result outright would have forced the
projector to drop the call with it, since a call with no result is a request most providers
reject, and the model would then be reading a conversation in which it never asked at all. What
an elided item holds beyond its marker is counted as held back rather than spent, and
<kbd>space</kbd> spends it again.

Item 8 is the last way the two columns come apart, and the one a session is full of rather than
the one it has once: most endpoints have no field for an assistant turn's thinking, so the projector does not
carry it back. The turn is `active`, every word it said is going, and the four thousand tokens it
*thought* are in the record, on this pane and in nothing that goes out. That belongs in the `held`
column rather than nowhere — it is what sending the whole of that turn would add, and on a
reasoning model it is most of what a session weighs. Switch to a provider that does take thinking
back and the same context reports it as spent, without a byte of it moving.

<kbd>tab</kbd> moves the keys between the prompt and the table:

| key | what happens |
| --- | --- |
| <kbd>space</kbd> | cycle how much of it the model gets: all of it → a `…` marker → nothing → back |
| <kbd>p</kbd> | pin it, so that the compactor is refused if it tries |
| <kbd>e</kbd> | change what it **says** — a tool call is not something a turn says, so a turn that is only a call declines this and tells you why |
| <kbd>f</kbd> | list only what the next request carries, or everything again |
| <kbd>/</kbd> | filter the rows: fuzzy, over the label, the kind and the whole of what an item holds — see below |
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

**`v1`**, **`v2`** and so on are what it said before somebody rewrote it, newest first. Both hands
that rewrite an item — <kbd>e</kbd> here and `amend`'s `revise` — **replace it in place**, which
keeps the number the item is referred to by and leaves the old text nowhere except the
`context.replaced` event. This reads it back off there, up to eight versions deep — including
after a `-r`, since `/save` writes that event stream to the `.jsonl` beside the snapshot and a
resumed session goes looking for it. The `as stored` page says whose hand it was:
`` rewritten by `user` `` for an edit at the terminal, and `` rewritten by `amend` `` with the
model's own reason for the tool.

<kbd>e</kbd> is the verb the others were missing. `space` and `p` decide whether the model reads an
item; `e` decides **what** it reads. The prompt turns into an editor holding the item's text, and
committing rewrites the item where it stands:

```text
  1 ▪ ledger.py    reference    477  """A running-balance ledger.
```

One row, the same number, and the same state it was in: editing decides what an item says, not
whether it is sent, so a pruned item stays pruned and an elided one stays elided. What it said
before is under <kbd>enter</kbd> as `v1`, and the edit is one <kbd>u</kbd> from coming back — on
both screens, since the conversation reads the item out of the context rather than keeping its own
copy. Trimming a 2,000-line file down to the function that matters is two keystrokes and a delete.

This used to **supersede** instead: the edit was a new item and the original stayed as a row
of its own, marked `~`. The row said what the `v1` page already said, and it cost a state to carry
over by hand, a kind to rebuild without orphaning the tool calls inside a turn, and a hint on the
new item so the conversation could read it back into the old place. [`Kernel::supersede`] is still
the runtime's, and it is the right shape for a client whose next round replaces the last while the
earlier ones stay readable — a session saved by one of those still draws in order here. It is not
the shape of a person fixing a sentence.

[`Kernel::supersede`]: https://docs.rs/nachalnik/latest/nachalnik/struct.Kernel.html#method.supersede

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
│14:22:24         permission.decided    shell: allow, answered when it was asked about                         │
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
thinking, and a command running. That is the question people bring to a log — *which step was
slow* — and here it is answered without subtracting a column of timestamps.

**What it never shows is how long you took.** A session spends most of its wall time in two places
where nothing is stepping at all: a permission question nobody has answered yet, and the wait
between one turn and the next thing somebody types. Those gaps used to be drawn like any other, so
the largest figure in the column was routinely a measure of how long a person had been reading —
which is the one number in there nobody should act on, and the one the eye goes to first. The line
that ends such a wait keeps its clock and is given no gap.

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
space for — the filename you are looking for is almost never on the first line — plus its label and
its **kind**, so `tool_result` narrows a long pane to the tool results and `assistant` to what the
model said, which is the question a pane of eighty rows is usually being put. The kind is matched
whether or not the column is on screen; it is dropped below 84 columns, and a filter that found
less on a narrow terminal would be the worse surprise. A trace row matches on the name, the detail
and the clock, so an hour or a date finds what happened in it. The
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
| <kbd>ctrl+d</kbd> | leave, from anywhere — including a permission prompt, where <kbd>d</kbd> on its own means something else |
| <kbd>F1</kbd> | the keys, opened at the tab you are on; also <kbd>?</kbd> on any tab but the chat one |

**<kbd>F1</kbd> answers for where you are standing.** The panel is one page per tab, plus one for
the slash commands and one for the keys that mean the same thing everywhere, and it opens at the
page for the tab it was pressed from — so asking what the keys are on the trace answers with the
trace's own, not with every key in the program and four fifths of them about somewhere else.
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

Three kinds of row. A **capability** is what a tool declares, which is what makes "always" work for
tools this program has never heard of — including an MCP server's, which all carry `mcp:<name>`. A
**path rule** is finer than any capability: `read: allow` is a reasonable thing to want and
`read .env: allow` is not, and the difference is a property of the file rather than of the tool
that opened it. An **action rule** is the same idea one tool along, spelled `<tool>:<action>`: a
capability is the whole of a tool and `amend` is not one decision, since `note` adds an item to
your context and `exclude` takes one out. So the four that change or remove what is already there
— `amend:elide`, `amend:exclude`, `amend:archive`, `amend:revise` — are subjects of their own and
stay questions whatever `amend` itself says, until somebody answers about them. The strictest of
everything consulted wins, so a rule can only tighten what a capability allows: `--allow
amend,amend:note` lets notes through and leaves an `exclude` a question, and there is deliberately
no way to spell the other way round. `network` is the odd one: no tool declares it, because a model
that wants the network writes `curl` — so the row says which shell it reaches, and when.

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

An `edit` is drawn as a diff, since it is the call where two blocks of near-identical text sit one
above the other and the whole question is which of them is on its way out: the value of `old` is
red and the value of `new` is green. The names stay in the panel's own colour, so what is green is
exactly the text that would end up in the file.

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

`--introspect`, or `/introspect` at any point, offers more tools. They are off by default,
because a model that can rewrite its own context is a decision rather than a default. There are
[write-ups](https://ljedrz.github.io/nachalnik/) of three sessions driven from the two that
existed when they were recorded, `context` (called `introspect` then) and `amend`:
one where an agent found a false note in its own context and corrected it, one where it took back
a hallucination of its own by rewriting the two turns it had made things up in, and one where it
answered *why do you think that* by forking itself and running the ablation rather than by
introspecting. Two more transcripts are up there in which I do the editing instead, through the
keys rather than through these.

**`context`** reads. `look` lists every item it is carrying — what each one is, what it costs,
whether it is going into the next request and why not if it is not — and reads any of them back,
block by block, including what it was thinking when it produced them. A long one comes back as its
start and its end: reading an item copies it into the context, so seeing all of a 9,000-token tool
result in order to decide whether to keep it costs about what keeping it costs. `whole: true` asks
for it anyway. `request` shows the
request about to go out, message by message, with what the projector left out and what it had to
repair.

`request` says which rule left each item out, which is two different answers in one list until it
is split: an item left out by **its own state** is one `restore` puts straight back, while one the
**projector** dropped — an orphaned tool result, a second result for one call — reads as `active`,
costs nothing, and does not move for a state change at all. It is a consequence of something else,
and the cause is the thing to fix. `setup policy` is the other half: this says what the rule did,
that says what the rule is.

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

`search` is the one that reaches the archive. An archived item is kept in full and never sent,
and until now the only way to see inside one was to read it back — which copies it into the
context, so a session that had put eleven megabytes away could not look at any of it without
undoing the saving it had just made. That made the archive write-only from the agent's side, which
is not what *nothing is destroyed* is supposed to mean. Same rule as `log`: the count and the price
first, the lines on request, never the item.

```text
⟩ context({"action":"search","text":"landlock"})

  14 line(s) say `landlock`, ~300 tokens if you take them all, in 2 item(s):
    13  archived    tool_result            9 line(s)  shell: cargo test --workspace…
    27  active      reference              5 line(s)  src/sandbox.rs

  `take` shows that many of the lines. None of this puts an item into your request: an
  archived one is still archived, and searching it changed nothing.
```

Case is ignored, because a model that searched for `landlock` in a context full of `Landlock` and
was told there were no matches has been told something false about itself, silently — the one
shape of wrong answer a search must not have. A nil result says what it looked at for the same
reason.

`draft` and `fork` take a snapshot of the context, resume it as a second kernel with **no tools**,
ask it once, and hand back only what it said. `draft` is for reading your own answer before you
give it; `fork` is for asking whether a piece of context is what is leading you astray:

```text
⟩ context({"action":"fork","question":"am I overfitting to the first stack trace?",
          "without":[14,15]})

  a copy of you, asked `am I overfitting to the first stack trace?`, on 9 of your items,
  without 14, 15, which the copy could not read at all. None of this is in your context
  and nobody has read it; it is yours to use or drop.
```

Leaving nothing out is said just as plainly, because *"on 9 of your items"* cannot be read as
*"on all of them"*. A live session asked a copy what it would conclude **without knowing my
earlier statement**, passed no `without` at all, got the matching answer back and reported it as
an ablation — it had asked the copy to pretend, which is not a test of anything, because the copy
is still reading the thing it is being told to disregard. A fork that took nothing away now says
so in as many words.

A fork can think; it cannot act, and it cannot go on thinking after it has answered once. Nothing
it does reaches this session's context or its log. Forking needed no change to the runtime at all
— `Kernel::snapshot` and `Kernel::resume` already *are* that, and leaving an item out is one field
on a copy of the snapshot.

**`log`** reads the other thing a session has, which is its own record. The context is what the
agent is carrying; the log sits beside it, costs nothing until something asks, and holds what a
context cannot — what an item *used* to say, which permissions were answered and how, which tools
appeared and went away. Called bare it hands back no records at all, only what there is:

```text
412 records, ~8,900 tokens if you take them all. Nothing here is in your context until you ask for it.
  context.added         180
  tool.repaired          96
  permission.decided     41
  context.compacted      12
  context.replaced        3

`take`, `ids`, `since` or `kinds` asks for the records themselves; the last sequence number is 412.
```

`take`, `ids`, `since` and `kinds` then ask for some of them — and **every answer opens with the
true total**, not the filtered one:

```text
412 records, ~8,900 tokens in all. 3 match kinds:["context.replaced"], ~90 tokens. Showing 3.
```

The total and the match count are separate questions, and a `take` on its own answers only the
second: it shortens what is *shown* without narrowing what counts, so it says `Showing the 3 most
recent; 409 older are not here` rather than claiming three records matched.

That one rule is what makes the tool safe to hand a model. A short answer is self-describing, so
truncation cannot read as absence — which matters more here than anywhere else, because the one
wrong answer a log can give is *nothing happened*. It is why a malformed `since` comes back as
something to correct rather than as an empty list, and why a filter that genuinely matched nothing
says so in words and lists the kinds that do exist.

What it will not do is interpret. The records arrive in order, named the way the kernel names them
and detailed the way the trace pane details them — the same function writes both, so the account a
model reads and the account you read cannot drift apart. The histogram counts every kind rather
than flagging an interesting one. `context.replaced 3` sitting in a list of five is a fact, and
noticing that it is an interesting fact is the model's job, not the tool's.

**`setup`** reads what the session is running *with*, which is state rather than events and is the
other half of what `log` does. Four things nothing else in this program could tell a model about
itself:

- **`model`** — which model it is, what parameters are being sent, how much context it has, and
  whether this conversation was **resumed from a snapshot**. That last one is the one that could
  not be worked out from inside: a restored context carries first-person turns this model never
  produced, possibly a different model's, and nothing in an assistant turn records which hand wrote
  it. Ask a model about its own earlier reasoning in a resumed session and it will own all of it,
  because it has no way not to. The test for this has the resumed session reading its predecessor's
  answer — *"this conversation started here"* — sitting in its own context, true when it was
  written and false now.
- **`tools`** — every tool on offer, what each declares it needs, and how much of its output
  reaches the model. Nothing anywhere let an agent enumerate its own toolset, so a `shell` removed
  between two turns was something it could only keep asking for, or confabulate a reason for not
  using. With `log`'s `tools.changed` beside it, the pair answers both halves: what there is, and
  when the other thing went.
- **`permissions`** — what the policy allows, refuses, or will stop and ask about, so a thing that
  will be refused can be told from a thing nobody has decided yet. Read off the policy's own table
  rather than by running a call through it: `evaluate` records the reason it refused something, and
  a read tool that asked it four questions would write four refusals into a queue of sixty-four and
  quietly evict the explanations a real refusal is going to need.
- **`policy`** — what the compactor and the projector will do to the context without being asked,
  by name, so they can be looked up. `look` has always said whether an item is going into the next
  request; it has never said what decided that, and a model that can read the verdict but not the
  rule cannot argue with either.

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

They are a tool per noun rather than one with a mode argument, because a tool declares its
capabilities once for every call it will ever receive. One tool would mean that answering
**always** to "may it read its own context?" also answered "may it rewrite a tool result?" — a
grant that delivers more than it implies, which is the shape of thing this program exists not to
do. So the permissions tab has a row for `context`, one for `log`, one for `setup` and one for
`amend`, and you can answer them differently.

Which also means each of them can be taken *away* separately, mid-session, and that is deliberate
rather than incidental. An agent whose ability to check the record is revoked half way through a
run is a thing this program can set up, and a thing worth watching a model in.
