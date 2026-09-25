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

The status line opens with the word for what the runtime is doing: `idle`, `asking`, `ready`,
`running`, `waiting on you` or `done`. While it is working, three dots move beside that word, and
after five seconds they are joined by how long it has been. Which dot is lit comes from the clock
rather than from a frame counter, so it stops where it is if the screen stops being drawn. `asking`
on its own is the same word whether a request is in flight or the program is wedged; the marker is
there to tell the two apart. It is absent while the runtime is resting, including when it is waiting
on **you**: nothing should suggest work is happening while a question sits unanswered.

Next along is the model and the address it is at, both of them, because the same name at a
different address is a different model. A session started without `-m` has no model yet, and the
corner says `no model` in its place rather than leaving it out: nothing is sent until `/model`
picks one, and `/models` lists what the endpoint serves.

After them comes the budget: what the next request is estimated to cost, marked `~` because it
is an estimate, as a share of the model's limit and with the limit beside it — green, then yellow
from 70%, then red from 90%. Once a request has gone out, what the provider really counted
follows it, and after that how much the context is holding back from the request.

**chat** is the conversation, and every terminal agent has one — this one also says which of it
the model is still being sent, and reads a turn as it now stands rather than as it arrived. Both
of those are read off the context every frame rather than written down when they happen, so a
<kbd>u</kbd> that takes an edit back takes it off here too. A shell result opens with what the
command's exit said, and that line is drawn in the colour of what it says: **green** where the
command reported success, **red** where it reported a failure, and **yellow** where it never got
to report — stopped at your request, killed, or a status that could not be read at all. What a
`1` from `grep` means is not guessed at: telling *no match* from a fault means knowing what the
command was. The output under the line stays quiet, which is what makes the line worth looking
at.

**context** is why this exists. It is not a summary and not a debug view: it is the list of items
the runtime is holding, in order, one row each, with what each one costs, whether it is going into
the next request — and the column that matters most, what the model will actually read of it.

| column | what it says |
| --- | --- |
| **id** | the number `/exclude`, `/copy` and <kbd>G</kbd> take, then the mark for its state |
| **label** | what the item is called: `user`, `assistant`, a tool's name, a file's |
| **kind** | `user_message`, `tool_result`, `reference` and so on; dropped below 84 columns |
| **sending** | what it puts into the next request |
| **held** | what it is keeping out of one, blank where that is nothing |
| **what it says** | the first line the model will read of it, or why what it holds is not sent |

| mark | state | what it means for the next request |
| --- | --- | --- |
| `·` | active | it goes |
| `▪` | pinned | it goes, and the compactor is refused if it comes for it |
| `…` | elided | it goes as a one-line marker in its place |
| `-` | excluded | it does not go |
| `▫` | archived | kept whole, and it does not go |
| `~` | superseded | a later item stands in its place |

The line along the bottom counts the items and how many of them are not having what they say
sent, the elided ones among them; past the limit it also says by how much the request is over.

A `+` after a figure in **sending** says the counter would not put a number on part of that item,
so the figure is a **floor**. Nothing here has a tokenizer for a PDF, and a bare `0` would have
made the largest thing in the request read as the cheapest row in the pane you opened to decide
what to delete.

**sending** is what an item puts into the next request; **held** is what it is keeping out of one.
For most rows the first is everything and the second is blank. The rows where they differ are the
ones worth finding: an elided tool result holds all it ever held and spends only the marker that
replaced it, an excluded item spends nothing at all, and an assistant turn sends what it said while
holding what it thought. The `sending` column adds up to the figure on the status line, and the
`held` column to the `held back` beside it. An excluded row says why it is out in the projector's
words, and an elided one gives the note it was elided with. Nothing disappeared: things changed
state, and the state is on screen.

A pin is worth most *before* a pass rather than after one, which is what `/compact` is for: it
lists every item the compactor would take and waits, in the prompt's place, for <kbd>y</kbd> or
<kbd>n</kbd>. The question is pinned rather than modal, so this tab is one keystroke away while it
stands: come here, <kbd>p</kbd> what should stay, go back and answer. Saying yes works the pass out
again, so what you just kept is not in it.

An **elided** item is the third answer between in and out. It is still in the request — as a
one-line marker in place of what it holds — so the call it answers still has an answer, and the
model reads a conversation in which it asked for something and can no longer see what came back.
Dropping the result outright would have forced the projector to drop the call with it, since a call
with no result is a request most providers reject, and the model would then be reading a
conversation in which it never asked at all. What an elided item holds beyond its marker is counted
as held back rather than spent, and <kbd>space</kbd> twice — out altogether, then back to all of
it — spends it again.

The marker the model reads says one more thing than the row has room for: *reading it again
would put the same tokens back into a context that had no room for them — ask for the part you
need instead*. Without that half, a model that wants the file back simply reads it again and the
compactor takes it again, over and over. A refusal that does not say the next attempt ends the
same way is read as an invitation to make it; with the second half, the model narrows to a search
instead.

An assistant turn's thinking is the last way the two columns come apart, and the one a session is
full of rather than the one it has once: most endpoints have no field for it, so the projector does
not carry it back. The turn is `active`, every word it said is going, and what it *thought* is in
the record, on this pane and in nothing that goes out. That belongs in the `held` column rather than
nowhere — it is what sending the whole of that turn would add, and on a reasoning model it is most
of what a session weighs. Switch to a provider that does take thinking back and the same context
reports it as spent, without a byte of it moving.

<kbd>tab</kbd> moves the keys between the prompt and the table:

