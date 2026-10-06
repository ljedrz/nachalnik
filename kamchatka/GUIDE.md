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
after five seconds how long it has been; they stop if the program is wedged, and are absent while
it waits on **you**.

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
of those are read off the context, so a <kbd>u</kbd> that takes an edit back takes it off here too.
A shell result opens with its exit status in colour: **green** for success, **red** for a failure,
**yellow** where it never got to report — stopped, killed, or unreadable.

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
| `-` | excluded | it does not go; the row says who took it out, and why |

The line along the bottom counts the items and how many of them are not having what they say
sent, the elided ones among them; past the limit it also says by how much the request is over.

A `+` after a figure in **sending** says part of that item could not be priced — a PDF, say — so
the figure is a **floor**.

For most rows **sending** is everything and **held** is blank. Where they differ is worth finding:
an elided result spends only its marker, an excluded item spends nothing, and an assistant turn
sends what it said while holding what it thought, where the endpoint takes no thinking back. The
`sending` column adds up to the status line's figure less the tool definitions (`/budget` gives the
two apart). An excluded or elided row says why, in its note.

A pin is worth most *before* a pass rather than after one, which is what `/compact` is for: it
lists every item the compactor would take and waits, in the prompt's place, for <kbd>y</kbd> or
<kbd>n</kbd>. The question is pinned rather than modal, so this tab is one keystroke away while it
stands: come here, <kbd>p</kbd> what should stay, go back and answer. Saying yes works the pass out
again, so what you just kept is not in it.

An **elided** item is the third answer between in and out: a one-line marker in place of what it
holds, so the call it answers still has an answer. Excluding a result would make the projector drop
its call too. The marker tells the model it had read what it stands in for, and to read it again,
or only the part it needs, if it still needs it.

Once the context is full the compactor takes the oldest exchanges whole, excluded rather than
elided, and a summary at the end of the context says how many have gone. Each is still a row
here, one `/restore` from coming back, and <kbd>p</kbd> keeps a row, and the call it is paired
with, through every pass.

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
| <kbd>u</kbd> / <kbd>U</kbd> | undo / redo the last change to the context — `/undo` and `/redo`, which is the only way in down a pipe or from a browser |
| <kbd>23G</kbd> | go to the item numbered 23 — the number `/exclude` takes |

**<kbd>y</kbd> copies an item whole and unwrapped**, which a mouse dragged over the screen cannot.
`/copy` is the same from the chat tab: the last thing the model said, or `/copy 7` for item 7. It
uses [OSC 52](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Operating-System-Commands),
so it works over `ssh`, but a terminal that does not support it drops it silently (`tmux` needs
`set-clipboard on`); the line it prints gives the byte count to compare against the paste.

An oversized tool result is held as *two* items: the truncated copy the model was shown, and the
whole of it beside it, marked `- excluded` and not going. <kbd>space</kbd> or <kbd>p</kbd> on that
row is how you say **send the whole thing**, as `/restore` with its number is, and the token count
in the row is what it will cost you.

<kbd>enter</kbd> opens the item in a paged box; <kbd>←</kbd> / <kbd>→</kbd> move between pages:

- **`to the model`** is what it puts into the next request, from the projection of the whole
  context — so a result whose call was taken out, which the projector drops though its row reads
  `active`, says so here, and the box opens on this page when that is the surprise.
- **`as stored`** is what the item holds, which for an elided or excluded one is not the same.
- **`v1`**, **`v2`** and so on are what it said before it was rewritten, newest first, up to eight
  deep and across a `-r`, with whose hand rewrote it.

<kbd>e</kbd> decides **what** the model reads of an item, where `space` and `p` decide whether. The
prompt becomes an editor holding the item's text, and committing rewrites it in place: same number,
same state, the old text under `v1`, and one <kbd>u</kbd> from coming back. Trimming a 2,000-line
file down to the function that matters is two keystrokes and a delete.

**trace** is every event the runtime emits, as it happens, in the same names the session log is
made of. Each is one line: when it happened, the gap since the line above, the event's name, and
what it carries.

Every transition of the state machine is in there, and everything either side of it, down to the
wiring, and each line says what the event carries rather than just naming it. It is the stream
`/save` writes to a `.jsonl`.

