# running kamchatka

Running without a screen, choosing an endpoint, configuration, embedding, and what the number in
the status line means. [The README](README.md) says what the program is, and
[GUIDE.md](GUIDE.md) covers the screen and the keys.

---

## 🤖 the same program, without a screen

`--headless` drives the session from lines on stdin instead of from keys. Each line is treated like
a line typed at the prompt: a message, or a command if it starts with `/`. It's turned on
automatically when stdout isn't a terminal, and says so.

```console
$ printf 'what is 2+2? answer with just the number\n/budget\n' \
    | kamchatka --headless -m qwen/qwen3-coder > session.jsonl
```

**stdout is the session log**, one JSON record per line, the same as `/save` writes, so reading a
run back is as simple as `jq 'select(.event.event == "tool.requested")' session.jsonl`. **stderr is
for people**: what the model said, what tools were asked to do, and what your commands answered.
Every key that does something has a command equivalent (`/undo`, `/redo`, `/cleanup`, `/continue`),
which is how to do it through a pipe.

Since there's no prompt to ask questions at, answers are given in advance:

| flag | what it does |
| --- | --- |
| `--allow fs,exec:run` | answer `allow` for a whole domain, one operation (`--allow context:note`) or a path rule (`--allow '*.rs'`, `--allow 'vendor/'`) |
| `--deny fs:write,.env*` | the same, refused; the strictest matching rule still wins |
| `--on-ask deny` | what to do with questions not answered in advance; `deny` is the default |

A path rule is a file name where `*` matches any characters, a name starting with a dot such as
`.env`, or a directory name followed by a slash, which matches that directory anywhere in a path.
It's matched against the *last* name in a path, so a rule about a file is just that file's name:
`--deny 'b.txt*'` is a rule about `b.txt`, but `--deny b.txt` isn't, because a bare name is read as
a domain. It isn't the glob syntax `fs`'s `glob` argument uses, and a pattern that looks like one
(`src/**`) stops the session with an explanation of how to write the rule.

`--on-ask deny` is the default because an unattended run shouldn't be able to do anything nobody
allowed. The model is told the refusal was for *this call* rather than a standing rule, so it tries
another way rather than retrying.

MCP servers are allowed the same way, and which server a tool came from is a separate permission.
`--mcp files=… --allow-server files` is all it takes to allow one server: the same rule the
permissions tab records when someone answers **always** at the prompt. Without it, the tools are
offered but every call is refused, because naming a server on the command line doesn't mean
trusting it to run.

An unattended run needs to know when to stop. `--deadline 300` interrupts whatever is running and
exits normally: what arrived is kept and the session is saved, which wouldn't happen if the process
were killed. <kbd>ctrl+c</kbd> does the same; pressing it again exits immediately. `--deadline 0`
means no deadline, as `0` means no limit for `--spend` and `--requests`.

