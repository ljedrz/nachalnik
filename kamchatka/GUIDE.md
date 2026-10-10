# using kamchatka

The screen, the keys, the permission prompt, and the tools an agent uses to read and manage its own
session. [The README](README.md) says what the program is; this says how to use it, and
[RUNNING.md](RUNNING.md) covers running it without a screen, configuring it and embedding it.

---

## 👉 four tabs, one window

<kbd>ctrl+t</kbd> switches to the next tab, and <kbd>alt+1</kbd> … <kbd>alt+4</kbd> go to one
directly. The status line is shown under all of them, so the budget is always visible. The prompt
is on the chat tab, and on the context tab while an item is being edited; otherwise the other
three are for reading and operating rather than typing.
<kbd>tab</kbd> takes you back to the prompt from any of them.

![Four sessions side by side, one on each tab: the chat with a shell command waiting to be allowed,
the context with an item being edited, the trace of a session starting, and the permissions with
each rule's answer and what it covers.][shot-tabs]

The status line starts with what the runtime is doing: `idle`, `asking`, `ready`, `running`,
`waiting on you` or `done`. While it's working, three dots move next to that word, and after five
seconds it shows how long it's been; the dots stop if the program is stuck, and don't appear while
it's waiting for **you**.

Next is the model and its address, both, because the same model name at a different address is a
different model. A session started without `-m` has no model yet, and shows `no model` there: nothing
is sent until `/model` picks one, and `/models` lists what the endpoint offers.

After that is the budget: the estimated cost of the next request, marked `~` because it's an
estimate, as a percentage of the model's limit, with the limit next to it. It's green, then yellow
from 70%, then red from 90%. Once a request has been sent, the provider's actual count follows, and
then how much the context is holding back from the request.

**chat** is the conversation. Unlike most terminal agents, it also shows which parts are still
being sent to the model, and shows each turn as it currently is rather than as it originally
arrived. Both come from the context, so if <kbd>u</kbd> undoes an edit, the chat shows that too. A
shell result starts with its exit status in colour: **green** for success, **red** for failure,
**yellow** if it never reported one (stopped, killed, or unreadable).

**context** is the main point of `kamchatka`. It isn't a summary or a debug view: it's the list of
items the runtime holds, in order, one row each, with what each costs, whether it's going into the
next request, and, most importantly, what the model will actually read of it.

| column | what it says |
| --- | --- |
| **id** | the number `/exclude`, `/copy` and <kbd>G</kbd> use, then a mark for its state |
| **label** | the item's name: `user`, `assistant`, a tool's name, a file's |
| **kind** | `user_message`, `tool_result`, `reference` and so on; hidden below 84 columns |
| **sending** | what it adds to the next request |
| **held** | what it's keeping out of the request, blank if nothing |
| **what it says** | the first line the model will read, or why its content isn't sent |

| mark | state | what it means for the next request |
| --- | --- | --- |
| `·` | active | it's sent |
| `▪` | pinned | it's sent, and the compactor can't remove it |
| `…` | elided | a one-line placeholder is sent instead |
| `-` | excluded | it isn't sent; the row says who excluded it, and why |

The line at the bottom counts the items and how many of them aren't being sent in full, including
the elided ones; when the request is over the limit, it also says by how much.

A `+` after a figure in **sending** means part of the item couldn't be priced (a PDF, for example),
so the figure is a **minimum**.

For most rows **sending** is the whole item and **held** is blank. The interesting rows are where
they differ: an elided result only costs its placeholder, an excluded item costs nothing, and an
assistant turn sends what it said but holds back its reasoning when the endpoint doesn't accept
reasoning back. The `sending` column adds up to the context half of `/budget`'s estimate, which
puts the tool definitions beside it. The status line shows that estimate only until the first
answer: after it, the status line starts from the provider's count for the last request and adds
what has changed since, and `/budget` shows both figures. An excluded or elided row explains why
in its note.

Pinning is most useful *before* compaction, which is what `/compact` is for: it lists every item
the compactor would remove and waits for <kbd>y</kbd> or <kbd>n</kbd> in place of the prompt. The
question doesn't block the screen, so while it's waiting you can switch here, <kbd>p</kbd> the
items that should stay, go back and answer. Answering yes recalculates the compaction, so the items
you just pinned are left out of it.

An **elided** item sits between kept and removed: a one-line placeholder replaces its content, so
the tool call it answers still has an answer. Excluding a result instead would make the projector
remove its call too. The placeholder tells the model it already read the content, and to read it
again, or just the part it needs, if necessary.

Once the context is full, the compactor removes the oldest exchanges entirely (excluded, not
elided), and a summary at the end of the context says how many were removed. Each is still listed
here, one `/restore` away from coming back, and <kbd>p</kbd> protects a row, and the call it's
paired with, from every compaction.

<kbd>tab</kbd> moves the keys between the prompt and the table:

| key | what happens |
| --- | --- |
| <kbd>space</kbd> | cycle how much of it the model gets: all of it → a `…` placeholder → nothing → back |
| <kbd>p</kbd> | pin it, so the compactor can't remove it |
| <kbd>e</kbd> | edit what it **says**; a tool call isn't text, so a turn that's only a call can't be edited, and you're told why |
| <kbd>f</kbd> | show only what the next request sends whole, leaving out the elided as well as the excluded, or everything again |
| <kbd>y</kbd> | copy the whole content to the clipboard through the terminal (see below) |
| <kbd>/</kbd> | filter the rows: fuzzy, over the label, the kind and the full content (see below) |
| <kbd>enter</kbd> | open the whole item (see below) |
| <kbd>←</kbd> / <kbd>→</kbd> | switch between its pages, while it's open |
| <kbd>u</kbd> / <kbd>U</kbd> | undo / redo the last change to the context; `/undo` and `/redo` do the same, and are the only way when driving through a pipe or a browser |
| <kbd>23G</kbd> | go to item 23 (the number `/exclude` takes) |

**<kbd>y</kbd> copies an item in full and without line wrapping**, which selecting with the mouse
can't do. `/copy` does the same from the chat tab: the last thing the model said, or `/copy 7` for
item 7. It uses
[OSC 52](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Operating-System-Commands), so
it works over `ssh`, but terminals that don't support it ignore it silently (`tmux` needs
`set-clipboard on`); the line it prints gives the byte count, so you can compare it with what you
paste.

