# using kamchatka

The screen, the keys, the permission prompt, and the tools an agent uses to read and manage its own
session. [The README](README.md) says what the program is; this says how to use it, and
[RUNNING.md](RUNNING.md) covers running it without a screen, configuring it and embedding it.

---

## 👉 four tabs, one window

<kbd>ctrl+t</kbd> or <kbd>alt+tab</kbd> switches to the next tab, and <kbd>alt+1</kbd> …
<kbd>alt+4</kbd> go to one directly. The status line is shown under all of them, so the budget
is always visible. The prompt is on the chat tab, and on the context tab while an item is being
edited; otherwise the other three are for reading and operating rather than typing.
<kbd>tab</kbd> takes you back to the prompt from any of them.

![Four sessions side by side, one on each tab: the chat with a shell command waiting to be allowed,
the context with an item being edited, the trace of a session starting, and the permissions with
each rule's answer and what it covers.][shot-tabs]

The status line starts with the runtime state: `idle`, `asking`, `ready`, `running`, `waiting on
you`, or `done`. While it works, three dots move next to that word; after five seconds it shows how
long it has been. The dots stop if the program is stuck, and do not appear while it waits for
**you**.

Next is the model and its address, because the same model name at a different address is a different
model. A session started without `-m` shows `no model` there: nothing is sent until `/model` picks
one, and `/models` lists what the endpoint offers.

The budget shows the estimated cost of the next request, marked `~` because it is an estimate, as a
percentage of the model's limit with the limit beside it. It is green, then yellow from 70%, then
red from 90%. After a request is sent, the provider's actual count follows, then how much the
context holds back from the request.

**chat** is the conversation. It shows which parts are still being sent to the model, and shows each
turn as it currently is rather than as it originally arrived. Both come from the context, so if
<kbd>u</kbd> undoes an edit, the chat shows that too. A shell result starts with its exit status in
colour: **green** for success, **red** for failure, **yellow** if it never reported one (stopped,
killed, or unreadable).

**context** is the list of items the runtime holds, in order, one row each, with each item's cost,
whether it goes into the next request, and what the model will actually read of it.

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

The bottom line counts the items and how many are not sent in full, including elided ones; when the
request is over the limit, it says by how much.

A `+` after a figure in **sending** means part of the item could not be priced, so the figure is a
**minimum**.

For most rows **sending** is the whole item and **held** is blank. Where they differ: an elided
result costs only its placeholder, an excluded item costs nothing, and an assistant turn sends what
it said but holds back its reasoning when the endpoint does not accept reasoning back. The `sending`
column adds up to the context half of `/budget`'s estimate, with the tool definitions beside it. The
status line shows that estimate until the first answer; after that it starts from the provider's
count for the last request and adds what has changed since, and `/budget` shows both figures. An
excluded or elided row explains why in its note.

Pinning is most useful *before* compaction. `/compact` lists every item the compactor would remove
and waits for <kbd>y</kbd> or <kbd>n</kbd> in place of the prompt. It does not block the screen, so
while waiting you can switch here, <kbd>p</kbd> the items that should stay, go back and answer.
Answering yes recalculates the compaction and leaves the pinned items out.

An **elided** item keeps a one-line placeholder instead of its content, so the tool call it answers
still has an answer. Excluding a result would make the projector remove its call too. The
placeholder tells the model it already read the content, and to read it again, or only the part it
needs, if necessary.

Once the context is full, the compactor removes the oldest exchanges entirely (excluded, not
elided), and a summary at the end of the context says how many were removed. Each is still listed
here and can be restored with `/restore`. <kbd>p</kbd> protects a row, and the call it is paired
with, from every compaction.

<kbd>tab</kbd> moves the keys between the prompt and the table:

| key | what happens |
| --- | --- |
| <kbd>space</kbd> | cycle how much of it the model gets: all of it → a `…` placeholder → nothing → back |
| <kbd>p</kbd> | pin it, so the compactor can't remove it |
| <kbd>e</kbd> | edit what it **says**; a tool call isn't text, so a turn that's only a call can't be edited, and you're told why |
| <kbd>f</kbd> | show only what the next request sends whole, leaving out the elided as well as the excluded, or everything again |
| <kbd>y</kbd> | copy its text to the clipboard through the terminal (see below) |
| <kbd>/</kbd> | filter the rows: fuzzy, over the label, the kind and the full content (see below) |
| <kbd>enter</kbd> | open the whole item (see below) |
| <kbd>←</kbd> / <kbd>→</kbd> | switch between its pages, while it's open |
| <kbd>u</kbd> / <kbd>U</kbd> | undo / redo the last change to the context; `/undo` and `/redo` do the same, and are the only way when driving through a pipe or a browser |
| <kbd>23G</kbd> | go to item 23 (the number `/exclude` takes) |

**<kbd>y</kbd> copies an item's text in full and without line wrapping**, which selecting with the
mouse can't do; a turn's reasoning is left out, and a picture is a line naming it. `/copy` does the
same from the chat tab: the last thing the model said, or `/copy 7` for item 7. It uses
[OSC 52](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Operating-System-Commands), so
it works over `ssh`, but terminals that don't support it ignore it silently (`tmux` needs
`set-clipboard on`); the line it prints gives the byte count, so you can compare it with what you
paste.