`--spend 50000` also stops a run, because a model stuck in a loop will stay within any deadline you
were willing to give it. The unit is tokens (`input + output`, as the provider reports them,
including `fork`s); there are no prices here. It's a stopping point rather than a hard cap, since a
response's cost is only known once it arrives. When the session stops, the remaining lines are still
read: commands run, and messages are skipped without being sent. `/spend` shows what has been spent
and against what limit, `/spend N` raises the limit, and `/spend 0` removes it. It belongs to the
session, so the screen, a piped script and [an embedding program](#-embedding-it) all use the same
limit.

The total starts from zero in every process. `-r` carries over a session's context and what its
token counter learned, but not what it spent, since the snapshot is the runtime's record and this
figure is `kamchatka`'s. So a limit given to a resumed run applies to that run, not the whole
session since it began, and `/restart` starts a session with a fresh total.

An endpoint that reports no usage at all is mentioned once, instead of silently keeping a limit that
can never be reached, since someone who set a limit expects the run to be limited.

`--deadline` counts from program start, so it also covers an endpoint that never answers and an MCP
server that never finishes starting, and `/restart` doesn't reset it. It can't interrupt a `/model`
or `/endpoint` switch, which always finishes first.

A busy endpoint (a `429`, a `503`, a connection that timed out) is asked again after waits that double
from two seconds. A session with a screen gives up after four tries, because someone watching would
rather be told; a headless run tries ten times, with the waits capped at a minute, which is about
five minutes in all. An endpoint that asks to be left for longer than a minute is reported at once
either way, because that is a spent quota rather than a busy server.

A background job started by a command (`sleep 300 &`, a server, or something detached with
`setsid`) outlives its call, and is stopped when the session ends: `SIGTERM`, then `SIGKILL` two
seconds later for anything still running, with a line naming the command each came from. They're
found through `KAMCHATKA_CALL`, an environment variable every command runs with and passes on, so
anything that clears its environment (`env -i`) isn't found. `--leave-running` leaves them running,
for servers you started on purpose, and still lists them.

A run stopped by one of these says so on stderr when it happens, and exits with its own status, so
a script can tell a completed session from a stopped one:

| status | the run |
| --- | --- |
| `0` | finished its input |
| `1` | failed: the last turn couldn't be finished, or the program couldn't start |
| `3` | reached the `--spend` limit |
| `4` | ended on a turn paused at `--requests`, waiting for `/continue` |
| `5` | finished its input, but couldn't save its record |
| `124` | ran out of `--deadline`, whether or not the session had started (as `timeout` does) |
| `129`, `130`, `143` | ended by `SIGHUP`, <kbd>ctrl+c</kbd> or `SIGTERM`: `128` plus the signal |

If more than one applies, the first is reported. A served session without a screen exits the same
way when a signal ends it.

Lines are only read while the runtime is idle, so a script's lines can't get ahead of the turns
they belong to. `--no-default-features --features mcp` builds the program without the screen.

## 🔌 a session you can walk away from

`--serve` makes a session available over a socket, and `--connect` attaches to one. The screen is
still drawn where there's a terminal, so a session started at your desk is the same one a browser
or another client picks up: the keys and the clients both control the same session.

```console
$ kamchatka --serve unix:/run/user/1000/kamchatka.sock -m qwen/qwen3-coder
```

```console
$ printf 'what is 2+2? answer with just the number\n' \
    | kamchatka --connect unix:/run/user/1000/kamchatka.sock > session.jsonl
```

**The session belongs to the program running it, not to whoever is attached.** A turn continues
with nobody watching, a question waits for someone to come back and answer it, and a client
attaching an hour later gets the same session. When a client's input closes, it detaches; it never
ends the session, and `/quit` is how you end it on purpose.

`--connect` writes the same output as `--headless`, so it can replace it in a script, and only
sends each piped line once the previous turn is over. It answers permission questions with the same
three letters as the screen, `y`, `n` and `a`; `ctrl+c` stops the turn and a second one detaches;
and `?4` prints the actual content of item 4, which the record stream can't show, because
[the log refers to items rather than copying them](#-a-session-on-disk). A running command that
[tries to use the network](#-the-network-when-a-command-tries) is answered with the same three
letters, after any of the kernel's own questions.

A question still open when a client's input closes is left for someone else: the next client to
attach, the screen at the desk, or the same client coming back; the script lines still waiting are
not sent, and it says which. A script that drives a session alone should pass `--on-ask deny` or
`--on-ask allow` to `--connect`; a settings file's `on-ask` isn't used there.

**There's no authentication, so it only listens where only you can reach it.** The protocol can
use the `shell` tool, so reaching the session means being able to run commands on the machine. A
unix socket is made `0600` as soon as it's created, and `tcp:127.0.0.1:PORT` is only reachable from
this machine; anything else (`--serve tcp:0.0.0.0:7878`, for example) is refused. From another
machine, use a tunnel that authenticates:

```console
host$  kamchatka --serve tcp:127.0.0.1:7878 -m qwen/qwen3-coder
other$ ssh -N -L 7878:127.0.0.1:7878 host &
other$ kamchatka --connect tcp:127.0.0.1:7878
```

A dropped TCP connection is retried for a minute, with increasing delays.

**A session serves one client at a time, and the newest one wins.** A client that attaches takes
over the session, and the previous one is told it was replaced and disconnected: `--connect` exits
with a message, and the browser page stops reconnecting; attaching again takes it back. The newest
wins because usually the second connection is the same client reconnecting while the session still
holds the old connection.

There's no design yet for several people controlling one agent, so it's one client at a time. A
served session with a screen still has two ways in: both can send messages, interrupt and answer
questions, and messages sent during a running turn from either wait in the same queue, each getting
its own turn, in the order sent.

**Commands that contact the endpoint** (`/models`, `/compact`, `/model`, `/endpoint`) run while the
session continues, and the next line waits until they finish. `ctrl+c` stops a listing or a
compaction; a model or endpoint switch always finishes.

The protocol is newline-delimited JSON, so `nc` and `jq` can read it, and
[`remote`](https://docs.rs/kamchatka/latest/kamchatka/remote/)'s documentation describes it.

`examples/attached.rs` is an example client for a served session: it attaches, asks a question,
follows the turn, refuses a permission question, and reads back the item the answer was saved as:

```console
$ kamchatka --serve tcp:127.0.0.1:7878 -m qwen/qwen3-coder &
$ cargo run --example attached -- tcp:127.0.0.1:7878 "what is 2+2"
```

Its header comment has a transcript of the messages, for writing a client in another language.

`--web` serves the session as a web page, for browsers, which can't connect to the socket
themselves:

```console
$ kamchatka --web 127.0.0.1:8080 -m qwen/qwen3-coder
```

With `--serve`, the page relays to that socket (`unix:` or `tcp:`); without it, the session is
served on a loopback port of its own, which is printed like `--serve`'s. The screen is still drawn
where there is a terminal, and a browser that loses the connection reconnects and catches up by
itself. This is the `webui` feature, which release builds include.

The page has no authentication or encryption. It listens on loopback, or on this machine's IP
address on a private network (10/8, 172.16/12, 192.168/16, fc00::/7):

```console
$ kamchatka --web 192.168.1.5:8080 -m qwen/qwen3-coder
```

A phone on the same network can then open `http://192.168.1.5:8080/`, but so can every other device
on it: any of them can control the session, including running shell commands as you, and read its
traffic. kamchatka warns about this at startup and in each session. Host names, `0.0.0.0` and public
addresses are refused. On a network you don't trust, tunnel over SSH instead and open the page at
`localhost`:

```console
phone$ ssh -N -L 8080:127.0.0.1:8080 host
```

then `http://localhost:8080/`. The page refuses requests from other web pages open in the same
browser: a request must be addressed to an IP address or `localhost` (not a host name, which a DNS
rebinding attack would use), must come from the page itself when it says where it came from, and a
command must be JSON. The host name rule also means a tunnel that forwards its own host name, such
as `tailscale serve`, is refused. The page can answer permission questions, so its port is closed
to the session's own commands, like the session's port.

The page has the same four tabs as the terminal, in the same order: the button in the top right
corner cycles **chat → context → events → permissions**.

![The page on a phone, one tab per panel: the chat with the model's reasoning and its answer, the
context with each item's state button, the events with the time each step took, and the permissions
with allow, ask and deny for each rule.][shot-web]

The **context** view shows the same rows as the context tab: what each item is, its estimated cost,
and what the next request will do with it. Tapping a row loads the whole item, since the session
only sends item summaries; the button on the right of a row shows its state, and tapping it moves
to the next one (active, elided, excluded, active again), like <kbd>space</kbd> on the context tab,
note included. The **chat** updates with it, as in the terminal. The **events** view is the trace,
with the time each step took; the **permissions** view lists the rules, each with **allow**,
**ask** and **deny**, and **ask** undoes an `always`.

The prompt is only on the chat. A waiting question colours the tab button, and a mark in the top
left blinks while a turn is running. The model's answers are rendered as markdown, never as raw
HTML, and links only work for `http`, `https` and `mailto`. Answers stream in as they're written and
are then replaced by the saved version, as in the terminal.

`/cleanup` (<kbd>ctrl+l</kbd>) removes `kamchatka`'s own notes from the chat and keeps the
conversation; clearing it on one client clears it on all of them.

`examples/gateway.rs` serves the same page from a separate process, for a session started without
`--web` or by a build without the `webui` feature:

```console
$ kamchatka --serve tcp:127.0.0.1:7878 -m qwen/qwen3-coder &
$ cargo run --example gateway -- tcp:127.0.0.1:7878 127.0.0.1:8080
```

It has the same address rules, but it can't close its port to the session's commands, since
those are confined by another process; see [SECURITY.md](../SECURITY.md).

## 🧩 three dialects, and why the others keep the order

`--gemini` uses Google's own API instead of an OpenAI-compatible one, `--anthropic` uses
Anthropic's Messages API (OpenRouter supports it too, at `/api/v1/messages`), and `--responses`
uses OpenAI's Responses API, at the same address and with the same key as chat completions. All
three return a turn's parts in the order they were produced (thinking, a sentence, a tool call),
whereas chat completions squashes a turn into a string next to a list of calls. Here the order is
kept, and the parts are counted, pruned and elided like any other content; the context tab and the
`context` tool show them in order, and each part is sent back with the signature or encrypted
reasoning it came with, without which the next request is rejected. Under `--responses`,
instructions added mid-session stay where they were added, so the cached start of the prompt
doesn't change.

Every provider implements one trait, `Dialect`, so `/model`, `/models`, `/endpoint` and the status
line work the same with all of them.

```console
$ export KAMCHATKA_API_KEY=...        # a Google AI Studio key
$ kamchatka --gemini "what does src/kernel.rs do?"
$ export ANTHROPIC_API_KEY=...        # an Anthropic Console key
$ kamchatka --anthropic -m claude-haiku-4-5 "what does src/kernel.rs do?"
$ export KAMCHATKA_BASE_URL=https://api.openai.com/v1 KAMCHATKA_API_KEY=...   # an OpenAI key
$ kamchatka --responses -m gpt-5-mini "what does src/kernel.rs do?"
```

## 🔀 the model, and the address it lives at

`/model` shows which model the session uses, its address, its API, and its context size.

`/model ID` switches the model, and `/endpoint URL [ID]` switches the address, and the model with
it, since model names depend on the endpoint. Without a model id, the old name is kept and the new
endpoint is checked for a model by that name, so you get a notice immediately instead of a 404 on
the next request.

`/models [FILTER]` lists the models the endpoint offers, under the ids it uses (they differ between
endpoints), and marks the current one with `▸`.

The API key *isn't* switched with the address. It's read from the environment once, at startup,
because a key typed at a prompt would end up in the transcript. So `/endpoint` is for addresses
that need no key or the same key: a local model, a proxy, another base URL on the same account.

The context isn't switched either: the same items go to whatever model answers next, which is what
makes answers comparable. `/seams` lists the runtime's replaceable components and what each is
currently set to.

`/params KEY JSON` sets a model parameter, `/params KEY null` removes it, and `/params` lists them,
along with the other parameters the model accepts, where the endpoint publishes them, because a
parameter the model doesn't accept is sent and silently ignored. A parameter you set that isn't in
the model's listing is marked as ignored, and a value above a published maximum is noted. Only the
parameters you set are sent, plus anything an API requires; under `--anthropic`, that includes
prompt caching, which `/params cache_control false` turns off for endpoints that reject the field.

Parameters named after parts of the request itself (`messages`, `tools`, `model` and similar, in
each API) are refused.

## 📏 the number in the status line is an estimate, and says which kind

`kamchatka` doesn't have the model's tokenizer, so the main figure in the status line is an
estimate, shown as `~2,460`. The percentage next to it says what total it's a percentage of
(`1.9% (128k)`), since a percentage of an unknown number isn't useful, and it turns yellow above
70% and red above 90%. After that comes what the provider actually charged for the last request.
`/budget` shows all of it in detail: the next request, split into context and tool definitions; the
same request based on the last response; the limit and how much of it the next request would use;
what the last request really cost, as the provider counted it; and what the counter has learned.

Once there has been a response, the status line shows the response-based figure: what the provider
charged for the last request, plus an estimate of only what has changed since. Unchanged items
contribute their measured cost exactly, so the error is a few percent of the change rather than of
the whole context, and per-message overhead, tool schemas and anything the counter can't price are
already included in the provider's figure.

The counter adjusts itself from every response to a request it could fully price, and from every
"too long" refusal that states the limit, which is even more useful, since it measures the request
in the same units the limit uses. `/budget` shows how many requests it learned from and by what
factor it adjusts.

Before the first response, on endpoints that don't report usage, and after switching models until
the next answer, the status line shows the plain estimate. When something in the context can't be
priced at all, `/budget` says how many parts; a figure that is only a minimum is never shown as
complete.

`/budget` is about the next request; `/spend` is about the session. It adds up what the provider
charged for every response (measured, never estimated) and compares it with the limit, if one was
set with `--spend` or `/spend N`. The session stops at that limit; see
[running without a screen](#-the-same-program-without-a-screen). The `--advise` advisor's reported
costs are included too, whichever key pays for them, and `/spend` shows how much of the total was
the advisor's.

## 🗜️ when the context fills

The compactor removes what the conversation no longer needs, using two rules. The first runs
before every request: a tool result from a finished turn is replaced by a placeholder, as is an
image or document the model was shown in a finished exchange, unless the placeholder would be
bigger than the original. The second only applies when space runs out: once the context goes past
the `--compact` threshold (0.8 of the limit by default), the oldest exchanges are removed entirely
until it gets down to the target, the second number in `--compact 0.8,0.6`. If the target isn't
given, it's twenty percentage points below the threshold or half of it, whichever is lower, so
removals happen in batches instead of changing the cached start of the request every turn.

Neither rule touches the turn in progress, pinned items, or notes the model wrote for itself, and
neither summarizes anything. Every item is still on the context tab: an elided one is a
<kbd>space</kbd> away from coming back, a removed one a `/restore` away. A result you bring back
by hand isn't elided again by the first rule. The placeholder says the content has been read, and
content the model hasn't seen yet is never removed to make a request fit.

`/compact` runs the same compactor manually, and shows what it would do before doing anything:
every item it would remove, with its id, what it is and what it contains. It then waits for
<kbd>y</kbd> or <kbd>n</kbd> in place of the prompt. Like a tool's question, it doesn't block the
screen, so you can switch to the context tab and <kbd>p</kbd> anything you want to keep. Answering
yes recalculates the compaction, so items you pinned while reading the list are respected.

The compactor doesn't touch the turn in progress or pinned items, so if those make up most of the
context, it can't help. When the context is past the `--compact` threshold and there's nothing left
it's allowed to remove, the session says the context is full, once, while there's still room below
the limit; what's left is for you to `/exclude`, or for the model to exclude with its `context`
tool. The model is told too, by a note added to the context before its next request, within the
turn that filled it, so an unattended run can make room itself instead of hitting the limit. If the
model can't make room with the `context` tool (it's turned off, a rule refuses `exclude` and
`elide`, or a headless run would refuse them because nobody is there to ask), the note asks the
model to tell you instead, and it's reworded, including any copy already in the context, whenever
that changes. Once there's room again, the session says so and the note is excluded.

In a headless run, making room is a permission question like any other: every `context` call is
asked about, including `look`, and `--on-ask deny` refuses all of them, so the note doesn't send
the model somewhere it can't go. `--allow context` lets it make room, or
`--allow context:look,context:exclude,context:elide` for a run that should free space and change
nothing else.

The same applies to every message that points the model at a tool: a refusal from `fs` mentions
`shell`, `grep` or `write`, and an answer from `setup`, `log` or `fork` mentions `context` or `log`,
only if the model could actually make that call right now (the tool is offered, and the call isn't
refused by a rule or by a question nobody is there to answer).

A request over the limit isn't sent at all: the runtime refuses it instead of waiting for the
endpoint to say what the status line already shows, and prints how much needs to be removed. The
context tab shows the same figure while you decide which rows to remove. The refusal doesn't claim
the model saw the request, because it didn't; a refusal from `kamchatka` calls its figure an
estimate, and only a refusal from the endpoint quotes an actual count.

`--send-oversized` sends it anyway, since the figure is an estimate and the limit is whatever the
endpoint advertised, which can be lower than its real context window.

With no keys in a pipe, `--headless` prints the `/compact` list to stderr and goes ahead: a line the
operator typed doesn't need confirming.

`/compact` is also how to recover a session too big to send. The tool that prunes the context
belongs to the *model* (`context`), and using it costs a request, which is exactly what's failing.

In a pipe, once a turn has been refused for this and as long as the next request would be too,
the following messages are skipped without being sent, as they are past a `--spend` limit: keeping
them would make the request even longer, and they'd all be sent together once there was room.
Commands are still read, so an `/exclude` or `/limit` in the script is how to continue.

## 🧩 embedding it

The program is a library with a loop on top, and you can use both. `wiring::Setup` assembles a
session (the kernel, the policy, the six tools, the sandbox and the `App` around them) in the
right order, and returns the two receivers a loop needs:

```rust
let wired = kamchatka::wiring::Setup {
    tools: Some(vec!["fs".into(), "context".into()]),
    allow: vec![Subject::parse("fs:read")],
    system: Some("you are working in a Rust workspace".into()),
    spend: Some(50_000),
    ..Default::default()
}
.wire(kamchatka::endpoint::connect(Some("qwen/qwen3-coder")).await?)?;
```

It subscribes to events before setting anything up, so the trace includes the setup, and keeps
alive the handle the introspection tools hold weakly.

From there, `App::submit` takes a line (a message or a command) and returns what it did, what it
said, and any page it opened:

```rust
let reply = wired.app.submit("/budget").await;
```

`headless::Headless` is one loop built on that, and the program's screen is another. A host with
its own event loop needs neither: it holds the `App`, feeds `wired.events` into `on_event` and
`wired.finished` into `on_outcome`, and passes in lines whenever it has them.

`spend` is enforced in `on_event` and `App::start_turn`, so a host that keeps passing in lines is
told when the limit is reached instead of being billed without notice. `App::spent`, `App::spend`
and `App::set_spend` give the total, the limit, and a way to change it.

## 🗂️ a settings file

`--config-file kamchatka.json` replaces arguments you'd otherwise type every time:

```json
{
  "model": "qwen/qwen3-coder",
  "system": "you are working in a Rust workspace; run `cargo test` before saying anything is done",
  "mcp": ["files=npx -y @modelcontextprotocol/server-filesystem /srv"],
  "sandbox-read": ["~/.rustup", "~/.cargo"],
  "allow": ["fs:read"],
  "allow-server": ["files"],
  "spend": 200000
}
```

Every key is optional and named after its command-line argument, and **anything given on the
command line takes precedence**; a list given there *replaces* the file's. For the model, the order
is the command line, then `KAMCHATKA_MODEL`, then the file. A leading `~` in `sandbox-allow` and
`sandbox-read` means your home directory. An unknown key stops the program with its name, and so
does a key that doesn't apply to the current run: `deadline` and `on-ask` (other than `deny`) with
`--serve`. Things that only apply to one run (a message, `-r`, `-f`, `--headless`) have no key.

Two keys have no command-line equivalent:

- **`border-color`**, six hex digits, is the colour of the window border and of the marker showing
  which part of the screen has the keys. The default is `#1A936F`; `null` uses the terminal's own
  foreground colour. Colours that carry meaning (yellow for `ask`, red for a waiting question)
  aren't affected.
- **`tools`** sets which of the six tools a session starts with; all of them if omitted. `[]` offers
  none; `null` and unknown names are refused. `/tools toggle ID` turns one off or on during a
  session.

**Without `--config-file`, two places are checked**: `./kamchatka.json`, then
`kamchatka/kamchatka.json` under `XDG_CONFIG_HOME` or `~/.config`. A file found this way is named
before anything else happens. **`./kamchatka.json` needs your approval before it's read**, because
whoever wrote the directory wrote it, and it can start MCP servers, turn off the sandbox and grant
permissions; anything but `y` runs without it, and a run without a terminal is refused and told to
pass it with `--config-file`. The one in your config directory is read without asking.

**A starting point comes with the crate**, as `kamchatka.json` next to this file and in each
release archive, and `kamchatka --print-config` prints the same content: every setting at its
default. It grants nothing and sets no limits (`allow` and the sandbox lists are empty, `on-ask` is
`deny`, `spend` is `null`), because a default that granted anything would be `kamchatka` deciding on
your behalf.

## 🎛️ options

`kamchatka --help` lists every option, and the environment variables:

- **`KAMCHATKA_API_KEY`**, sent to whatever endpoint is used. Without it, `OPENROUTER_API_KEY` is
  used for OpenRouter, `OPENAI_API_KEY` for `api.openai.com` and `ANTHROPIC_API_KEY` for
  Anthropic's own API, each only sent to that service. A model served on this machine needs no key.
- **`KAMCHATKA_BASE_URL`**, the endpoint: OpenRouter by default, Google's own API with `--gemini`,
  or Anthropic's own with `--anthropic`.
- **`KAMCHATKA_CONTEXT_LIMIT`**, for providers that don't report their model's context size.
- **`KAMCHATKA_NO_ATTRIBUTION`**, which stops `kamchatka` from identifying itself to OpenRouter.

A build with `--advise` also lists the advisor's variables.

## 🚦 an advisor on the question

### a colour on the question

`--advise` only exists if the program was built with `--features shell-advisor`, and even then it
has to be turned on. It asks a **System One model** (a model that answers typed questions instead
of writing text) about every shell command you're about to be **asked** about, and shows its answer
in the permission question:

```console
$ export KAMCHATKA_SYSTEM1_MODEL=<one of the models below>
$ kamchatka --advise "tidy up the build artifacts"
```

**You choose the model; there's no default.** OpenRouter offers several, from different vendors,
listed at `https://openrouter.ai/api/v1/models?output_modalities=decisions`. `--advise` without
`KAMCHATKA_SYSTEM1_MODEL` stops before the session starts and says where the list is. A provider
that offers one of these but isn't on the list can be used through OpenRouter's bring-your-own-key,
which needs nothing from `kamchatka`.

A build with the feature but without `--advise` shows no ratings, and the permissions tab says so.

**The questions go to OpenRouter and are paid for with an OpenRouter key**:
`KAMCHATKA_SYSTEM1_API_KEY` if set; otherwise the session's own key, if the session already uses
OpenRouter; otherwise `OPENROUTER_API_KEY`. A session using another service has that service's
key, so if neither of the other two is set, it stops before starting and says which key to set.

`KAMCHATKA_SYSTEM1_BASE_URL` sends the questions elsewhere: to your own server answering the same
route OpenRouter uses (`/systemone`), or to one of the services the client knows the address of.
Keys from other variables are only ever sent to OpenRouter, so an advisor elsewhere gets
`KAMCHATKA_SYSTEM1_API_KEY` if set, and no key otherwise, never an OpenRouter one. If such a service
does require a key, it says so on the first question, in place of the colour, or at startup if the
client knows it requires one. Which servers are supported, and how to address and run each, is in
[the client's documentation][engines].

It **decides nothing**. What your rules allow runs without a question and without anything being
sent, and what they refuse is refused. If the advisor is unreachable or its answer can't be read,
you only lose the colour, and the question says why in its place.

**The colour is the model's opinion, and different models have different opinions**: not every one
answers what `--advise` asks, those that do sometimes rate a command a level apart, and their
confidence figures don't mean the same thing. [Where the models differ](#where-the-models-differ)
says what to do about it.

**What is sent**: for each shell command you're about to be asked about (in a default session,
every command the model writes, since `exec:run` asks by default), the tool's id, the capabilities
it declared, and its arguments, cut to 2KB per argument with a note where cut. Not the
conversation, the system instruction, or the model's text. Calls your rules allow or refuse aren't
sent anywhere.

### an advisor on this machine

A server running locally is used by pointing `KAMCHATKA_SYSTEM1_BASE_URL` at it; it needs no key,
and **nothing leaves the machine**. [The client's documentation][engines] lists the servers that
support the route and how to start each.

[engines]: https://docs.rs/nachalnik-providers/latest/nachalnik_providers/system1/#engines

### what the colour says

The rating appears in the question, below what the tool wants and above the arguments: the
advisor's rating of the command, in colour, and its confidence.

Green, yellow or red, on a three-level scale: it only reads, lists, searches or changes directory;
it changes files inside the working directory, in ways git or a rebuild could undo; or it reaches
outside the working directory, destroys something unrecoverable, or sends something off the
machine. You still have to read the command; the colour is just a quick first impression. A chained
command is rated stage by stage, and the worst stage is underlined.

**The most dangerous level is also asked as a separate yes-or-no question**, and the worse of the
two answers is shown, since models answer the two forms differently. A score is rounded to the
nearest level, and a rating the advisor wasn't confident about is never shown as green, or as safer
than it scored. The confidence percentage is shown too, so you can see when an unexpected yellow is
one the advisor wasn't sure about.

### where the models differ

Every server takes the same request and answers in the same format, but that's where the similarity
ends: whether one answers the scale at all, how it rates a command, what its percentage means, and
what it accepts all differ, and `kamchatka` treats all of them the same rather than adjusting for
any. [The client's documentation][differ] describes what has been observed. In practice, an
unexpected colour may be the model's quirk rather than the command's danger, and a command is never
shown green on a rating the model itself reported as uncertain.

To see how a model handles the scale before relying on it, run `tests/advise.rs` with
`KAMCHATKA_SYSTEM1_MODEL` set (and `KAMCHATKA_SYSTEM1_BASE_URL` for your own server): it sends a
couple of dozen commands and reports how each was rated. A failure there tells you something about
the model rather than indicating a bug; the scale was worded for the models it was first tested
with.

[differ]: https://docs.rs/nachalnik-providers/latest/nachalnik_providers/system1/#where-engines-differ

## 💾 a session on disk

Every session is saved as it goes, and the last thing printed says where: a record of its events
and a snapshot of its context, both in a `kamchatka` directory in the system's temporary
directory, plus the `kamchatka -r` command to continue from the snapshot. Each event is appended as
soon as it happens, and the snapshot is rewritten whenever the session goes idle and when a turn
starts, so a `kill -9` or a power cut leaves the record complete up to the last event. A `SIGTERM`
or `SIGHUP` ends the session the same way `/quit` does.

A session is named after its start time, in UTC, and its two files use that name. A resumed
session keeps the name, and its record is saved next to the one it continued from, as `NAME-2`. It
continues with the model its record names, unless `-m`, `KAMCHATKA_MODEL` or a settings file names
another, but at the address this run uses: a snapshot is a file anyone can pass to anyone, and the
address determines where this run's key is sent. The directory is a safety net, not an archive
(`/save PATH` is how to keep a session somewhere permanent); it's `0700`, and `--no-record` turns it
off.

`/save` writes two files: a `.jsonl` of every event, and a `.json` snapshot of the context. Given a
directory, it names them after the session, and the first save there goes next to any existing
record with that name rather than overwriting it, since a resumed session has the same name as the
one it continued from. There are two ways to use the snapshot.

`kamchatka -r PATH` starts a new session from it, which restores it exactly: item numbers, model
parameters and what the token counter learned come back as they were. It also reads the `.jsonl`
with the same name, if it's next to the snapshot, for earlier versions of edited items; without
it, those pages are empty. The resumed session's own log starts at the resume, so for history
before that, keep the old `.jsonl`.

`/load PATH` brings it into your current session, which is often more useful. It's a context
operation and follows the same rule as the others: nothing is destroyed. What was in the context is
**excluded**, keeping its numbers and content; anything **pinned** stays, because pins are your
decisions (and `--system` is pinned); the loaded items are added as new items with new numbers, and
their conversation is shown on the chat tab. Two `/undo`s put everything back, except model
parameters the snapshot replaced, which the load lists.

That makes any saved file a checkpoint. `/save good`, let the agent wander off somewhere useless,
`/load good`, and continue from where it was still on track, without losing the detour, which stays
in the context marked `-` excluded if you want to read it.

`kamchatka --check PATH` checks a record without starting anything. PATH is the log, the snapshot,
or their shared name. It reports what doesn't add up: a line that isn't a record, an event this
version doesn't know, a record missing or numbered twice, a call requested but never finished, and a
snapshot with items the log doesn't account for. A session that ended with calls still waiting says
so in its record, and those calls aren't reported; a killed run doesn't say anything, so not every
finding is a fault. Any finding makes the exit status non-zero.

`kamchatka reconcile a.json b.json -o merged` merges several forks of one session (one snapshot
resumed twice and continued in two ways) into one session to continue from, without starting it.
Items the forks still have in common are kept whole. From the rest of each fork, only the notes the
agent wrote for itself are kept; the turns are left out, because the same work done twice in an
order that never happened isn't a real conversation. A shared item left in different states in
different forks gets the most included state, and an item one fork revised is kept as revised; if
two forks revised it differently, the merge is refused, naming both. Two notes with the same label
are both kept, and a pinned instruction where the forks split says which notes came from which fork
and which labels appear in more than one. Two sessions that merely start the same way are refused,
and nothing is overwritten: `kamchatka -r merged.json` continues, and `/request` shows the first
request before anything is sent.

### starting again

`/restart` is the opposite of `/load`. Instead of bringing a file into the current session, it
saves the session and replaces it with a brand new one, as if you'd quit and started the program
again, without quitting.

The new session first says where the old one's record and snapshot were saved, with the
`kamchatka -r` command to continue it.

**It goes back to the command-line settings, not to where the session had got to.** The model is
whatever `--model` said, the permissions are `--allow` and `--deny` again, tools turned off with
`/tools toggle` are back, `--system` and `--file` are re-read, and the context is empty. Only what
can't be cheaply recreated carries over: the connection to the provider, the MCP servers (whose
tools are installed into the new session without restarting their processes), and the sandbox, which
can't be lifted once applied. A session started with `-r` restarts into an *empty* one. A running
turn is stopped first.

It works anywhere a line does, and a piped run continues reading the same input into the new
session. Clients attached over a socket are **disconnected**: the browser page reconnects to the
new session by itself, and `--connect`, which follows one session, exits with the old one.

## 🧪 the tests

They draw the screen and check it, against a scripted model:

```rust
harness.tab(Tab::Context);               // over to the context
harness.press(KeyCode::Home).await;      // the first item
harness.press(KeyCode::Char(' ')).await; // out

let after = harness.app.kernel.preview_request().unwrap();
assert!(!format!("{:?}", after.messages).contains("hunter2"));
```

The rest of the program is tested without a screen (the policy, real commands in a real Landlock
sandbox, sessions driven by lines and over a socket, MCP servers), and
`cargo test -p kamchatka --no-default-features --features mcp` runs only those. `--test live` needs
a key, and checks that a real API accepts what's sent and that a real model can act on what the
tools return.

## 🧰 a toolchain lives in your home directory

`$HOME` isn't a system directory, so a sandboxed command can't read it, and most toolchains are
installed there. `cargo` is a rustup shim, rustup reads `~/.rustup/settings.toml` before doing
anything, and a model asked to build a Rust project gets a `Permission denied` that looks like a
missing compiler. Give it read-only access to the toolchain:

```console
$ kamchatka --sandbox-read ~/.rustup,~/.cargo -m …
```

The same goes for `~/.nvm`, `~/.pyenv`, `~/.rbenv` and the rest. Both flags take a comma-separated
list and can be repeated: `--sandbox-allow /srv/repo,/tmp/work`.

**Daemons you talk to over a socket need access too**, on Linux 7.1 and later. A sandboxed command
can only connect to a unix socket where it could have created one, so the session bus, the
compositor and container daemons all return `Permission denied`, since each would run requests
outside the sandbox. Allow the socket itself rather than its directory:

```console
$ kamchatka --sandbox-allow /run/docker.sock -m …
```

Connecting counts as writing, so it's `--sandbox-allow`, and `--deny fs:write` removes it along with
everything else writable. On older kernels, every socket is reachable anyway.

Abstract sockets (ones with no file, used by the X server and some session buses) created outside
the sandbox are refused too, on Linux 6.12 and later, and no flag allows them: a command can only
reach abstract sockets it created itself. An X client refused the abstract socket tries the file
next, so `--sandbox-allow /tmp/.X11-unix/X0` gives a command a display back, and with it access to
every window on that display.

**A command can only signal processes the session started**, on Linux 6.12 and later: a server one
call left running can be stopped by the next, and `kill` aimed anywhere else fails with `Operation
not permitted`. The restriction is applied to `kamchatka`'s own process before it starts anything,
so everything it starts runs with `no_new_privs`, including MCP servers, which means set-user-ID
programs like `sudo` gain no privileges in them. `--no-sandbox` turns it off.

**In `/dev`, a command can reach five devices** (`null`, `zero`, `full`, `random` and `urandom`) and
nothing else: the rest of `/dev` includes your other terminals, which a command could read your
typing from, shared memory, and on a desktop the camera and microphone. `--sandbox-device` and the
settings file's `sandbox-device` replace this list rather than adding to it. A command that needs a
pseudo-terminal (`script`, `expect`) needs `/dev/ptmx` and `/dev/pts`, which give access to all your
other terminals too.

**Git needs no flag.** A sandboxed command that can't read your git configuration gets
`GIT_CONFIG_GLOBAL` pointing at nothing, since git would otherwise treat the refusal as a corrupt
configuration and fail every command. `--sandbox-read ~/.gitconfig` gives it your identity and
aliases.

**Permission errors say where they came from.** When the sandbox refuses a path, the tool result
says it's outside what the session can reach, lists every place the session can reach (read-write
or read-only), and says to work there or ask for the path to be allowed. A refusal that wasn't
the sandbox's (`cat /etc/shadow`) gets no such note.

## 🌐 the network, when a command tries

When the shell is sandboxed, network access is asked about when a command opens an internet socket,
not based on the command's name. The sandboxed child process installs a seccomp filter after the
Landlock rules, and the filter pauses every `socket()` call for `AF_INET` or `AF_INET6` until
`kamchatka` answers: from `net:reach` if it says `allow` or `deny`, or by asking you if it says
`ask`, once per command. So with `--allow exec:run`, `git status` runs without a question, while
`python3 fetch.py` is asked about as soon as it does a DNS lookup, which couldn't be told apart from
the command alone.

An unattended run answers with `--on-ask`, immediately, while the turn is running, since the
command is waiting on the answer and waiting for the turn to end would wait forever:

```console
$ kamchatka --headless --allow exec:run 'fetch the release notes'
```

reports on stderr which command tried to use the network and what it was told. A served session
sends the question to its client as `reaching`, answered with `reach`, and the browser page shows it
in the same panel as the kernel's questions.

`deny` refuses every internet socket, including UDP, which Landlock alone can't refuse; most
programs report a refused lookup as `Temporary failure in name resolution`, which is why the tool
result explains it.

Where the filter can't be installed (`--no-sandbox`, or a kernel that can't pause calls), the
program goes back to judging commands by name: a short list of programs that exist to use the
network are asked about before they run, and UDP isn't refused. The permissions tab ends a
sandboxed shell's line with `network gated` or `network not gated`, so you can see which applies;
under `--no-sandbox` the line is `shell: a command can do any of these` instead.

[shot-web]: https://github.com/ljedrz/nachalnik/raw/HEAD/kamchatka/assets/web.jpg