An oversized tool result is kept as *two* items: the truncated copy the model saw, and the full
output next to it, marked `- excluded` and not sent. <kbd>space</kbd> or <kbd>p</kbd> on that row
(or `/restore` with its number) sends the full output, and the row's token count is what that costs.

<kbd>enter</kbd> opens the item in a box with pages; <kbd>←</kbd> / <kbd>→</kbd> switch between
them:

- **`to the model`** is what it adds to the next request, based on the whole context, so a result
  whose call was removed (which the projector drops even though its row says `active`) shows that
  here, and the box opens on this page when that's the case.
- **`as stored`** is the item's actual content, which differs for an elided or excluded item. If
  it has been rewritten, the page starts with who rewrote it last and why.
- **`v1`**, **`v2`** and so on are earlier versions from before it was edited, newest first, up to
  eight, kept across `-r`.

<kbd>e</kbd> controls **what** the model reads of an item, while `space` and `p` control
whether it reads it. The prompt becomes an editor holding the item's text, and saving replaces it
in place: same number, same state, the old text kept as `v1`, and one <kbd>u</kbd> to undo. Cutting
a 2,000-line file down to the one function that matters is a few keystrokes.

**trace** is every event the runtime emits, as it happens, with the same names as in the session
log. Each is one line: when it happened, the time since the line above, the event's name, and its
details.

Every state machine transition is there, along with everything around it, and each line shows the
event's contents, not just its name. It's the same stream `/save` writes to a `.jsonl` file.

There are two times on each line: when it happened (with a line across the pane when the date
changes), and the time since the line above, left blank under a tenth of a second, so the slow
steps stand out. Waits for *you* get no time. Streamed fragments don't get a line each: a tool's
output is one `tool.output` line whose byte count grows, and the model's text is on the chat tab.
The pane keeps the last few hundred lines; `/save` keeps all of them.

**<kbd>/</kbd> filters either of those two panes**, fuzzily (`mreq` finds `model.requested`). On
the context tab it matches each item's full content, label and kind, so `tool_result` narrows a
long list to the tool results; on the trace, an event's name, details and time. In the filter box,
<kbd>←</kbd> <kbd>→</kbd> <kbd>home</kbd> <kbd>end</kbd> edit the search and <kbd>↑</kbd>
<kbd>↓</kbd> still move between rows. <kbd>esc</kbd> closes and clears it, and so does switching
tabs.