Two clocks: when it happened, with the date as a rule across the pane where it changes, and the gap
since the line above, blank under a tenth of a second — so what has a number beside it is the slow
step. A wait for *you* is given no gap. Streamed fragments get no line each: tool output is one
`tool.output` line whose byte count grows, and the model's text is on the chat tab. The pane keeps
the last few hundred lines; `/save` keeps all of them.

**<kbd>/</kbd> filters either of those two panes**, fuzzily (`mreq` finds `model.requested`). A
context row matches on everything the item holds, its label and its kind, so `tool_result` narrows
a long pane to the tool results; a trace row on its name, detail and clock. Inside the box,
<kbd>←</kbd> <kbd>→</kbd> <kbd>home</kbd> <kbd>end</kbd> edit the query and <kbd>↑</kbd>
<kbd>↓</kbd> still move between rows. <kbd>esc</kbd> closes and clears it, and so does changing
tabs.

And from anywhere, <kbd>ctrl+p</kbd> prints the request those items add up to — the kernel's own
rendering of it, not a description, under a header that counts the items in and out, names each
one the projector left out and why, and names each repair it had to make, such as a call dropped
because its result is not in the projection.

`/payload` goes one further and prints what the provider will put on the wire, field for field.

## ✍️ what the model writes

The chat tab renders a model's answer as markdown — headings, emphasis, lists, fenced code with a
rule down its left, and tables whose widest columns give way first when the window is narrow.
Nothing else on the screen is treated as markdown: a tool's output is what the tool said.

## ⌨️ the rest of the keys

| key | what happens |
| --- | --- |
| <kbd>enter</kbd> / <kbd>shift+enter</kbd> | send / a new line; <kbd>alt+enter</kbd> too, for a terminal that sends <kbd>shift+enter</kbd> as <kbd>enter</kbd> |
| <kbd>up</kbd> | in an empty prompt, the last message back: the one still waiting, or a copy of the last one sent |
| <kbd>down</kbd> | put a recalled line away again, while nothing has been typed over it |
| <kbd>ctrl+l</kbd> | take this program's own lines off the chat; the conversation stays |
| <kbd>pgup</kbd> / <kbd>pgdn</kbd> | scroll the conversation |
| <kbd>ctrl+home</kbd> / <kbd>ctrl+end</kbd> | the beginning of the conversation / the end of it |
| <kbd>home</kbd> / <kbd>end</kbd> | the prompt's own, as in any other line editor |
| <kbd>ctrl+e</kbd> | follow the newest again |
| <kbd>tab</kbd> | move the keys between the prompt and whatever else on the screen wants them; from a tab with no prompt, back to the chat |
| <kbd>ctrl+t</kbd> | the next tab; <kbd>alt+1</kbd> … <kbd>alt+4</kbd> for one in particular |
| <kbd>esc</kbd> | close an open search box; otherwise stop what is running, and keep what arrived — or, resting in `ready`, drop the calls waiting to run |
| <kbd>ctrl+c</kbd> | stop what is running either way, and again to leave |
| <kbd>ctrl+d</kbd> | leave, from anywhere — including a permission prompt, where <kbd>d</kbd> on its own means something else |
| <kbd>F1</kbd> | the keys, opened at the tab you are on; also <kbd>?</kbd> on any tab but the chat one |

**<kbd>F1</kbd> opens at the page for the tab you are on**, with a page each for the slash commands,
the keys that mean the same everywhere, and a waiting tool, if there is one.

Where you leave the conversation is where it stays while a turn goes on writing, and the line along
the bottom says how much has arrived underneath; <kbd>ctrl+e</kbd> or scrolling to the end follows
again. Nothing said is shortened to fit, except a command's output while it is still running.

Stopping is cooperative: the provider returns the text it has, and the shell tool kills the
command's process group and still answers the call. The partial turn ends up in the context like
any other.

A message sent while a turn is running **waits for the end of it**, then gets a turn of its own —
it cannot go in earlier, since it would land after the model's answer or between a call and its
result. While it waits, <kbd>up</kbd> in an empty prompt takes it back to change; otherwise
<kbd>up</kbd> brings back a copy of the last line sent, and <kbd>down</kbd> puts it away again
while it is unchanged.

**<kbd>ctrl+l</kbd> takes this program's own lines off the chat** — the notes about what it just
did — and **the conversation stays**, because it is the context. The trace keeps everything either
way.