| key | what happens |
| --- | --- |
| <kbd>space</kbd> | cycle how much of it the model gets: all of it → a `…` marker → nothing → back |
| <kbd>p</kbd> | pin it, so that the compactor is refused if it tries |
| <kbd>e</kbd> | change what it **says** — a tool call is not something a turn says, so a turn that is only a call declines this and tells you why |
| <kbd>f</kbd> | list only what the next request carries, or everything again |
| <kbd>y</kbd> | hand the whole of what it says to the terminal, for the clipboard — see below |
| <kbd>/</kbd> | filter the rows: fuzzy, over the label, the kind and the whole of what an item holds — see below |
| <kbd>enter</kbd> | read the whole of it — see below |
| <kbd>←</kbd> / <kbd>→</kbd> | move between its pages, while it is open |
| <kbd>u</kbd> / <kbd>U</kbd> | undo / redo the last change to the context |
| <kbd>23G</kbd> | go to the item numbered 23 — the number `/exclude` takes |

**<kbd>y</kbd> is for getting something out of here and into something else.** A screen is a
rectangle and a selection dragged over one is a rectangle too, so a mouse across the chat pane
takes the frame down both sides of every line with it, the wrapping of whatever width the window
happened to be, and none of what has scrolled past. This program is holding the answer itself —
unwrapped, whole, and with nothing drawn around it — and <kbd>y</kbd> hands that to the terminal
instead, which puts it on the clipboard. `/copy` is the same act from the chat tab, where the keys
belong to the prompt: with nothing after it, the last thing the model said; `/copy 7`, item 7.

It is [OSC 52](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Operating-System-Commands),
an escape sequence rather than a library, so it also works over `ssh` — the terminal at the far end
is the one with the clipboard you will paste into. What it cannot do is find out whether it worked:
there is no reply to read, and a terminal that does not implement it drops it silently. `foot` and
`alacritty` take it, `tmux` passes it on only with `set-clipboard on`, and Apple's Terminal has
never had it. So the line it prints says what it did — which item, and how many bytes it handed
over — and the byte count is what to compare against whatever turns up in the paste.

An oversized tool result is held as *two* items: the truncated copy the model was shown, and the
whole of it beside it, marked `▫ archived` and not going. <kbd>space</kbd> or <kbd>p</kbd> on that
row is how you say **send the whole thing** — it is the only way to say it, and the token count in
the row is what it will cost you.

<kbd>enter</kbd> opens the item, and an item has more than one honest answer to *what is this?*
The box is titled with the item's number, label, kind, state and cost, and it is paged: a strip
along its top names the pages, and <kbd>←</kbd> / <kbd>→</kbd> move between them.

**`to the model`** is what this item puts into the next request, taken from the projection of the
whole context rather than of the item alone — so a call the projector had to drop, or an ordered
turn it had to flatten, shows up here as the repair it really is. An item that is not going says
so, with the reason. That last one is not always visible from the row: a tool result whose call
has been taken out reads as `· active` and costs what it costs, and the projector drops it anyway,
because a result answering nothing is a request most providers reject. Its page says
`nothing: this item is not in the request`, and the box opens on that page rather than on the
text, since the projection rather than the state decides which of the two is the surprise.

**`as stored`** is what the item itself holds, which for an elided or excluded one is not the same
thing at all. That gap is the reason the pages exist, and it is why an item the model does not
read in full opens on the first page instead of the second.

**`v1`**, **`v2`** and so on are what it said before somebody rewrote it, newest first. Both hands
that rewrite an item — <kbd>e</kbd> here and `context`'s `revise` — **replace it in place**, which
keeps the number the item is referred to by and leaves the old text nowhere except the
`context.replaced` event. This reads it back off there, up to eight versions deep — including
after a `-r`, since `/save` writes that event stream to the `.jsonl` beside the snapshot and a
resumed session goes looking for it. The `as stored` page says whose hand it was:
`` rewritten by `user` `` for an edit at the terminal, and `` rewritten by `context` `` with the
model's own reason for the tool.

<kbd>e</kbd> is the verb the others were missing. `space` and `p` decide whether the model reads an
item; `e` decides **what** it reads. The prompt turns into an editor holding the item's text, and
committing rewrites the item where it stands. It stays one row, with the same number and in the same
state it was in: editing decides what an item says, not whether it is sent, so an excluded item
stays excluded and an elided one stays elided. What it said before is under <kbd>enter</kbd> as
`v1`, and the edit is one <kbd>u</kbd> from coming back — on both screens, since the conversation
reads the item out of the context rather than keeping its own copy. Trimming a 2,000-line file down
to the function that matters is two keystrokes and a delete.

This used to **supersede** instead: the edit was a new item and the original stayed as a row
of its own, marked `~`. The row said what the `v1` page already said, and it cost a state to carry
over by hand, a kind to rebuild without orphaning the tool calls inside a turn, and a hint on the
new item so the conversation could read it back into the old place. [`Kernel::supersede`] is still
the runtime's, and it is the right shape for a client whose next round replaces the last while the
earlier ones stay readable — a session saved by one of those still draws in order here. It is not
the shape of a person fixing a sentence.

[`Kernel::supersede`]: https://docs.rs/nachalnik/latest/nachalnik/struct.Kernel.html#method.supersede

**trace** is every event the runtime emits, as it happens, in the same names the session log is
made of. Each is one line: when it happened, the gap since the line above, the event's name, and
what it carries.

Every transition of the state machine is in there, and so is everything either side of it: what
was requested, what was decided, what was added to the context and what it cost. That goes down to
the wiring — plugging in a provider, a policy and each tool is itself an event, and the screen
subscribes before any of it happens.

Every event says what it carries, rather than just naming itself. `context.replaced` shows the
first line of what an item used to say, which is the one thing nothing else can recover.
`tool.repaired` names the identifier a provider reused, and what was wrong with it.
`tools.changed` lists the tools rather than counting them. The tab draws every event the session
recorded, and none of them as a name with an empty line beside it; both are tested.

**Two clocks, because neither answers the other's question.** The second column is the gap since
the line above, blank under a tenth of a second. Nearly everything in a session happens between one
frame and the next, so what is left with a number beside it is the model thinking, and a command
running — *which step was slow*, answered without subtracting a column of timestamps.