And from anywhere, <kbd>ctrl+p</kbd> prints the request all those items add up to, as the kernel
actually builds it, under a header that counts the items included and left out, names each one the
projector left out and why, and lists each fix it had to make, such as dropping a call because its
result isn't in the request.
`/payload` goes further and prints exactly what the provider will send.

## ✍️ what the model writes

The chat tab renders the model's answers as markdown: headings, emphasis, lists, code blocks with a
line down the left, and tables whose widest columns shrink first when the window is narrow. Nothing
else on the screen is treated as markdown: a tool's output is shown exactly as the tool returned it.

## ⌨️ the rest of the keys

| key | what happens |
| --- | --- |
| <kbd>enter</kbd> / <kbd>shift+enter</kbd> | send / new line; also <kbd>alt+enter</kbd>, for terminals that send <kbd>shift+enter</kbd> as <kbd>enter</kbd> |
| <kbd>up</kbd> | in an empty prompt, bring back the last message: the one still waiting to be sent, or a copy of the last one sent |
| <kbd>down</kbd> | put a recalled message away again, if you haven't changed it |
| <kbd>ctrl+l</kbd> | remove `kamchatka`'s own notes from the chat; the conversation stays |
| <kbd>pgup</kbd> / <kbd>pgdn</kbd> | scroll the conversation |
| <kbd>ctrl+home</kbd> / <kbd>ctrl+end</kbd> | jump to the start / end of the conversation |
| <kbd>home</kbd> / <kbd>end</kbd> | start / end of the prompt, as in any text field |
| <kbd>ctrl+e</kbd> | follow the newest output again |
| <kbd>tab</kbd> | move the keys between the prompt and whatever else on screen can use them; from a tab without a prompt, go back to the chat |
| <kbd>ctrl+t</kbd> | next tab; <kbd>alt+1</kbd> … <kbd>alt+4</kbd> for a specific one |
| <kbd>esc</kbd> | close an open search box; otherwise stop what's running and keep what arrived, or, when stopped in `ready`, drop the calls waiting to run |
| <kbd>ctrl+c</kbd> | stop what's running, or quit when nothing is |
| <kbd>ctrl+d</kbd> | quit, from anywhere, including a permission prompt, where <kbd>d</kbd> alone means something else |
| <kbd>F1</kbd> | key help, opened at the current tab; also <kbd>?</kbd> on any tab except chat |

**<kbd>F1</kbd> opens on the page for the current tab**, with separate pages for slash commands,
keys that work everywhere, and the waiting tool call, if there is one.

If you scroll away from the end while a turn is still writing, the view stays where you left it,
and the line at the bottom says how much has arrived below; <kbd>ctrl+e</kbd> or scrolling to the
end follows the output again. Nothing is shortened to fit, except a tool's output while it's
still arriving.

Stopping is cooperative: the provider returns the text it has so far, and the shell tool kills the
command's process group and still returns a result for the call. The partial turn goes into the
context like any other.

A message sent while a turn is running **waits until the turn ends**, then gets a turn of its own;
it can't be sent sooner, because it would land after the model's answer or between a tool call and
its result. While it's waiting, <kbd>up</kbd> in an empty prompt brings it back to edit; otherwise
<kbd>up</kbd> brings back a copy of the last message sent, and <kbd>down</kbd> puts it away again if
you haven't changed it.

**<kbd>ctrl+l</kbd> removes `kamchatka`'s own notes from the chat** (the notes about what it just
did), and **the conversation stays**, because it's the context. The trace keeps everything either
way.

## 🔑 the permissions tab

The permission prompt asks about one call at a time, at the moment you're least inclined to think
about it. The **permissions** tab shows every answer you've given in one place, where you can change
them. Each row is a capability or a path, the answer for it, and what that answer covers.

The line at the top shows the policy in force and its answer for everything not listed.

A fresh session has no rows: everything starts at `ask`, and rows are **decisions**, added as you
answer. What isn't listed is counted at the bottom, as how many more it will ask about, and
setting a row back to `ask` removes it. A rule for a whole domain is one row, listing the operations
it covers; an operation answered separately (`--allow fs --deny fs:write`) gets its own row. **What
it covers** is never wider than the rule, and a rule that matches nothing says so. To decide in
advance, answer the first question with <kbd>a</kbd>, which allows it from then on, or use a flag;
<kbd>n</kbd> refuses that one call, and a standing refusal is a row here or `--deny`.