## 🔑 the permissions tab

The other place the policy appears is the permission prompt — one call at a time, at the moment
you are least inclined to think about it. **permissions** is every answer you have given, in one
place, where it can be changed. Each row is a capability or a path, the answer standing for it, and
what that answer covers.

The line along the top is the policy in force and what it answers about everything the list does
not mention.

A fresh session has no rows: everything starts at `ask`, and rows are **decisions**, added as you
answer. What is not listed is counted along the bottom, as how many more it will ask about, and
cycling a row back to `ask` takes it off. A rule about a whole domain is one row, naming the
operations it covers; an operation answered separately — `--allow fs --deny fs:write` — is a row of
its own. **What it covers** is never wider than the rule, and a rule that reaches nothing says so.
Deciding in advance means answering the first question with <kbd>a</kbd> or <kbd>n</kbd>, or a flag.

The line along the bottom opens with the shell, because it is the one thing on this tab that is not
negotiable. A registered `shell` that is not refused can read, write and reach the network whatever
the other rows say — so `shell: confined` (or `partly confined`, or `a command can do any of these`)
is what makes the rest of the table mean anything. After it comes `network gated` or `network not
gated`: whether a command is asked about when it opens a socket, or read off its name before it
runs.

Four kinds of row, and the first two are one thing at two depths. A **domain** is what a tool acts
in — `fs`, `exec`, `context` — and answering for one answers for everything done in it, which is
what makes "always" work for tools this program has never heard of. An **operation** is one thing
done in a domain, spelled `<domain>:<operation>`: `fs:read` and `fs:write` are not the same
decision, and neither are `context:note`, which adds an item to your context, and
`context:revise`, which rewrites one. A **path rule** is finer than either — `fs:read: allow` is a
reasonable thing to want and `fs:read .env: allow` is not. It binds every tool handed a path:
`grep` and `glob` cannot ask, so they skip a file a rule does not allow and say how many, and a
link to such a file is refused and named. A **server** is about where a tool came from rather than
what it does — the one thing about an MCP tool nobody has to take the server's word for.

The most specific rule that has an answer decides, and a refusal above it overrules.
`--allow context` allows the lot; `--allow context:note` allows a note and says nothing about the
rest; `--allow context --deny context:revise` is everything but that one. What a finer rule cannot
do is overrule a refusal — a domain you have *denied* stays denied however finely an operation in
it is named, because the strictest of everything consulted wins and `--deny` is the last word.
`net:reach` is the one nothing declares: where the network is gated, a command is asked about the
moment it opens an internet socket; where it is not, it is a guess from the command's name.

<kbd>space</kbd> cycles a row through **ask → allow → deny**, or
<kbd>a</kbd>/<kbd>n</kbd>/<kbd>r</kbd> directly, and it takes effect on the next call. Answering
"always" at a permission prompt writes to this same table — the prompt and the tab are one object,
not two.

`allow` runs with no question. `deny` never runs and never asks, and the transcript and the model
are both told which rule did it — ``shell: refused by `net:reach`, which this command reaches
for`` — and which *kind* of refusal it was: a standing rule, which will refuse the same call
again, or an answer to this call, after which a different approach may be allowed.

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

**A question you cannot investigate is a question you cannot answer**, so it takes nothing away:
<kbd>ctrl+t</kbd> to the context tab, read the item it is about, <kbd>alt+1</kbd> back, answer. The
**chat** tab goes red on the strip while one is waiting.

**It never takes the keys by itself.** <kbd>tab</kbd> gives them to it — or coming back to the chat
tab — and until then none of the answers does anything, nor does <kbd>enter</kbd>, so a letter or
an <kbd>enter</kbd> meant for the prompt cannot answer it. Whatever was in the prompt is there again
when the question has gone.

It names **everything the policy consulted**, not just what the tool declared, and <kbd>a</kbd>
answers for all of it, calls already queued behind this one included. <kbd>y</kbd> is this call
only. Arguments longer than the box scroll with <kbd>pgup</kbd> and <kbd>pgdn</kbd> while the
answers stay put.