**What it never shows is how long you took.** A session spends most of its wall time in two places
where nothing is stepping at all: a permission question nobody has answered yet, and the wait
between one turn and the next thing somebody types. Drawn like any other gap, those would make the
largest figure in the column a measure of how long a person had been reading — the one number in
there nobody should act on, and the one the eye goes to first. The line that ends such a wait
keeps its clock and is given no gap.

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
off the screen before anybody could read it. So tool output is one `tool.output` line whose byte
count goes up as the output arrives, and the model's text is on the chat tab as it arrives. The pane
keeps the last few hundred lines. `/save` keeps every event there was, including the one line no
subscriber can ever catch: the kernel's own `session.started`, emitted while it is still being
constructed.

**<kbd>/</kbd> filters either of those two panes.** Four hundred events is not a log anybody reads;
it is a log somebody scrolls past looking for one line. <kbd>/</kbd> opens a one-row box where the
prompt would be — these panes are read and operated rather than typed into, so it is not taking
anything — and what you type filters the rows, fuzzily, counting what it found beside the query. It
is [`nucleo-matcher`](https://crates.io/crates/nucleo-matcher), which is Helix's, because that is
where the expectation of what fuzzy *feels* like comes from: `mreq` finds `model.requested`.

A context row matches on the whole of what the item holds rather than the one line the row has
space for — the filename you are looking for is almost never on the first line — plus its label and
its **kind**, so `tool_result` narrows a long pane to the tool results and `assistant` to what the
model said, which is the question a pane of eighty rows is usually being put. The kind is matched
whether or not the column is on screen; it is dropped below 84 columns, and a filter that found
less on a narrow terminal would be the worse surprise. A trace row matches on the name, the detail
and the clock, so an hour or a date finds what happened in it.

<kbd>←</kbd> and <kbd>→</kbd> move within the query, so a mistake four letters back is one you can
go to and fix — <kbd>backspace</kbd> takes out what is behind the cursor and <kbd>delete</kbd> what
is in front of it, and typing goes in where the cursor is. Everything else stays with the pane,
deliberately: the point of filtering four hundred events down to nine is to read the nine, and a
box that swallowed the scroll keys would mean closing the search, and so losing the filter, to look
at what it found. So <kbd>↑</kbd> <kbd>↓</kbd> and the paging still move between the rows
underneath, and so do <kbd>home</kbd> and <kbd>end</kbd> — which are all that is left of
<kbd>g</kbd> and <kbd>G</kbd> while every letter is going into the box. <kbd>esc</kbd> closes it,
and closing clears it: a filter that outlived its box would leave a window quietly showing four rows
of hundreds with nothing on screen saying why. Changing tabs clears it for the same reason.

And from anywhere, <kbd>ctrl+p</kbd> prints the request those items add up to — the kernel's own
rendering of it, not a description, under a header that counts the items in and out, names each
one the projector left out and why, and names each repair it had to make, such as a call dropped
because its result is not in the projection.

"why is that not in there?" is the question this whole runtime is for, and the JSON on its own can
only answer the other one. `/payload` goes one further and prints what the provider will put on
the wire, field for field - laid out to be read, where the wire has it on one line.

## ✍️ what the model writes

Models answer in markdown, so the chat tab reads it as markdown. Headings and emphasis get weight
and colour, inline code is told apart from prose, and list continuations hang under their bullets.
A fenced block gets a rule down its left rather than a slab of background — a slab being the one
thing a terminal cannot draw without knowing what colour the theme is.

Tables are drawn here rather than by the renderer, and for the same reason as the fenced blocks:
the renderer lays one out at whatever width its contents want, and a row wider than the window
would then be wrapped like a sentence — half a border on the next line, and the table apart.
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
| <kbd>up</kbd> | in an empty prompt, the last message back: the one still waiting, or a copy of the last one sent |
| <kbd>down</kbd> | put a recalled line away again, while nothing has been typed over it |
| <kbd>ctrl+l</kbd> | take this program's own lines off the chat; the conversation stays |
| <kbd>pgup</kbd> / <kbd>pgdn</kbd> | scroll the conversation |
| <kbd>ctrl+home</kbd> / <kbd>ctrl+end</kbd> | the beginning of the conversation / the end of it |
| <kbd>home</kbd> / <kbd>end</kbd> | the prompt's own, as in any other line editor |
| <kbd>ctrl+e</kbd> | follow the newest again |
| <kbd>tab</kbd> | move the keys between the prompt and whatever else on the screen wants them; from a tab with no prompt, back to the chat |
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

Where you leave the conversation is where it stays. Nothing but your own message moves it, so
what a long turn said thirty seconds ago stays readable while it goes on writing, and the line
along the bottom says how much has arrived underneath. <kbd>ctrl+e</kbd> goes back to following,
and so does scrolling down to the end. <kbd>ctrl+home</kbd> and <kbd>ctrl+end</kbd> are the two
ends of it in one key; control is held because <kbd>home</kbd> and <kbd>end</kbd> belong to the
prompt, and a line editor whose <kbd>home</kbd> moved something else would be a trap.

**Nothing said is shortened to fit.** However long an answer is, the whole of it is on the chat tab
and can be scrolled back through. A tool's output while it is still running is the one exception,
and only while it is running: a command that produces megabytes is bounded on screen, the whole of
it goes into the context, and the finished result is shown as its first few lines with the rest one
keystroke away on the context tab.

Stopping is cooperative rather than a killed process. The provider notices between fragments and
returns the text it has; the shell tool kills the command's process group, which is everything the
command started short of a process that left the group, as one run under `setsid` does, and still
answers the call it was given. The partial turn ends up in the context like any other, where it can
be read, excluded, or left alone.

A message sent while a turn is running **waits for the end of it**, and then goes in and gets a
turn of its own. It says so when you send it: until the turn ends it is on the screen but not yet
in the context, and that is the one moment those two disagree.

It cannot go in any earlier. The answer the model is still writing would land after it, leaving
the next request ending with the model talking rather than with your question — and mid-loop it
would land between a tool call and that call's result, where a request cannot have a user
message. So a message typed to steer a turn is answered after that turn rather than during it.

While it waits, <kbd>up</kbd> takes it back. In an empty prompt that key puts the last message
back where you can change it: the one still waiting if there is one, and otherwise a copy of the
last line you sent, which is often enough just a way to read what you asked. A waiting message is
*taken* rather than copied — nothing is queued afterwards, so the changed version is what goes in,
and a message you take back and never send again simply does not go in at all. There is only ever
one waiting message, which is why one key reaches it; everything else you have sent is on the
context tab, with more said about each of them than a prompt could show.

<kbd>up</kbd> does this only from an empty prompt, and only when there is something to put back;
before anything has been sent it scrolls the conversation, as it does at the top of any prompt.
With anything typed it still moves the cursor, and at the top line it still scrolls.
<kbd>down</kbd> puts a recalled line away again — but only while the prompt still says exactly
what came back, since a word typed onto the end makes it a message somebody is writing, and a key
that emptied the box then would be the worst kind of shortcut.

**<kbd>ctrl+l</kbd> takes this program's own lines off the chat.** The `·` notes about what it
just did, and the answers it gave a key you pressed — a session that excludes eleven items one at
a time has eleven of them interleaved with the run you are trying to read, each worth saying once
and none worth keeping. It is what the key means in a shell, narrowed to the only thing here that
is safe to clear: **the conversation stays**, because the conversation is the context, and nothing
on this screen hides an item — that is the context tab's to do, on a row that says so afterwards.
Nothing is said to report that it happened, which would be the first line of the pile it just
cleared, and the trace keeps every event either way.

A pasted block arrives as the lines it was pasted as: bracketed paste keeps a pasted newline from
being read as <kbd>enter</kbd> and sending half of it, and the carriage returns a terminal spells
those newlines with are put back.

## 🔑 the permissions tab

The other place the policy appears is the permission prompt — one call at a time, at the moment
you are least inclined to think about it. **permissions** is every answer you have given, in one
place, where it can be changed. Each row is a capability or a path, the answer standing for it, and
what that answer covers.

The line along the top is the policy in force and what it answers about everything the list does
not mention. It is there because a screen of permissions raises exactly one question before any of
the rows — *which policy is this, and what is it doing?* — and reading `/seams` for the name and
the source for the behaviour is not a screen. Both halves come out of the policy itself, so neither
can come to describe a `Careful` that has since changed its mind.

A fresh session has no rows at all: everything starts at `ask`, and the tab fills up as you answer.
Rows are **decisions**, not defaults. `ask` is what this policy does about anything nobody has
mentioned, so a row per undecided thing would be a screenful of "it will stop and ask" burying the
one or two lines that say what this agent can do *without* stopping. What is not listed is counted
instead, along the bottom, as how many more it will ask about — because a screen showing a handful
of decisions while standing for many more answers would be a different kind of dishonest. A subject
arrives here when somebody answers a question about it, and cycling one back to `ask` takes it off
again, which is what taking a decision back looks like.

A rule about a whole domain is one row too. `--allow log` decides `log:read`, and the `log` row
names it in the column beside it rather than the operation getting a row that says the same answer
back — `--allow context` would otherwise put a row on the screen for every one of its operations.
An operation is a row of its own when somebody has answered about it separately:
`--allow fs --deny fs:write` is two decisions.

**What it covers** is said in the terms the rule is written in, and is never wider than the rule: a
domain names the operations in it, an operation names itself, and a server or a path rule names the
tools it binds; `fs:glob  allow  fs` would be one operation reading as an answer about the whole
tool. Where nothing here is judged by a rule the column says that instead, so a flag that reaches
nothing looks like one — which is what `--allow mcp` is beside a server this program spawned, since
a call to one of its tools is judged as the server it came from.

What that costs is that you cannot refuse something here that has never come up. Deciding in
advance means answering the first question with <kbd>a</kbd> or <kbd>n</kbd>.

The line along the bottom opens with the shell, because it is the one thing on this tab that is not
negotiable. A registered `shell` that is not refused can read, write and reach the network whatever
the other rows say — so `shell: confined` (or `shell: a command can do any of these`) is what makes
the rest of the table mean anything. After it comes `network gated` or `network not gated`: whether
a command is asked about when it opens a socket, or read off its name before it runs.

Four kinds of row, and the first two are one thing at two depths. A **domain** is what a tool acts
in — `fs`, `exec`, `context` — and answering for one answers for everything done in it, which is
what makes "always" work for tools this program has never heard of. An **operation** is one thing
done in a domain, spelled `<domain>:<operation>`: `fs:read` and `fs:write` are not the same
decision, and neither are `context:note`, which adds an item to your context, and
`context:revise`, which rewrites one. A **path rule** is finer than either — `fs:read: allow` is a
reasonable thing to want and `fs:read .env: allow` is not, and the difference is a property of the
file rather than of the tool that opened it. It binds every tool that is handed a path, the walks
included: a walk cannot *ask*, so what `grep` and `glob` do about a rule that is not `allow` is
not open the file, and say how many they left alone. A link does not take a file out from under
one: `read`, `write` and `edit` refuse a link to a file a rule has not allowed, naming the file so
the next call can ask for it by that name, and a walk leaves it out with the rest. A **server** is
the odd one out, because it is about where a tool came from rather than what it does — which is the
one thing about an MCP tool that nobody has to take the server's word for.

The most specific rule that has an answer decides, and a refusal above it overrules.
`--allow context` allows the lot; `--allow context:note` allows a note and says nothing about the
rest; `--allow context --deny context:revise` is everything but that one. What a finer rule cannot
do is overrule a refusal — a domain you have *denied* stays denied however finely an operation in
it is named, because the strictest of everything consulted wins and `--deny` is the last word.
`net:reach` is the one nothing declares, because a model that wants the network writes `curl` — so
the row says which shell it reaches, and when. Where the network is gated, "when" is literal: a
command is asked about the moment it opens an internet socket, whatever it is called. Where it is
not, it is a guess from the command's name, and `git status` is asked about while a script that
opens a socket of its own is not.

<kbd>space</kbd> cycles a row through **ask → allow → deny**, or
<kbd>a</kbd>/<kbd>n</kbd>/<kbd>r</kbd> directly, and it takes effect on the next call. Answering
"always" at a permission prompt writes to this same table — the prompt and the tab are one object,
not two.

`allow` runs with no question. `deny` never runs and never asks: the model gets a tool result it
can read and work around, rather than a call that silently vanished. The transcript says which
stance did it — ``shell: refused by `net:reach`, which this command reaches for`` — because "the
call was not permitted" beside a `shell: allow` is true and useless.

The model is told the same thing, and told which *kind* of refusal it was, which is the only part
it can act on. A refusal by a standing rule gives the policy's reason and says that making the same
call again will be refused the same way. A refusal given when the call was asked about says it was
an answer to that call rather than a standing rule, so a different approach may well be allowed.

A model that cannot tell those apart does the wrong thing with either: it rephrases the same call at
a rule that will never move, or it abandons an approach that was only refused once. The reason comes
from `Careful` and reaches the model through `PermissionPolicy::why`, so the person and the model
read the same reason instead of the person reading it alone; which kind of refusal it was is the
kernel's to say, since the kernel is what resolved the grant.

### the question itself

It stands in the prompt's place on the **chat** tab, rather than being laid over the middle of
the screen. It is headed `a tool wants to run`, names the tool and everything the policy will judge
the call by, shows the arguments, and ends with the answers:

| key | what happens |
| --- | --- |
| <kbd>y</kbd> | run this call, once |
| <kbd>a</kbd> | always, for everything the question names |
| <kbd>n</kbd> / <kbd>esc</kbd> | no |
| <kbd>i</kbd> | the exact JSON, and the tool's own definition |
| <kbd>d</kbd> | drop every call it is waiting on, and tell the model why |

The prompt is not underneath it: the box holding the keys is the box on the screen. Stacked, the
two would disagree on any window shorter than about fifteen rows: the question needs the room, so
the prompt would give way and go on holding the keys, and whatever had been typed into it, from
*off* the screen — a session waiting on an answer nobody can give without first pressing a key
nothing mentions.

**A question you cannot investigate is a question you cannot answer.** Being asked whether
`context` may elide item 22 means deciding about item 22, and what item 22 *is* is on the context
tab. The question takes none of that away: <kbd>ctrl+t</kbd> to the context tab, read the item,
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

Coming *back* to the chat tab while a question is waiting does put the keys on it — that is what
the trip was for. So the whole gesture is: <kbd>ctrl+t</kbd>, look at the thing, <kbd>alt+1</kbd>,
<kbd>y</kbd>.

It names **everything the policy consulted**, not just what the tool declared, and <kbd>a</kbd>
answers for all of it. That includes any calls already queued behind this one: a model that asks
for three things at once produces three questions, and an "always" that did not reach them would
go back on itself one keystroke later. <kbd>y</kbd> is this call only — where the network is not
gated, a `curl` allowed once runs with the network open for that command and no other.

Arguments longer than the box get their own scrolling region between the header and the answers,
with <kbd>pgup</kbd> and <kbd>pgdn</kbd> moving them; the answers stay where they are. A `revise`
carrying a rewritten tool result is as long as the result was, and a question whose answers had
been pushed off the bottom of the screen is one nobody can answer.

A shell command is drawn as code — the rule down the left, the colours a fenced block gets on the
chat tab — with its own joints picked out: the `|`, `&&`, `||` and `;` that join one stage to the
next are coloured, because they are what you are scanning for. Wrapped as prose it would be folded
wherever the space ran out and the continuation would go back to the margin, so the second half of a
pipeline would sit under `cmd:` looking exactly like the next argument; the rule settles that, and
it wraps at a space rather than cutting mid-word the way a block of code does.

The joints are read off the command rather than off the highlighter, which calls every flag's
hyphen an operator and does not tokenise `|` at all. A separator inside a quote is not a joint and
is not coloured as one, and a command this cannot read to the end — an unterminated quote, an
unclosed `$(` — is drawn with nothing picked out rather than guessed at. <kbd>i</kbd> is the
byte-exact view, and stays the thing to reach for when the question is what *precisely* would
run.

A build with `--features shell-advisor`, run with `--advise`, puts one more line in the header,
under the one naming what the call wants, above the arguments and inside the part that does not
scroll: what the advisor reads the command as, and how sure it was.

The reading is green, yellow or red, off a three-level rubric — it only reads, lists, searches or
changes directory; it changes files inside the working directory, the way git or a rebuild could
undo; it reaches outside the working directory, destroys something that cannot be got back, or
sends something off this machine. It decides nothing: the verdict is the same one the rules would
have given, and an advisor that is down costs the colour and nothing else: the line says it could
not rate the command, and why. A rating nobody was sure
of is never drawn green, which is why the percentage is on the line — a yellow you cannot explain
is a yellow the advisor could not place. [RUNNING.md](RUNNING.md) has what it sends out.

An `edit` is drawn as a diff, since it is the call where two blocks of near-identical text sit one
above the other and the whole question is which of them is on its way out: the value of `old` is
red and the value of `new` is green. The names stay in the panel's own colour, so what is green is
exactly the text that would end up in the file.

### a command that reaches for the network

Where the network is gated, a command is not asked about for what it is called. It runs, and the
moment it opens an internet socket — a DNS lookup counts — the call is held and the question stands
in the prompt's place, headed `a command wants the network`, with the command under it:

| key | what happens |
| --- | --- |
| <kbd>y</kbd> | this command may, for the rest of it |
| <kbd>a</kbd> | always: `net:reach` allowed from now on, and for every command already waiting |
| <kbd>n</kbd> | this command may not, and every socket it asks for is refused |

It is asked once per command: a build that opens a hundred connections is one question. The command
waits while it is up, and <kbd>esc</kbd> is what it always is while something runs — the turn
stops, and the command and its question go with it. `tab` puts the keys on it, as on the other
kind.

What the model is handed says what happened, near the top where an output limit cannot take it:
that the command reached for the network and that you let it, or said no. A refused lookup comes
back from the command as `Temporary failure in name resolution`, which reads like a network with a
problem rather than a network refused, and a model that is not told the difference goes looking for
another way out.

`deny` refuses every internet socket a command opens without asking — UDP too, which the ruleset
alone cannot — and says so the same way. `allow` asks nothing. An <kbd>a</kbd> given to one command
reaches the others still running: one that has not reached out yet goes by it when it does.

## 🔦 finding things without a shell

`fs`'s `grep` and `glob` are what the model reaches for to find its way around, and the reason
they exist is the *capability* they ride. Finding a symbol used to mean `exec:run`, which subsumes
every other capability — so a session that only wanted to be asked about a repository had to hand
over the one permission that answers for everything. These declare `fs:grep` and `fs:glob`, and
the path rules that bind a read bind them too.

Underneath is ripgrep's own engine, linked in rather than shelled out to: no `rg` on the machine,
no second process for the sandbox to think about, and the walker that knows what a `.gitignore`
means. What this program writes is the *printer*. An answer opens with a line counting the
matches, the files they are in and the files searched; a `skipped:` line follows wherever anything
was left unopened, and then the matching lines, each as `path:line:text`.

**It cuts at matches, not at bytes,** and says that it stopped. A byte limit takes the tail of the
last file searched and leaves the model believing it has seen the rest, which is what makes an
agent run the same search three times. A hundred matches is the ceiling, a line is cut at two
hundred characters with a `…`, and when either fires the first line says so and says what to do
about it.

**`files_only` is the first thing it suggests**, and it is what `grep -l` is for: the files that
matched and how many each has, most first, instead of the lines. A common word fills the cap
early, and because the walk is alphabetical all hundred lines can come from the first directory it
reaches, never getting to the file the question was about. The same search with `files_only`
costs a fraction of the tokens, sees every file, and lists them as `path: count`, the ones with
most matches near the top where they can be read properly.

**And it accounts for what it did not read.** The count of files searched is what tells
"the symbol is not there" from "nothing was opened" — the same distinction the context pane draws
between an empty pane and a filtered one — and the `skipped:` line names every category: a path
rule, a link out of reach, a binary file, one that could not be read.

The rules it walks by, all of which are said in the tool's own description so the model is not
guessing: what a `.gitignore` hides is skipped, `.git` always; hidden files **are** searched, since
a model that cannot find `.github/workflows` concludes the file is not there; a symbolic link is
read where it points inside the working directory and counted where it points out; and a path rule
that is not `allow` stops a walk opening that file, because "ask me first" is not a thing a walk of
nine hundred files can honour. The path the call *names* is judged the way `read`'s is, so
`grep` in `.env` is a question exactly as reading it is.

`glob` is the same walk with a different question: `**/*.rs` in, matching paths out, in the shape
`ls -R` prints them — a directory and a `:`, the names in it that matched, and a blank line before
the next. A pattern that matches most of a tree is the usual one, and written out whole the
directories were most of its answer; a model reads `ls -R` without being taught it, and one that
now and then reads a bare name as a path loses a call, not a screenful. Both are deterministic —
two identical searches give the same answer in the same order, which matters because the answer
becomes a context item, and two items differing only in their order are two items nobody can diff
and a budget pays for twice.

**A long file is read in parts, and `read` says where each one ends.** Past the output limit —
32,000 bytes, unless `/limit fs:read` says otherwise — it stops at the last whole line that fits,
and its first line says which lines those are and the `from` to read on with. `from` and `lines`
read any part of a file, a log too large to hold in memory included. The other way to read a part
is `sed -n` through `shell`, which is `exec:run` again, for a file the session may already read.

## 📎 putting something in, with or without a question

`/attach` takes a path and then whatever you want to ask about it, so the file and the question
go out as one request:

```text
/attach ~/reports/q3.pdf what is the headline number, and what is it compared against?
```

What goes in depends on what the file is. Source, markdown, logs, CSV — anything this program has no
media type for — goes in as **text**, which is countable, readable on the context tab and
compactable like everything else. A PDF, an image or a recording goes in as **bytes**, and the
endpoint is told what they are. The extension decides, and only for the ten types this program has a
name for; sniffing the content instead gets the interesting case wrong, because an uncompressed PDF
is valid UTF-8 for pages at a time and would be sent to the model as PDF source. A file that is
neither a listed type nor readable as text is refused rather than guessed at — a media type is a
claim about what the bytes are, and inventing one buys you an error message about a shape instead of
one about a file.

`-f` at startup is the same thing at a different moment — one function, so `kamchatka -f
diagram.png` works the same way — and with no question after the path, `/attach` just puts it in.

**`/note` is the same act with a message instead of a file.**

```text
/note the CI runner has no network; a test that fetches will hang there
```

Everything else a person can say to a model goes in as a message, and a message starts a turn — so
telling it a fact it will need in four turns' time costs a request, an answer, and an "understood"
nobody wanted. Saying it *with* the next question buries it; saying it afterwards is too late. A
note goes into the context and stops there: nothing is sent, and it is carried by the next request
like everything else on that tab.

It arrives as a **reference**, not as a message, and the model reads it with its label, `note:`,
on the line in front of the text.

That is the same shape an attached file goes out in, and it is what makes a note legible as
something you *handed* the model rather than something you asked it. On this end it has a source of
its own — `memory` — so `/exclude memories` names every note you have written and nothing else, the
row says where it came from, and the chat draws it as what went in rather than as a line you spoke.
Like an attachment it is **not** pinned: <kbd>p</kbd> on the row is how you say this one should
survive compaction.

The one difference between `-f` and `/attach` is the pin, and it follows from what the two acts are.
A file named on the command line is part of how the session was set up and is meant to still be
there at the end, so `-f` pins it. One attached at the prompt is something brought into a
conversation, as ordinary as a message, and it gets old the same way — so `/attach` does not, and
<kbd>p</kbd> on the context tab is there for the one that is meant to last.

Nothing here has a tokenizer for a picture, and it says so rather than putting a `0` where a
number should be: the line the chat prints for an attachment gives its media type and size, the
tokens the counter could put a figure on, and how many pieces of it nothing here can price.

The gap between that count and the bill is not a rounding. A request carrying a one-page PDF is one
the counter puts at a few tokens, saying one piece of it has no number on it, and one the provider
charges hundreds for; dividing the base64 by four — the thing the counter refuses to do — does not
land on the bill either. The row on the context tab carries a `+` for the same reason, `/budget`
counts how many pieces are in that state, and the figure in the corner stops being a floor the
moment the request has gone out once — because from then on the provider's own number has the
document inside it. If you want the estimate to be right *before* that, `Kernel::set_counter` takes
a counter that knows your vendor's formula, and [`pricing_a_picture.rs`][pricing] in the runtime is
one written out, beside the same context counted by a counter that has no formula and says so.

[pricing]: https://github.com/ljedrz/nachalnik/blob/master/nachalnik/examples/pricing_a_picture.rs

## 🔎 letting the agent read and manage its own context

Four of the tools are about the session itself, and they are offered like the rest of them.
`/tools toggle context` takes one away and `/tools toggle context` again gives it back, and the
`tools` key in a settings file says which of them a session starts with — which is where a project
that does not want its agent buying extra requests leaves `fork` out.

There are [write-ups](https://ljedrz.github.io/nachalnik/) of three sessions: one where an agent
found a false note in its own context and corrected it, one where it took back a hallucination of
its own by rewriting the two turns it had made things up in, and one where it answered *why do you
think that* by forking itself and running the ablation rather than by introspecting. They were
driven from the two tools that existed when they were recorded — `context` (called `introspect`
then) for reading and `amend` for changing, which are one tool now. Two more transcripts are up
there in which I do the editing instead, through the keys rather than through these.

One rule runs through all of them and through `fs`. **An argument that the named action does not
read is refused, not ignored.** `fs {action: "read", …, old: "…"}` is a session asking for the part
of a file around some text; `old` is a real `fs` argument and `edit` is whose, so a read that
ignored it would answer with the whole file — a real answer, to a call nobody made, with nothing in
it saying so. The refusal names the action the argument belongs to when exactly one does, which is
what a session in that position needs to know. The same is true of
`context {action: "note", ids: […]}`: `note` writes a new item and has nothing to do with an id.

**`context`** reads. `look` lists every item it is carrying — what each one is, what it puts into
the next request and what it is holding out of one, and why it is out if it is out — and reads any
of them back, block by block, including what it was thinking when it produced them. A long one
comes back as its start and its end: reading an item copies it into the context, so seeing all of
a 9,000-token tool result in order to decide whether to keep it costs about what keeping it costs.
`whole: true` asks for it anyway. `request` shows the request about to go out, message by message,
with what the projector left out and what it had to repair.

`request` says which rule left each item out, which is two different answers in one list until it
is split: an item left out by **its own state** is one `restore` puts straight back, while one the
**projector** dropped — an orphaned tool result, a second result for one call — reads as `active`,
costs nothing, and does not move for a state change at all. It is a consequence of something else,
and the cause is the thing to fix. `setup policy` is the other half: this says what the rule did,
that says what the rule is.

`budget` is the one a decision gets made from. It gives the estimate for the next request against
the limit, split between the context and the tool definitions; how much is being held back, and by
what; what the last request really cost, as the provider counted it; and how far the estimate is
being corrected by what earlier requests cost. Then it lists up to ten of the most expensive items
actually going into the request, each with its state, its kind, what it sends, a running total,
and the start of what it is.

Estimates are named as estimates, the provider's own figure sits beside them, and the list is
what the request *actually* carries — an orphaned tool result the projector repairs away costs
nothing however active it looks, and offering it as something to give up would be advice that
buys nothing. Items that are not the agent's to move say so, rather than costing it a refused
call.

An assistant turn is the same thing one step subtler, and it is on nearly every row of a real
session: it sends what it said and holds what it *thought*, because most endpoints have no field to
send thinking back in. Ranked by what it holds it would head this list, offering an elision that
frees what the turn said under a heading priced by what it thought. So the list ranks on what a row
sends — the column the decision is actually made from — and says what it is holding beside it,
because an agent that cannot see the difference cannot tell a context it could shrink from one it
cannot.

`search` is the one that reaches the archive. An archived item is kept in full and never sent, and
reading one back copies it into the context, so without `search` a session that had put eleven
megabytes away could not look at any of it without undoing the saving it had just made. That would
make the archive write-only from the agent's side, which is not what *nothing is destroyed* is
supposed to mean. Same rule as `log`: the count and the price first, the lines on request, never the
item. A search answers with how many lines say the text, what taking them all would cost, and which
items they are in, with each one's state; `take` shows that many of the lines, and an answer
without it says that searching an archived item left it archived.

Case is ignored, because a model that searched for `landlock` in a context full of `Landlock` and
was told there were no matches has been told something false about itself, silently — the one
shape of wrong answer a search must not have. A nil result says what it looked at for the same
reason.

The other eight operations of **`context`** change it. `elide`, `exclude`, `pin` and `restore` move
items between the same states the <kbd>space</kbd> and <kbd>p</kbd> keys do, and each is named for
the state it leaves — which is the word you will read back on the item afterwards:

* `elide` — for a tool result that has served its purpose. The call stays answered, and the result
  stops costing what it holds.
* `exclude` — take it out altogether.
* `pin` — protect it from compaction.

Items are named by `ids`, or by `select`, which takes the same selector language `/exclude` does. So
"the tool results I am done with" is one call rather than twelve numbers read off a listing. One or
the other, and a call giving both is refused rather than answered on whichever it read first: a move
that quietly dropped half of what it was told reads exactly like a move that did what it was asked.
That cannot be said in the schema — mutual exclusion is `oneOf`, and neither of the two dialects one
schema has to go out in has the keyword — so it is said in each argument's description and enforced
where the call is read.

`look` takes the same `select`, and that is the only way to resolve a selector without using it on
something. It lists the items the class comes to, their cost against the request's, and which of
them a move would refuse — the person's pins, a system instruction, the turn the model is speaking
in — read off the same function the move consults, so a preview and the move it previews cannot
disagree.

`revise` rewrites what an item says. `note` writes something into the context — a plan, a
conclusion, a thing not to try again. A note is attributed to `agent`, so the chat line that records
it and `look` can say who put it there, and it can be pinned so compaction cannot take it. Saying
the same thing out loud in a turn is not a promise about anything; a pin is.

`undo` walks back — deliberately *not* the kernel's undo stack. That stack is yours, bound to
<kbd>u</kbd>, and the top of it while a tool is running is always the assistant turn that asked
for the call: one step would erase the model's own question and orphan the answer it is waiting
for. So the tool keeps a journal of what *it* did, and that is what it walks. A `reason` is
required by every one of the eight that change something, and it is what you read in the context
pane.

Three things are refused outright, with the refusal handed back to the model: a **pinned** item
(a pin is a promise, and it was not made to the model), a **system instruction**, and the
assistant turn it is currently speaking in. It may unpin what it pinned itself, and nothing else.

Reading the context and changing it were two tools once, on the argument that a tool declares its
capabilities for every call it will ever receive — so one tool would mean answering **always** to
"may it read its own context?" also answered "may it rewrite a tool result?", a grant that
delivers more than it implies. The hazard is real and it is no longer a reason for two tools: a
subject is `<domain>:<operation>`, a call declares which operation it is, and `context:look` and
`context:revise` are separate rows on the permissions tab whichever tool they arrive under. You
can answer them differently, and `--allow context` is how you answer for the lot.

**`fork`** went the other way and is its own tool, because it is neither a reading nor a change: it
stands up a copy of the session and pays a provider for an answer. Letting something read its own
items should not be letting it buy another request, which is why it is `fork:draft` and `fork:ask`
rather than two more operations on the context. Both take a snapshot, resume it as a second kernel
with **no tools**, ask it once, and hand back only what it said. `draft` is for reading your own
answer before you give it; `ask` is for asking whether a piece of context is what is leading you
astray, with `without` naming the items the copy is not given. The answer says what the copy was
asked, how many items it was given, which it could not read at all, and that none of it is in the
context: it is the model's to use or drop.

A model that asks a copy what it would conclude **without knowing my earlier statement** and passes
no `without` has asked the copy to pretend, which is not a test of anything, because the copy is
still reading the thing it is being told to disregard. A fork that took nothing away says so in as
many words — the same context answering again — so its answer is not mistaken for an ablation.

A fork can think; it cannot act, and it cannot go on thinking after it has answered once. Nothing
it does reaches this session's context or its log. Forking needed no change to the runtime at all
— `Kernel::snapshot` and `Kernel::resume` already *are* that, and leaving an item out is one field
on a copy of the snapshot.

**`log`** reads the other thing a session has, which is its own record. The context is what the
agent is carrying; the log sits beside it, costs nothing until something asks, and holds what a
context cannot — what an item *used* to say, which permissions were answered and how, which tools
appeared and went away. Called bare it hands back no records at all, only what there is: how many
records, what taking them all would cost, how many there are of each kind, and the last sequence
number. `take`, `ids`, `since` and `kinds` then ask for some of them — and **every answer opens
with the true total**, not the filtered one, before how many matched and how many are shown.

The total and the match count are separate questions, and a `take` on its own answers only the
second: it shortens what is *shown* without narrowing what counts, so it says how many of the most
recent it is showing and how many older ones are not here, rather than claiming that only those
matched.

That one rule is what makes the tool safe to hand a model. A short answer is self-describing, so
truncation cannot read as absence — which matters more here than anywhere else, because the one
wrong answer a log can give is *nothing happened*. It is why a malformed `since` comes back as
something to correct rather than as an empty list, and why a filter that genuinely matched nothing
says so in words and lists the kinds that do exist.

What it will not do is interpret. The records arrive in order, named the way the kernel names them
and detailed the way the trace pane details them — the same function writes both, so the account a
model reads and the account you read cannot drift apart. The count by kind covers every kind
rather than flagging an interesting one. A few `context.replaced` in it is a fact, and noticing
that it is an interesting fact is the model's job, not the tool's.

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
  reaches the model. Without it an agent cannot enumerate its own toolset, so a `shell` removed
  between two turns is something it can only keep asking for, or confabulate a reason for not
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

Any tool can be taken *away* mid-session with `/tools toggle ID`, and that is deliberate rather
than incidental. An agent whose ability to check the record is revoked half way through a run is a
thing this program can set up, and a thing worth watching a model in.