The line at the bottom starts with the shell, because it's the one thing on this tab you can't
negotiate with. A registered `shell` that isn't refused can read, write and use the network whatever
the other rows say, so `shell: confined` (or `partly confined`, or `a command can do any of these`)
is what makes the rest of the table meaningful. Where the shell is confined, after it comes
`network gated` or `network not gated`: whether a command is asked about when it opens a socket,
or judged by its name before it runs.

There are four kinds of rows, and the first two are the same thing at different levels of detail.
A **domain** is what a tool acts on (`fs`, `exec`, `context`), and an answer for a domain covers
everything done in it, which is what makes "always" work for tools `kamchatka` has never seen. An
**operation** is one action in a domain, written `<domain>:<operation>`: `fs:read` and `fs:write`
are separate decisions, and so are `context:note`, which adds an item to your context, and
`context:revise`, which rewrites one. A **path rule** is more specific than either: `fs:read: allow`
is a reasonable thing to want, `fs:read .env: allow` is not. It applies to every tool given a path:
`grep` and `glob` can't ask, so they skip files a rule doesn't allow and say how many, and a
symlink to such a file is refused and named. A **server** rule is about which MCP server a tool
came from rather than what it does, which is the one thing about an MCP tool you don't have to
take the server's word for.

The most specific rule with an answer decides, but a refusal at a broader level always wins.
`--allow context` allows everything in `context`; `--allow context:note` allows notes and says
nothing about the rest; `--allow context --deny context:revise` allows everything except that one.
A more specific rule can't overrule a refusal: a domain you've *denied* stays denied, however
specifically you allow an operation in it, because the strictest matching rule wins and `--deny`
always takes precedence. `net:reach` is the one no tool declares: where the network is gated, a
command is asked about the moment it opens an internet socket; where it isn't, it's a guess based on
the command's name.

<kbd>space</kbd> cycles a row through **ask → allow → deny**, or use <kbd>a</kbd>/<kbd>n</kbd>/<kbd>r</kbd>
directly, and it takes effect from the next call. Answering "always" at a permission prompt writes
to this same table; the prompt and the tab are the same thing.

`allow` runs without asking. `deny` never runs and never asks, and both the transcript and the
model are told which rule refused it (``shell: refused by `net:reach`, which this command reaches
for``) and which *kind* of refusal it was: a standing rule, which will refuse the same call again,
or an answer to this specific call, after which a different approach might be allowed.

### the question itself

It appears in place of the prompt on the **chat** tab, rather than over the middle of the screen.
It's headed `a tool wants to run`, names the tool and everything the policy will judge the call by,
shows the arguments, and ends with the answers:

| key | what happens |
| --- | --- |
| <kbd>y</kbd> | run this call, once |
| <kbd>a</kbd> | always, for everything the question names |
| <kbd>n</kbd> / <kbd>esc</kbd> | no |
| <kbd>i</kbd> | the exact JSON, and the tool's own definition |
| <kbd>d</kbd> | drop every call it's waiting on, and tell the model why |

**You can investigate before answering**, because the question doesn't block anything:
<kbd>ctrl+t</kbd> to the context tab, read the item it's about, <kbd>alt+1</kbd> back, and answer.
The **chat** tab turns red in the tab bar while a question is waiting.

**It never takes the keys by itself.** Press <kbd>tab</kbd> (or come back to the chat tab) to give
them to it; until then, none of the answer keys work, and neither does <kbd>enter</kbd>, so typing
meant for the prompt can't answer it by accident. Whatever was in the prompt is restored when the
question is gone.

It lists **everything the policy checked**, not just what the tool declared, and <kbd>a</kbd>
answers for all of it, including calls already queued behind this one. <kbd>y</kbd> is for this call
only. Arguments too long for the box scroll with <kbd>pgup</kbd> and <kbd>pgdn</kbd> while the
answers stay in place.

A shell command is shown as code, wrapped at spaces, with the `|`, `&&`, `||` and `;` between its
stages coloured, unless it can't be parsed to the end (an unclosed quote, for example), in which
case nothing is highlighted. <kbd>i</kbd> shows it byte for byte. An `edit` is shown as a diff,
`old` in red and `new` in green.