`/copy` typed in the web page puts the text on the browser's clipboard. Some browsers only allow
that straight after a click, and then the page shows a **copy** button to click. Typed in
`--connect`, it goes to the clipboard of the terminal `--connect` runs in, the same way as above.
If a `/copy` from either of them has to wait behind a `/model` switch, `/models` or a compaction,
it isn't copied, and the session tells you to type it again.

An oversized tool result is kept as two items: the truncated copy the model saw, and the full output
next to it, marked `- excluded` and not sent. <kbd>space</kbd> or <kbd>p</kbd> on that row (or
`/restore` with its number) sends the full output; the row's token count is the cost.

<kbd>enter</kbd> opens the item in a box with pages; <kbd>←</kbd> / <kbd>→</kbd> switch between
them:

- **`to the model`** is what the item adds to the next request, based on the whole context. The
  projector drops a result whose call was removed, even though its row says `active`; that shows
  here, and the box opens on this page when that happens.
- **`as stored`** is the item's actual content, which differs for an elided or excluded item. If it
  has been rewritten, the page starts with who rewrote it last and why.
- **`v1`**, **`v2`** and so on are earlier versions from before it was edited, numbered from the
  oldest and shown newest first, up to eight, kept across `-r`.

<kbd>e</kbd> controls **what** the model reads of an item, while `space` and `p` control
whether it reads it. The prompt becomes an editor holding the item's text, and saving replaces it
in place: same number, same state, the old text kept as `v1`, and one <kbd>u</kbd> to undo. Cutting
a 2,000-line file down to the one function that matters is a few keystrokes.

**trace** is every event the runtime emits, as it happens, with the same names as in the session
log. Each is one line: the event's name and its details.

Every state machine transition is there, and each line shows the event's contents, not just its
name. It's the same stream `/save` writes to a `.jsonl` file, except for streamed output, which the
log doesn't keep.

There are two times on each line: when it happened (with a line across the pane when the date
changes), and the time since the line above, left blank under a tenth of a second, so the slow steps
stand out. Waits for *you* get no time. Streamed fragments don't get a line each: a tool's output is
one `tool.output` line whose byte count grows, and the model's text is on the chat tab. The pane
keeps the last few hundred lines; `/save` keeps every one the log has.

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
`/payload` goes further and prints exactly what the provider will send, except that an
attachment's base64 is replaced by its media type and size.

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
| <kbd>ctrl+t</kbd>, <kbd>alt+tab</kbd> | next tab; <kbd>alt+1</kbd> … <kbd>alt+4</kbd> for a specific one |
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

A message sent while a turn is running waits until the turn ends, then gets a turn of its own. It
cannot be sent sooner: it would land after the model's answer or between a tool call and its result.
If the turn stopped or failed, the message goes into the context and waits for `/continue`. While
waiting, <kbd>up</kbd> in an empty prompt brings it back to edit; otherwise <kbd>up</kbd> brings
back a copy of the last message sent, and <kbd>down</kbd> puts it away if you haven't changed it.

**<kbd>ctrl+l</kbd> removes `kamchatka`'s own notes from the chat** (the notes about what it just
did), and **the conversation stays**, because it is the context. The trace keeps everything either
way.

## 🔑 the permissions tab

The permission prompt asks about one call at a time, at the moment you're least inclined to think
about it. The **permissions** tab shows every answer in one place, where you can change them. Each
row is a capability or a path, the answer for it, and what that answer covers.

The line at the top shows the policy in force and its answer for everything not listed.

A fresh session has no rows: everything starts at `ask`, and rows are decisions added as you answer.
What is not listed is counted at the bottom as how many more it will ask about; setting a row back
to `ask` removes it. A rule for a whole domain is one row listing the actions it covers; an
operation answered separately (`--auto-allow fs --auto-deny fs:write`) gets its own row. What it
covers is never wider than the rule, and a rule that matches nothing says so. To decide in advance,
answer the first question with <kbd>a</kbd> to allow it from then on, or use a flag. <kbd>n</kbd>
refuses that one call, and a standing refusal is a row here or `--auto-deny`.