A shell command is drawn as code, wrapped at spaces, with the `|`, `&&`, `||` and `;` joining its
stages coloured — unless it cannot be read to the end, an unterminated quote say, when nothing is
picked out. <kbd>i</kbd> is the byte-exact view. An `edit` is drawn as a diff, `old` red and `new`
green.

With `--advise` (a `shell-advisor` build), one more line in the header says what the advisor reads
the command as — green, yellow or red — and how sure it was. It decides nothing.
[RUNNING.md](RUNNING.md#what-the-colour-says) has the rubric and what is sent out.

### a command that reaches for the network

Where the network is gated, a command is not asked about for what it is called. It runs, and the
moment it opens an internet socket — a DNS lookup counts — the call is held and the question stands
in the prompt's place, headed `a command wants the network`, with the command under it:

| key | what happens |
| --- | --- |
| <kbd>y</kbd> | this command may, for the rest of it |
| <kbd>a</kbd> | always: `net:reach` allowed from now on, and for every command already waiting |
| <kbd>n</kbd> | this command may not, and every socket it asks for is refused |

It is asked once per command, and the command waits while it is up; <kbd>esc</kbd> stops the turn,
command and question with it. The model is told near the top of the result that the command reached
for the network and what you answered, since a refused lookup otherwise reads like a broken network.
`deny` refuses every internet socket without asking, UDP too, and an <kbd>a</kbd> reaches the other
commands still running.

## 🔦 finding things without a shell

`fs`'s `grep` and `glob` let a session be asked about a repository without `exec:run`, which
subsumes every other capability: they declare `fs:grep` and `fs:glob`, and the path rules that bind
a read bind them too. Underneath is ripgrep's engine, linked in.

An answer opens with a line counting the matches, the files they are in and the files searched, so
"the symbol is not there" reads differently from "nothing was opened"; a `skipped:` line names what
was left unopened and why — a path rule, a link out of reach, a binary file, an unreadable one.
**It cuts at matches, not at bytes,** at a hundred, and says it stopped, and a line is cut at two
hundred characters. When the cap fills, it suggests `files_only`, which lists the files that
matched with how many each has, most first, at a fraction of the cost.

What a `.gitignore` hides is skipped, and `.git` always; hidden files **are** searched; a link is
followed inside the working directory and counted outside it. The path the call names is judged as
`read`'s is, so `grep` in `.env` is a question exactly as reading it is.

`glob` is the same walk: `**/*.rs` in, matching paths out in the shape `ls -R` prints them. Both are
deterministic, so two identical searches are the same context item.

**A long file is read in parts, and `read` says where each one ends.** Past the output limit —
32,000 bytes, unless `/limit fs:read` says otherwise — it stops at the last whole line that fits,
and its first line says which lines those are and the `from` to read on with. `from` and `lines`
read any part of a file, a log too large to hold in memory included. The other way to read a part
is `sed -n` through `shell`, which is `exec:run` again, for a file the session may already read.

**How much of a call's output the model is shown is a limit per subject**, so `fs:read` and
`fs:grep` have one each. It starts at 32,000 bytes, and at 8,000 for the tools whose answer is a
report of a fixed shape rather than a piece of the session. `/limit` lists them, numbered, and
changes one from its next call onward — `/limit fs:read 64000`. A result that was cut keeps its
whole beside it, excluded, and <kbd>space</kbd> on it sends the whole instead. The whole has a
ceiling of its own, 8 MiB: past it a command's output is let go and the result says how much, and
`fs` refuses to edit a larger file.

## 📎 putting something in, with or without a question

`/attach` takes a path and then whatever you want to ask about it, so the file and the question
go out as one request:

```text
/attach reports/q3.pdf what is the headline number, and what is it compared against?
```

What goes in depends on what the file is. Anything this program has no media type for goes in as
**text**, countable, readable on the context tab and compactable. A PDF, an image or a recording
goes in as **bytes**, with its media type, decided by the extension; a file that is neither a known
type nor valid text is refused.

`-f` at startup is the same thing, and with no question after the path `/attach` just puts the file
in. The difference is the pin: a file named on the command line is part of how the session was set
up, so `-f` pins it; one attached at the prompt is not, and <kbd>p</kbd> keeps it.

**`/note` is the same act with a message instead of a file.**

```text
/note the CI runner has no network; a test that fetches will hang there
```

A message starts a turn, so telling the model a fact it will need later costs a request and an
answer. A note goes into the context and stops there, carried by the next request. It arrives as a
**reference** labelled `note:`, the shape an attached file goes out in; it is from `memory`, so
`/exclude memories` names every note you have written, and it is not pinned.

Nothing here can price a picture, and it says so: the line the chat prints for an attachment gives
its media type, size, and how many pieces nothing could price, and its row carries a `+`. Once the
request has gone out, the provider's figure has it inside. `Kernel::set_counter` takes a counter
that knows your vendor's formula, and [`pricing_a_picture.rs`][pricing] is one written out.

[pricing]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/pricing_a_picture.rs

## 🔎 letting the agent read and manage its own context

Four of the tools are about the session itself, and they are offered like the rest: `/tools toggle`
takes one away or gives it back, and the `tools` key in a settings file says which a session starts
with. There are [write-ups](https://ljedrz.github.io/nachalnik/) of sessions using them.

**An argument the named action does not read is refused, not ignored**, in these and in `fs`, and
the refusal names the action it belongs to — a `read` given `old` would otherwise answer with the
whole file, to a call nobody made.

**`context`** reads. `look` lists every item — what it is, what it puts into the next request, what
it holds out of one and why — and reads any of them back, block by block, thinking included; a long
one comes back as its start and end unless `whole: true`, since reading copies it into the context.
`request` shows the request about to go out, and for each item left out whether its own state did
it (`restore` puts it back) or the projector dropped it as a consequence of something else.

`budget` is the one a decision gets made from: the estimate against the limit, split between the
context and the tool definitions, what is held back, what the last request really cost, and how far
the estimate is being corrected — then up to ten of the most expensive items going into the
request, ranked by what each *sends*, with a running total.

`search` reaches what is excluded without bringing it back: how many lines say the text, what
taking them would cost and which items they are in, and with `take` up to 64 of the lines. Case is
ignored, and a nil result says what it looked at.

The other operations change it. `elide`, `exclude`, `pin` and `restore` move items between the
states the <kbd>space</kbd> and <kbd>p</kbd> keys do, each named for the state it leaves. Items are
named by `ids` or by `select`, the selector language `/exclude` takes — one or the other, never
both — and `look` with a `select` previews which items a move would take and which it would refuse.
`revise` rewrites what an item says, never a message you wrote. `note` writes a plan, a conclusion
or a thing not to try again, attributed to `agent`, and can be pinned. `undo` and `redo` walk the
tool's own journal, not the kernel's undo stack, which is yours. Every change takes a `reason`,
which is what you read in the context pane.

Three things are refused outright: a **pinned** item (it may unpin only what it pinned itself), a
**system instruction**, and the turn it is speaking in. Each operation is its own subject, so
`context:look` and `context:revise` are separate rows on the permissions tab, and `--allow context`
answers for the lot.

**`fork`** is its own tool, because it buys a request: `fork:draft` and `fork:ask` take a snapshot,
resume it as a second kernel with **no tools**, ask it once, and hand back only what it said.
`draft` is for reading your own answer before giving it; `ask` is for asking whether a piece of
context is leading you astray, with `without` naming the items the copy is not given — and a fork
that took nothing away says so, so its answer is not mistaken for an ablation. Nothing it does
reaches this session's context or log.

**`log`** reads the session's own record: what an item *used* to say, which permissions were
answered and how, which tools came and went. Called bare it gives counts only — how many records,
what taking them would cost, how many of each kind. `take` (at most 64), `ids`, `since` and `kinds`
ask for some, and **every answer opens with the true total** before how many matched and how many
are shown, so a short answer can never read as *nothing happened*. It reports and does not
interpret.

**`setup`** reads what the session is running *with*:

- **`model`** — which model, what parameters, how much context, and whether this conversation was
  **resumed from a snapshot** — which a model cannot tell from inside, since a restored context
  carries turns it never produced.
- **`tools`** — every tool on offer, what each needs, and how much of its output reaches the model.
- **`permissions`** — what the policy allows, refuses or will ask about, read off its table.
- **`policy`** — what the compactor and the projector will do to the context unasked.

A tool can be taken *away* mid-session with `/tools toggle ID`, deliberately: an agent whose ability
to check its record is revoked halfway through a run is a thing worth watching a model in.