With `--advise` (in a `shell-advisor` build), an extra line in the header shows the advisor's
rating of the command (green, yellow or red) and how confident it was. It doesn't decide anything.
[RUNNING.md](RUNNING.md#what-the-colour-says) explains the ratings and what is sent.

### a command that reaches for the network

Where the network is gated, a command isn't judged by its name. It runs, and the moment it opens an
internet socket (DNS lookups included), the call is paused and a question appears in place of the
prompt, headed `a command wants the network`, with the command below it:

| key | what happens |
| --- | --- |
| <kbd>y</kbd> | this command may use the network, until it finishes |
| <kbd>a</kbd> | always: `net:reach` is allowed from now on, including for commands already waiting |
| <kbd>n</kbd> | this command may not, and every socket it opens is refused |

It's asked once per command, and the command waits while the question is open; <kbd>esc</kbd>
stops the turn, along with the command and the question. The model is told near the top of the
result that the command tried to use the network and what you answered, since a refused lookup
would otherwise look like a broken network. `deny` refuses every internet socket without asking,
including UDP, and an <kbd>a</kbd> also applies to other commands still running.

## 🔦 finding things without a shell

`fs`'s `grep` and `glob` let the model explore a repository without `exec:run`, which implies every
other capability: they declare `fs:grep` and `fs:glob`, and the path rules that apply to reading
apply to them too. They use ripgrep's libraries.

A result starts with a line counting the matches, the files they're in, and the files searched, so
"the symbol isn't there" is distinguishable from "nothing was searched"; a `skipped:` line lists
what wasn't opened and why: a path rule, a symlink outside the allowed paths, a binary file, an
unreadable one. **It stops after a hundred matches, rather than at a byte count,** and says so, and
cuts lines at two hundred characters. When it hits the limit, it suggests `files_only`, which lists
the matching files with their match counts, most first, at a fraction of the cost.

Files hidden by `.gitignore` are skipped, and `.git` always is; hidden files **are** searched;
symlinks are followed inside the working directory and only counted outside it. The search path is
checked the same way as `read`'s, so `grep` in `.env` asks the same question as reading it would.

`glob` walks the same way: `**/*.rs` in, matching paths out, formatted like `ls -R`. Both are
deterministic, so two identical searches produce the same context item.

**A long file is read in parts, and `read` says where each part ends.** Past the output limit
(32,000 bytes, unless `/limit fs:read` says otherwise), it stops at the last whole line that fits,
and its first line says which lines were returned and the `from` value to continue with. `from` and
`lines` read any part of a file, including logs too large to hold in memory. The alternative,
`sed -n` through `shell`, needs `exec:run`, even for a file the session is already allowed to read.

**The output limit is set per permission**, so `fs:read` and `fs:grep` each have their own. It starts
at 32,000 bytes, and at 8,000 for tools whose result is a fixed-format report rather than part of
the session's content. `/limit` lists them, numbered, and changes one from its next call on, e.g.
`/limit fs:read 64000`. A result that was cut keeps the full version next to it, excluded, and
<kbd>space</kbd> on it sends the full version instead. `fs read` and `grep` cut nothing: a read
stops at a line and says where to read on from, and a `grep` too long for its limit says which
files matched instead. The full version has its own limit, 8 MiB:
beyond that, a command's extra output is discarded and the result says how much, and `fs` refuses
to edit larger files.

## 📎 putting something in, with or without a question

`/attach` takes a path followed by an optional question, so the file and the question are sent
together:

```text
/attach reports/q3.pdf what is the headline number, and what is it compared against?
```

How the file is added depends on its type. Anything `kamchatka` has no media type for is added as
**text**: counted, readable on the context tab and compactable. A PDF, an image or an audio file is
added as **bytes**, with its media type, based on the file extension; a file that's neither a known
type nor valid text is refused.

`-f` at startup does the same, and `/attach` with no question just adds the file. The difference is
pinning: a file given on the command line is part of the session's setup, so `-f` pins it; one
attached at the prompt isn't pinned, and <kbd>p</kbd> pins it.

**`/note` does the same with a message instead of a file.**

```text
/note the CI runner has no network; a test that fetches will hang there
```

A normal message starts a turn, so telling the model something it'll need later costs a request and
an answer. A note goes into the context without starting a turn, and is sent with the next request.
It's added as a **reference** labelled `note:`, like an attached file; its source is `memory`, so
`/exclude memories` excludes every note you've written, and it isn't pinned.

`kamchatka` can't price images, and says so: the line the chat prints for an attachment gives its
media type, its size, and how many parts couldn't be priced, and its row shows a `+`. Once the
request has been sent, the provider's figure includes it. `Kernel::set_counter` accepts a counter
that knows your vendor's pricing formula, and [`pricing_a_picture.rs`][pricing] has an example.

[pricing]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/examples/pricing_a_picture.rs

## 🔎 letting the agent read and manage its own context

Four of the tools are about the session itself, and they're offered like the rest: `/tools toggle`
removes or restores one, and the `tools` setting in a settings file says which a session starts
with. There are [write-ups](https://ljedrz.github.io/nachalnik/) of sessions using them.

**An argument the chosen action doesn't use is refused, not ignored**, in these tools and in `fs`,
and the refusal says which action it belongs to; otherwise a `read` given `old` would return the
whole file in answer to a call nobody meant to make.

**`context`** reads the context. `look` lists every item (what it is, what it adds to the next
request, what it holds back and why) and reads any of them in full, block by block, including
reasoning; a long item is shortened to its start and end unless `whole: true`, since reading it
copies it into the context. `request` shows the request about to be sent, and for each item left
out, whether its own state caused it (`restore` brings it back) or the projector removed it because
of something else.

`budget` is the one to make decisions from: the estimate against the limit, split between the
context and the tool definitions, what's held back, what the last request really cost, and how much
the estimate is being corrected; then up to ten of the most expensive items in the request, ranked
by what each *sends*, with a running total.

`search` looks through excluded items without restoring them: how many lines contain the text, what
taking them all would cost and which items they're in, and with `take`, up to 64 of the lines. It's
case-insensitive, and when nothing is found it says what it searched.

The other operations change the context. `elide`, `exclude`, `pin` and `restore` move items between
the same states as the <kbd>space</kbd> and <kbd>p</kbd> keys, each named after the state it moves
items into. Items are chosen by `ids` or by `select` (the selector language `/exclude` uses), one or
the other, never both, and `look` with a `select` previews which items a change would affect and
which it would refuse. `revise` rewrites what an item says, but never a message you wrote. `note`
writes down a plan, a conclusion or something not to try again, attributed to `agent`, and can pin
it. `undo` and `redo` go through the tool's own history, not the kernel's undo history, which is
yours. Every change takes a `reason`, which is what you see in the context tab.

Three things are always refused: changing a **pinned** item (the agent can only unpin items it
pinned itself), changing a **system instruction**, and changing the turn the agent is currently in.
Each operation is a separate permission, so `context:look` and `context:revise` are separate rows on
the permissions tab, and `--allow context` covers all of them.

**`fork`** is its own tool, because each use costs a request: `fork:draft` and `fork:ask` take a
snapshot, resume it as a second kernel with **no tools**, ask it once, and return only its answer.
`draft` is for checking your own answer before giving it; `ask` is for checking whether part of the
context is misleading you, with `without` naming the items the copy doesn't get. A fork that removed
nothing says so, so its answer isn't mistaken for an ablation. Nothing a fork does affects this
session's context or log.

**`log`** reads the session's own record: what an item *used* to say, which permissions were
answered and how, which tools were added and removed. Called with no arguments, it returns only
counts: how many records there are, what reading them would cost, how many of each kind. `take` (at
most 64), `ids`, `since` and `kinds` ask for specific records, and **every answer starts with the
true total**, then how many matched where `ids`, `since` or `kinds` narrowed them, and how many
are shown, so a short answer never looks like *nothing happened*. It reports, without interpreting.

**`setup`** shows what the session is running *with*:

- **`model`**: which model, what parameters, how much context, and whether this conversation was
  **resumed from a snapshot**, which a model can't tell from inside, since a restored context
  contains turns it didn't produce.
- **`tools`**: every tool on offer, what each needs, and how much of its output reaches the model.
- **`permissions`**: what the policy allows, refuses or will ask about.
- **`policy`**: what the compactor and the projector will do to the context on their own.

A tool can be *removed* mid-session with `/tools toggle ID`, deliberately: watching how a model
behaves when it loses the ability to check its own record partway through a run is worthwhile.

[shot-tabs]: https://github.com/ljedrz/nachalnik/raw/HEAD/kamchatka/assets/tabs.jpg