The line at the bottom starts with the shell, because it's the one thing on this tab you can't
negotiate with. A registered `shell` that isn't refused can read, write and use the network whatever
the other rows say, so `shell: confined` (or `partly confined`, or `a command can do any of these`)
is what makes the rest of the table meaningful. Where the shell is confined, after it comes
`network gated` or `network not gated`: whether a command is asked about when it opens a socket,
or judged by its name before it runs.

There are four kinds of rows, and the first two are the same thing at different levels of detail. A
**domain** is what a tool acts on (`fs`, `exec`, `context`), and an answer for a domain covers
everything done in it, which is what makes "always" work for tools `kamchatka` has never seen. An
**operation** is one kind of thing done in a domain, written `<domain>:<operation>`: `fs:read` and
`fs:write` are separate decisions, and so are `context:note`, which adds to your context, and
`context:revise`, which rewrites an item. An operation is named by what a call does, not by which of
a tool's actions it uses, so actions with the same effect are one decision:

| operation | the actions it covers |
| --- | --- |
| `fs:read` | `fs`'s `read`, `glob` and `grep` |
| `fs:write` | `fs`'s `write` and `edit` |
| `exec:run` | `shell` |
| `context:read` | `context`'s `look`, `budget`, `request` and `search` |
| `context:move` | `elide`, `exclude` and `restore`, which change what of an item is sent and never what it says |
| `context:pin` | `pin`, which keeps an item from the compactor |
| `context:note` | `note`, which adds an item of the model's own |
| `context:revise` | `revise`, which rewrites what an item says |
| `log:read` | `log` |
| `setup:read` | `setup`'s `model`, `tools`, `permissions` and `policy` |
| `fork:ask` | `fork`'s `draft` and `ask` |

`context`'s `undo` and `redo` can replay any change, so they need all of `context:move`,
`context:pin`, `context:note` and `context:revise`, and the strictest of those four applies to them:
with `context:move` allowed and `context:revise` refused, `undo` is refused. A refused `fs:write` also makes the working directory
and the `--sandbox-write` paths read-only for `shell`. A refused `fs:read` does not stop `shell`
reading, since a shell that can't read its working directory can do almost nothing; refuse
`exec:run` to keep commands away from the files. A **path rule** is about the file a call is given,
not about what the call does: with `fs:read` allowed, the default `.env*` rule still asks before
`.env` is read. It applies to every tool given a path: `grep` and `glob` can't ask, so they skip files a rule doesn't allow and say how
many, and a symlink to such a file is refused and named. A **server** rule is about which MCP server
a tool came from rather than what it does, which is the one thing about an MCP tool you don't have
to take the server's word for.

Two rules decide a call. **Within a domain**, an answer for an operation counts over an answer for
the whole domain: `--auto-allow context` allows everything in `context`; `--auto-allow
context:note` allows notes and says nothing about the rest; `--auto-allow context --auto-deny
context:revise` allows everything except that one. A deny wins at either level, so `--auto-deny
context --auto-allow context:read` still refuses `look`. **Across rules**, a call is judged by its
operation, by every path rule that matches the path it was given, and by the server it came from,
and the strictest answer wins: deny over ask, and ask over allow. So allowing a path rule does not
let a file through while its operation still asks: `--auto-allow '*.rs'` on its own still asks
before reading `main.rs`. What allowing a path rule can do is lift a question that another path
rule asks, such as `.env*`. `net:reach` is declared by no tool: where the network is
gated, a command is asked about when it opens an internet socket; where it is not, it is judged by
the command's name.

<kbd>space</kbd> cycles a row through **ask → allow → deny**, or use <kbd>a</kbd>/<kbd>n</kbd>/<kbd>r</kbd>
directly, and it takes effect from the next call. Answering "always" at a permission prompt writes
to this same table; the prompt and the tab are the same thing.

`allow` runs without asking. `deny` never runs and never asks, and both the transcript and the model
are told which rule refused it (``shell: refused by `net:reach`, which this command reaches for``)
and which kind of refusal it was: a standing rule, which will refuse the same call again, or an
answer to this specific call, after which a different approach might be allowed.

### the domains, and how a rule is written

These are all the domains, the tools that use them, and the operations in each:

| domain | used by | operations |
| --- | --- | --- |
| `fs` | the `fs` tool | `read`, `write` |
| `exec` | `shell` | `run` |
| `net` | `shell`, when a command uses the internet | `reach` |
| `context` | the `context` tool | `read`, `move`, `pin`, `note`, `revise` |
| `log` | the `log` tool | `read` |
| `setup` | the `setup` tool | `read` |
| `fork` | the `fork` tool | `ask` |
| `mcp` | every tool from an MCP server | `call` |

You write a rule the same way everywhere: after `--auto-allow` or `--auto-deny` (separate several
with commas), in the `auto-allow` and `auto-deny` lists of a settings file, and as a row on this
tab. You can copy a row from the tab into a flag or a settings file. The way a rule is written
says what kind it is:

- `context` and `context:*` both mean the whole domain;
- `context:note` is one operation;
- anything with a `*` or a `/`, or starting with a dot, is a path rule: `.env*`, `*.pem`,
  `vendor/` ([running it](RUNNING.md#-the-same-program-without-a-screen) explains how they match);
- an MCP server can't go in these lists, because its name could also be a domain's. Use
  `--auto-allow-servers` and `--auto-deny-servers` instead. On the tab, a server called `files`
  has the row `server files`.

A rule must use the names in the table above. If it names something that isn't there, the session
stops and says why. For example, `shell` is a tool, not a domain, so a rule about the shell is
written `exec:run`.

`mcp` is different from the other domains: to let an MCP server's tools run, allow the server by
name. Allowing `mcp` or `mcp:call` would do nothing, so the session stops on it too. Denying `mcp`
works, and refuses the tools of every server.

### the question itself

It appears in place of the prompt on the **chat** tab, not over the middle of the screen. It is
headed `a tool wants to run`, names the tool and everything the policy will judge the call by, shows
the arguments, and ends with the answers.

| key | what happens |
| --- | --- |
| <kbd>y</kbd> | run this call, once |
| <kbd>a</kbd> | always, for everything the question names |
| <kbd>n</kbd> / <kbd>esc</kbd> | no |
| <kbd>i</kbd> | the exact JSON, and the tool's own definition |
| <kbd>d</kbd> | drop every call it's waiting on, and tell the model why |

The question does not block anything: <kbd>ctrl+t</kbd> to the context tab, read the item it is
about, <kbd>alt+1</kbd> back, and answer. The **chat** tab turns red in the tab bar while a question
is waiting.

**It never takes the keys by itself.** Press <kbd>tab</kbd> (or come back to the chat tab) to give
the keys to it. Until then, none of the answer keys work, and neither does <kbd>enter</kbd>, so
typing meant for the prompt cannot answer it by accident. Whatever was in the prompt is restored
when the question is gone.

It lists everything the policy checked, not just what the tool declared, and <kbd>a</kbd> answers
for all of it, including calls already queued behind this one. <kbd>y</kbd> is for this call only.
Arguments too long for the box scroll with <kbd>pgup</kbd> and <kbd>pgdn</kbd> while the answers
stay in place.

A shell command is shown as code, wrapped at spaces, with `|`, `&&`, `||` and `;` between its stages
coloured, unless it cannot be parsed to the end (for example, an unclosed quote), in which case
nothing is highlighted. <kbd>i</kbd> shows it byte for byte. An `edit` is shown as a diff, `old` in
red and `new` in green.

With `--rate-shell-commands` (in a `shell-advisor` build), an extra line in the header shows the
advisor's rating of the command (green, yellow or red) and its confidence. It does not decide
anything. [RUNNING.md](RUNNING.md#what-the-colour-says) explains the ratings and what is sent.

### a command that reaches for the network

Where the network is gated, a command is not judged by its name. It runs, and when it opens an
internet socket (DNS lookups included), the call is paused and a question appears in place of the
prompt, headed `a command wants the network`, with the command below it:

| key | what happens |
| --- | --- |
| <kbd>y</kbd> | this command may use the network, until it finishes |
| <kbd>a</kbd> | always: `net:reach` is allowed from now on, including for commands already waiting |
| <kbd>n</kbd> | this command may not, and every socket it opens is refused |

It is asked once per command, and the command waits while the question is open; <kbd>esc</kbd> stops
the turn, the command and the question. The model is told near the top of the result that the
command tried to use the network and what you answered, because a refused lookup would otherwise
look like a broken network. `deny` refuses every internet socket without asking, including UDP, and
<kbd>a</kbd> also applies to other commands still running.

## 🔦 finding things without a shell

`fs`'s `grep` and `glob` let the model explore a repository without `exec:run`, which implies every
other capability: they need `fs:read`, as reading a file does, and the path rules that apply to
reading apply to them too. They use ripgrep's libraries.

A result starts with a line counting the matches, the files they are in, and the files searched, so
"the symbol isn't there" is distinguishable from "nothing was searched"; a `skipped:` line counts
what was not opened, by why: a path rule, a symlink outside the allowed paths, a binary file, an
unreadable one. It stops after a hundred matches, not at a byte count, and says so, and cuts lines
at two hundred characters. When it hits the limit, it suggests `files_only`, which lists the
matching files with their match counts, most first, at a fraction of the cost.

Files hidden by `.gitignore` are skipped, and `.git` always is; hidden files **are** searched. A
link to a file the session may read is searched; a link to a directory is not followed, and is
counted if it leads somewhere the search doesn't come to; a link outside the allowed paths, or to
nothing, is counted. The search path is checked the same way as `read`'s, so `grep` in `.env` asks
the same question as reading it would.

`glob` walks the same way: `**/*.rs` in, matching paths out, formatted like `ls -R`. Both are
deterministic, so two identical searches produce the same context item.

**A long file is read in parts, and `read` says where each part ends.** Past the output limit (see
below), it stops at the last whole line that fits, and its first line says which lines were returned
and the `from` value to continue with. A single line too long for the limit comes back as its start,
with `grep` or `shell` named as the way to the rest. `from` and `lines` read any part of a file,
including logs too large to hold in memory. The alternative, `sed -n` through `shell`, needs
`exec:run`, even for a file the session is already allowed to read.

**The output limit is set per permission**, so `fs:read` and `exec:run` each have their own. It
starts at 32,000 bytes, and at 8,000 for fixed-format reports rather than session content:
`fs:write`, `context:note` and `context:revise`. On a small model each limit is lower still, a tenth
of the context window at about four bytes a token, so one answer cannot fill it. A limit set with
`/limit` is kept whatever the window. `/limit` lists the limits numbered and changes one from its
next call on, e.g. `/limit fs:read 64000`. A cut result keeps the full version next to it, excluded;
<kbd>space</kbd> on it sends the full version instead. `fs read` and `grep` cut nothing: a read
stops at a line and says where to read on from, and a `grep` too long for its limit says which files
matched instead. The full version has its own limit, 8 MiB: beyond that, a command's extra output is
discarded and the result says how much, and `fs` refuses to edit larger files.

## 📎 putting something in, with or without a question

`/attach` takes a path followed by an optional question, so the file and the question are sent
together:

```text
/attach reports/q3.pdf what is the headline number, and what is it compared against?
```

How the file is added depends on its type. Anything `kamchatka` has no media type for is added as
**text**: counted, readable on the context tab and compactable. A PDF, an image, an audio file or an
mp4 is added as **bytes**, with its media type, based on the file extension; a file that's neither a
known type nor valid text is refused.

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

Four of the tools are about the session itself, and they are offered like the rest: `/tools toggle`
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
The twelve actions come under five permissions, in the table in the permissions section above:
`context:read` and `context:revise` are separate rows, and `--auto-allow context` covers all five.

**`fork`** is its own tool, because each use costs a request: `draft` and `ask`, both `fork:ask`,
take a snapshot, resume it as a second kernel with **no tools**, ask it once, and return only its
answer. `draft` checks your own answer before you give it; `ask` checks whether part of the context
is misleading you, with `without` naming the items the copy does not get. A fork that removed
nothing says so, so its answer is not mistaken for an ablation. Nothing a fork does affects this
session's context or log.

**`log`** reads the session's record: what an item used to say, which permissions were answered and
how, which tools were added and removed. With no arguments, it returns only counts: how many records
there are, what reading them would cost, how many of each kind. `take` (at most 64), `ids`, `since`
and `kinds` ask for specific records. **Every answer starts with the true total**, then how many
matched where `ids`, `since` or `kinds` narrowed them, and how many are shown, so a short answer
does not look as if nothing happened. It reports without interpreting. In a resumed session it reads
the whole record, from before the resume as well, as one log.

**`setup`** shows what the session is running *with*:

- **`model`**: which model, what parameters, how much context, and whether this conversation was
  **resumed from a snapshot**, which a model cannot tell from inside, because a restored context
  contains turns it did not produce.
- **`tools`**: every tool on offer, what each needs, and how much of its output reaches the model.
- **`permissions`**: what the policy allows, refuses or will ask about.
- **`policy`**: what the compactor and the projector will do to the context on their own.

A tool can be *removed* mid-session with `/tools toggle ID`, deliberately: watching how a model
behaves when it loses the ability to check its own record partway through a run is worthwhile.

[shot-tabs]: https://github.com/ljedrz/nachalnik/raw/HEAD/kamchatka/assets/tabs.jpg
