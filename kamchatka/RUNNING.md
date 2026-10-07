# running kamchatka

Without a screen, against which endpoint, configured how, embedded in something else — and
what the number in the status line actually means. [The readme](README.md) says what the
program is and [GUIDE.md](GUIDE.md) covers the screen and the keys.

---

## 🤖 the same program, without a screen

`--headless` drives the session from lines on stdin instead of from keys. A line is what a line
typed at the prompt is: a message, or a command if it starts with `/`. It is implied when stdout
is not a terminal, and it says so rather than deciding quietly.

```console
$ printf 'what is 2+2? answer with just the number\n/budget\n' \
    | kamchatka --headless -m qwen/qwen3-coder > session.jsonl
```

**stdout is the session log**, one JSON record per line — the same bytes `/save` writes, so
`jq 'select(.event.event == "tool.requested")' session.jsonl` is the whole of reading a run back.
**stderr is a person's half**: what the model said, what a tool was asked to do, and what any
command you sent answered. Every key with a command behind it has the command too — `/undo`,
`/redo`, `/cleanup`, `/continue` — and the line is what works down a pipe.

Nothing can be asked at a prompt that is not there, so the answers are given in advance:

| flag | what it does |
| --- | --- |
| `--allow fs,exec:run` | answer `allow` for a whole domain, one operation in one (`--allow context:note`) or a path rule (`--allow '*.rs'`, `--allow 'vendor/'`) |
| `--deny fs:write,.env*` | the same, refused; the strictest of everything consulted still wins |
| `--on-ask deny` | what happens to a question nobody answered in advance; `deny` is the default |

A path rule is a file name in which `*` stands for any run of characters, a name starting with a dot
such as `.env`, or one directory name with a slash after it, which is about that directory wherever
it sits in a path. It is matched against the *last* name in a path, so a rule about a file is that
file's name and nothing else: `--deny 'b.txt*'` is a rule about `b.txt` and `--deny b.txt` is not
one, because a bare name is read as a whole domain. It is not the glob language `fs`'s own `glob`
argument takes, and a pattern that reads like one — `src/**` — stops the session and says how to
write the rule.

`--on-ask deny` is the default because a run nobody is watching should not be able to do a thing
nobody has allowed. The model is told it was *this call* rather than a standing rule, so it works
around it rather than retrying.

Somebody else's tools are given the same way, and where a tool came from is a subject of its own.
`--mcp files=… --allow-server files` is the whole of granting one server: the same subject the
permissions tab writes when somebody answers **always** at the prompt. Without it the tools are
there and every call is refused, because a server named on a command line is not thereby trusted
to run.

A run nobody is watching has to be told when to stop. `--deadline 300` interrupts whatever is in
flight and leaves by the ordinary door — what arrived is kept and the session is written out,
which a killed process cannot say. <kbd>ctrl+c</kbd> does the same once, and leaves
at once if pressed again. `--deadline 0` is no deadline, as `0` is no ceiling to `--spend` and
`--requests`.

`--spend 50000` stops one too, because a model that has found a loop will stay inside any deadline
you were willing to give it. The unit is tokens, `input + output` as the provider reports them,
`fork`s included; nothing here carries a price list. It is a stopping rule rather than a cap, since
what a response cost is known only once it has arrived. When the session stops, the lines after
are still read: a command runs, and a message is passed over unsent. `/spend` says what has been
spent and against what, `/spend N` raises it, and `/spend 0` takes it away. It belongs to the
session, so a screen session, a piped script and [an embedding host](#-embedding-it) are held to
the same number.

The total starts at nothing in every process. `-r` carries a session's context on, and what its
counter had learned, but not what it had spent: the snapshot is the runtime's record, and this
figure is the program's. So a ceiling given to a resumed run bounds that run, not the session
since it began, and `/restart` starts a session with a total of its own.

An endpoint that reports no usage at all says so, once, rather than holding a ceiling that nothing
will ever reach — a limit quietly never met is worse than no limit, because whoever set it is
reading the run as bounded.

`--deadline` counts from the moment the program starts, so an endpoint that never answers and an
MCP server that never finishes its handshake are held to it too, and a `/restart` does not start it
again. It cannot cut short a `/model` or `/endpoint` switch, which finishes first.

A job a command put in the background — `sleep 300 &`, a server it started, one it put in a
session of its own with `setsid` — outlives its call, and is stopped when the session ends:
`SIGTERM`, then `SIGKILL` for what is still there two seconds later, with a line naming each
command it came from. They are found by `KAMCHATKA_CALL`, which every command runs with and
everything it starts inherits, so what clears its own environment (`env -i`) is not found.
`--leave-running` leaves them running for a server you asked for on purpose, and names them the
same way.

A run that one of these cut short says which on stderr as it happens, and leaves with a status of
its own, so a script can tell a session that did its work from one that was stopped:

| status | the run |
| --- | --- |
| `0` | worked through its input |
| `1` | failed: the last turn could not be finished, or the program could not start |
| `3` | reached the `--spend` ceiling |
| `4` | ended on a turn paused at `--requests`, waiting for `/continue` |
| `5` | worked through its input, and its record could not be written |
| `124` | ran out of `--deadline`, whether or not the session had started — as `timeout` does |
| `129`, `130`, `143` | was ended by `SIGHUP`, <kbd>ctrl+c</kbd> or `SIGTERM`: `128` and the signal |

The first of them to happen is the one reported. A session served with no screen leaves the same
way when a signal ends it.

A line is read only while the runtime is resting, so the lines of a script cannot overtake the
turns they belong to. `--no-default-features --features mcp` builds the program with no screen
compiled in.

## 🔌 a session you can walk away from

`--serve` puts a socket in front of a session, and `--connect` attaches to one. The screen stays
where there is one to draw on, so a session started at a desk is the same session a browser picks up
— keys and clients are two ways into one `App`, and the one loop that owns it answers both.

```console
$ kamchatka --serve unix:/run/user/1000/kamchatka.sock -m qwen/qwen3-coder
```

```console
$ printf 'what is 2+2? answer with just the number\n' \
    | kamchatka --connect unix:/run/user/1000/kamchatka.sock > session.jsonl
```

**The session belongs to the program running it, not to whoever is attached.** A turn carries on
with nobody watching, a question waits for somebody to come back and answer it, and a client
picking the session up an hour later picks up the same session. A client's input closing detaches
it; it never ends anybody's session on the way out, and `/quit` is how you say you meant to.

`--connect` writes what `--headless` writes, so it is a drop-in for it in a script, and sends a
piped line only once the turn before it is over. It answers a permission question with the same
three letters the panel takes, `y`, `n` and `a`; `ctrl+c` stops the turn and a second one detaches;
and `?4` prints what item 4 actually holds, which is the one thing a stream of records can never
say, because [the log names things rather than copying them](#-a-session-on-disk). A running command
that [reaches for the network](#-the-network-when-a-command-tries) is answered with the same three
letters, once the kernel's own questions are.

A question still open when a client's input closes is left for somebody else: the next client to
attach, the desk where the session is drawn, or this one coming back; the lines of its script that
were waiting go unsent, and it says which. A script that drives a session alone says
`--on-ask deny` or `--on-ask allow` on the `--connect` command line; a settings file's `on-ask` is
not read there.

**Where it listens is the whole of its authentication, so it refuses to listen anywhere else.**
The protocol carries a `shell` tool, so reaching the session is reaching the machine. A unix
socket is made `0600` the moment it exists, and `tcp:127.0.0.1:PORT` is the machine; anything else,
`--serve tcp:0.0.0.0:7878` for one, is refused. From another machine, tunnel through something that
authenticates:

```console
host$  kamchatka --serve tcp:127.0.0.1:7878 -m qwen/qwen3-coder
other$ ssh -N -L 7878:127.0.0.1:7878 host &
other$ kamchatka --connect tcp:127.0.0.1:7878
```

A dropped TCP connection is picked back up for a minute, backing off.

**A session serves one client at a time, and the newest wins.** A client that attaches takes the
session, and the one that had it is told it was replaced and let go of: `--connect` exits saying
so, and the browser page stops reconnecting; attaching again on purpose takes it back. The newest
wins because the usual second connection is the same client coming back while the session still
holds its old one.

Several people driving one agent is not something this program has a design for, and one at a time
is what stands in for one. A session drawn at a desk and served is still two ways in: both may
submit, interrupt and answer questions, and a message either sends into a running turn waits in one
queue with the other's. Each goes in on its own, in the order it was sent, with a turn of its own.

**A command that reaches for the endpoint** — `/models`, `/compact`, `/model`, `/endpoint` — runs
while the session goes on, and the line after it waits until it is back. `ctrl+c` stops a listing
or a pass; a switch is always let finish.

The protocol is newline-delimited JSON, so `nc` and `jq` read it, and
[`remote`](https://docs.rs/kamchatka/latest/kamchatka/remote/)'s documentation describes it.

`examples/attached.rs` is a client of a served session — attach, ask, follow the turn, refuse a
permission question, read back the item the answer was recorded as:

```console
$ kamchatka --serve tcp:127.0.0.1:7878 -m qwen/qwen3-coder &
$ cargo run --example attached -- tcp:127.0.0.1:7878 "what is 2+2"
```

Its header carries the wire transcript, for a client in another language.

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

The page has the terminal's four tabs, in the terminal's order: the button in the top right corner
cycles **chat → context → events → permissions**.

The **context** view is the rows the context tab draws — what each item is, what it is estimated to
cost, and what the next request will do with it. Tapping a row fetches the whole of that item,
because the projection names items rather than carrying them; the button on the right of a row is
its state, and tapping *that* moves it to the next one — active, elided, excluded, active again —
which is <kbd>space</kbd> on the context tab, note included. The **chat** follows, as it does at
the terminal. The **events** view is the trace, with the time each step took; the **permissions**
view is the rules, each row with **allow**, **ask** and **deny**, and **ask** is the way back from
an `always`.

The prompt is on the chat alone. A waiting question colours the cycler, and a mark in the top left
blinks while a turn is running. A model's answer is rendered as markdown, nothing in it becomes
HTML, and a link is a link only to `http`, `https` or `mailto`. It streams in as it is written and
is then replaced by what was recorded, as at the terminal.

`/cleanup` (<kbd>ctrl+l</kbd>) takes this program's own lines off the chat and leaves the
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

`--gemini` talks to Google's own API instead of an OpenAI-compatible one, `--anthropic` to
Anthropic's Messages API (OpenRouter speaks it too, at `/api/v1/messages`), and `--responses` to
OpenAI's Responses API at the same address and key as chat completions. All three answer a turn as
parts in the order they were produced — thinking, a sentence, a call — where chat completions
flattens it into a string beside a list of calls. Here the order is kept, counted, pruned and
elided like any other content, the context pane and the `context` tool read it out as produced,
and each part goes back out with whatever signature or sealed thinking it arrived with, which the
next request is refused without. Instructions added mid-session stay where they were put under
`--responses`, so the cached start of the prompt does not move.

Every provider answers one trait, `Dialect`, so `/model`, `/models`, `/endpoint` and the status
line work the same against any of them.

```console
$ export KAMCHATKA_API_KEY=...        # a Google AI Studio key
$ kamchatka --gemini "what does src/kernel.rs do?"
$ export ANTHROPIC_API_KEY=...        # an Anthropic Console key
$ kamchatka --anthropic -m claude-haiku-4-5 "what does src/kernel.rs do?"
$ export KAMCHATKA_BASE_URL=https://api.openai.com/v1 KAMCHATKA_API_KEY=...   # an OpenAI key
$ kamchatka --responses -m gpt-5-mini "what does src/kernel.rs do?"
```

## 🔀 the model, and the address it lives at

`/model` says which model this is talking to, where that is, in what dialect, and how much context
it has.

`/model ID` switches the model and `/endpoint URL [ID]` switches the address — and the model with
it, because a model belongs to the address that serves it. Given no model, the old name is kept and
the new endpoint is asked whether it has one by that name, which is a notice now rather than a 404
on the next request.

`/models [FILTER]` lists what the endpoint serves under the ids it uses, which differ from one
endpoint to the next, and marks the one you are on with `▸`.

The key is *not* switched with the address. It is read from the environment once, at startup, and a
key typed at a prompt would be a key in the transcript — so `/endpoint` is for the addresses that
need no key or take the same one: a local model, a proxy, another base URL on the same account.

What is not switched either way is the context: the same items go to whatever answers next, which
is what makes the answers comparable. `/seams` names the runtime's replaceable parts and what is in
each now.

`/params KEY JSON` sets one model parameter, `/params KEY null` takes it away, and `/params` shows
them, with what else this model takes where the endpoint publishes it — because a parameter a model
does not take is sent and ignored with nothing saying so. A parameter you set that the listing does
not name is marked as ignored, and a value over a published maximum is noted. Only what you set is
sent, apart from what a dialect cannot send a request without; under `--anthropic` that includes
asking for the prompt to be cached, which `/params cache_control false` turns off for an endpoint
that refuses the field.

A parameter named after something the request is built from — `messages`, `tools`, `model` and
their kin in each dialect — is refused.

## 📏 the number in the status line is an estimate, and says which kind

Nothing here has the model's tokenizer, so the figure the status line leads with is an estimate —
it is written `~2,460` for that reason. The percentage beside it names the total it is a
percentage of (`1.9% (128k)`), because a fraction of an unstated number is not something anybody
can act on, and it turns yellow past 70% and red past 90%. Then comes what the provider actually
charged for the last request, and `/budget` is where all of it is reconciled: the next request,
split into context and tool definitions; the same request anchored on the last response; the limit
and how much of it the next request would fill; what the last request really cost, as the provider
counted it; and what the counter has learned.

Once there has been a response, the corner shows the anchored figure: what the provider charged
for the last request, plus the estimate of only what has changed since. An item that has not moved
contributes its measured cost and no error, so the error is a few percent of the change rather
than of the context, and per-message framing, tool schemas and anything the counter cannot price
are inside the provider's figure already.

The counter corrects itself from every response to a request it could price in full, and from
every refusal for length that names the limit this session knows — which is worth more, since it
counts the bytes in the units the limit is enforced in. `/budget` says from how many requests and
by what scale.

Before any response, on an endpoint that reports no usage, and after a change of model until the
next answer, the corner falls back to the plain estimate. And where something in the context has
no number on it at all, `/budget` says how many pieces — a figure that is a floor is never shown
as one that is complete.

`/budget` is about the next request; `/spend` is about the session. It adds up what the provider
charged for every response — measured, never estimated — and says it against the ceiling, if one
was set with `--spend` or with `/spend N`. The session stops at that ceiling, which is what it is
for; see [the guards on a run nobody is watching](#-the-same-program-without-a-screen). What the
`--advise` advisor reports its answers cost is counted too, whichever key pays for it, and `/spend`
says how much of the total was the advisor's.

## 🗜️ when the context fills

The compactor sheds what the conversation is done with, by two rules. The first runs before every
request: a tool result whose turn is over goes to a marker, and so does a picture or a document the
model has been shown in an exchange that is over — unless the marker would be bigger than the
result. The second waits for room: once the context passes `--compact` (0.8 by default) of the
limit, the oldest exchanges go whole until it is down to the target, the second fraction in
`--compact 0.8,0.6`; left out, that is twenty points under the first or half of it, whichever is
more, so the drops come in bursts rather than moving the cached start of the request every turn.

Neither rule touches the turn in progress, anything pinned, or a note the model wrote for itself,
and neither summarizes. Every item is still on the context tab: an elided one <kbd>space</kbd> from
coming back, a dropped one `/restore` from it. A result you bring back by hand is not elided again
by the first rule. The marker says what was taken has been read, and what the model has not been
shown is never taken to make a request fit.

`/compact` asks that same compactor by hand, and shows its answer before anything happens: every
item it would take, with the identifier, what it is and what it is holding. It then waits, in the
prompt's place, for <kbd>y</kbd> or <kbd>n</kbd> — pinned rather than modal, like a tool's
question, so the context tab is a keystroke away while it stands and <kbd>p</kbd> there is the
answer to "not that one". Saying yes works the pass out again, so a pin made while reading the
list is honoured rather than refused after the fact.

The compactor keeps away from the turn in progress and from what is pinned, so a context that is
mostly those is one it can do nothing about. When the context is past `--compact` and there is
nothing left it may take, the session says the context is full, once, while there is still room
under the limit: what is left is for you to `/exclude`, or for the model to exclude through its
`context` tool. The model is told too, by a note put into the context before its next request —
inside the turn that filled it, so a run nobody is watching can make its own room rather than run
into the limit. Where the model could not make room with the `context` tool — it is turned off,
a rule refuses `exclude` and `elide`, or a headless run would refuse them for want of somebody to
ask — the note asks the model to tell you instead, and it is worded again, a copy already in the
context included, whenever that changes. It says so again when there is room, and the note is
excluded.

In a headless run that making of room is a question like any other: every `context` call is
asked about, `look` included, and `--on-ask deny` refuses them all, so the note sends the model
nowhere it cannot go. `--allow context` is what lets it make room — or
`--allow context:look,context:exclude,context:elide`, for a run that should free room and change
nothing else.

The same holds for every sentence that sends the model to a tool: a refusal from `fs` names
`shell`, `grep` or `write`, and an answer from `setup`, `log` or `fork` names `context` or `log`,
only where the model could make that call now — the tool on offer, and the call not refused by a
rule or by a question nobody is there to answer.

Past the limit the request is not sent at all: the runtime refuses it rather than paying a round
trip for an endpoint to say what the corner already says, and it prints how much of it has to go.
The context tab says the same figure while you are deciding which rows answer for it. What it does
*not* say is that the model read the request, because the model never saw it — a refusal from here
names the counter's own estimate as an estimate, and a refusal from the endpoint is the only one
that quotes a count.

`--send-oversized` sends it anyway, since the figure is an estimate and the limit is whatever the
endpoint advertised, which can be smaller than the window behind it.

Down a pipe there are no keys, so `--headless` prints the `/compact` list to stderr and takes it:
a line the operator typed is not a question the model asked.

`/compact` is also the way out of a session too big to send. The tool that prunes a context is the
*model's* — `context` — and reaching it costs a request, which is the thing that is failing.

Down a pipe, once a turn has been refused for this and while the next request would be too, the
messages after it are passed over unsent, as they are past a `--spend` ceiling: kept, each would
make the request it could not get into longer, and they would all go out together once there was
room. Commands are still read, so an `/exclude` or a `/limit` in the script is the way on.

## 🧩 embedding it

The program is a library with a loop on top, and both halves are yours. `wiring::Setup` assembles
a session — the kernel, the policy, the six tools, the sandbox and the `App` around them — in the
order they have to go in, and hands back the two receivers a loop needs:

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

It subscribes to events before anything is plugged in, so the trace has the wiring, and keeps alive
the handle the introspection tools hold weakly.

From there, `App::submit` takes a line — a message or a command — and answers with what it did,
what it said, and any page it opened:

```rust
let reply = wired.app.submit("/budget").await;
```

`headless::Headless` is one loop over that, and the program's own is the other. A host with an
event loop of its own wants neither: it holds the `App`, pumps `wired.events` into `on_event` and
`wired.finished` into `on_outcome`, and hands in a line whenever it has one.

`spend` is enforced in `on_event` and `App::start_turn`, so a host that keeps handing in lines is
told rather than quietly billed. `App::spent`, `App::spend` and `App::set_spend` are the figure, the
ceiling and the way to move it.

## 🗂️ a settings file

`--config-file kamchatka.json` stands in for the arguments you would otherwise type every time:

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

Every key is optional and named after the argument it stands in for, and **anything given on the
command line wins** — a list there *replaces* the file's. For the model the order is the command
line, then `KAMCHATKA_MODEL`, then the file. A leading `~` in `sandbox-allow` and `sandbox-read` is
your home directory. A key nothing reads stops the program and names it, and so does a key that does
not apply to this run: `deadline` and `on-ask` (other than `deny`) beside `--serve`. What belongs to
one invocation — a message, `-r`, `-f`, `--headless` — has no key.

Two keys have no argument behind them:

- **`border-color`**, six hex digits, is the colour of the window's frame and of what says *the keys
  are here*. Left out it is `#1A936F`; `null` is the terminal's own foreground. The colours that
  mean something — yellow for `ask`, red for a question still waiting — are not affected.
- **`tools`** is which of the six tools a session starts with; left out, all of them. `[]` offers
  none, `null` and an unknown name are refused. `/tools toggle ID` turns one off or on mid-session.

**Given no `--config-file`, two places are looked in**: `./kamchatka.json`, then
`kamchatka/kamchatka.json` under `XDG_CONFIG_HOME` or `~/.config`. A file picked up this way is
named before anything else happens. **`./kamchatka.json` is asked about before it is read**,
because whoever wrote the directory wrote it, and it may start MCP servers, turn the sandbox off
and grant permissions; anything but `y` runs without it, and a run with no terminal is refused and
told to name it with `--config-file`. The one under your config directory is read without asking.

**A starting point ships with the crate**, as `kamchatka.json` beside this file and in a release's
archive, and `kamchatka --print-config` prints the same bytes: every setting, each at its default.
It grants nothing and bounds nothing — `allow` and the sandbox lists are empty, `on-ask` is `deny`,
`spend` is `null` — because a default that granted something would be this program deciding on
your behalf.

## 🎛️ options

`kamchatka --help` lists every option, and the environment variables:

- **`KAMCHATKA_API_KEY`**, sent wherever the requests go. Without it, `OPENROUTER_API_KEY` is read
  for OpenRouter, `OPENAI_API_KEY` for `api.openai.com` and `ANTHROPIC_API_KEY` for Anthropic's own
  address, each sent there and nowhere else. A model served on this machine needs no key.
- **`KAMCHATKA_BASE_URL`**, where the requests go: OpenRouter, Google's own with `--gemini`, or
  Anthropic's own with `--anthropic`, unless it says otherwise.
- **`KAMCHATKA_CONTEXT_LIMIT`**, for a provider that will not say how much context its model has.
- **`KAMCHATKA_NO_ATTRIBUTION`**, which stops the program naming itself to OpenRouter.

A build with `--advise` lists the advisor's too.

## 🚦 an advisor on the question

### a colour on the question

`--advise` is off unless the program was built with `--features shell-advisor`, and then it still
has to be asked for. What it adds is a question put to a **System One model** — a model that
answers typed questions rather than writing text — about every shell command you are about to be
**asked** about, and its answer drawn in that question:

```console
$ export KAMCHATKA_SYSTEM1_MODEL=<one of the models below>
$ kamchatka --advise "tidy up the build artifacts"
```

**Which model is yours to name, and there is no default.** OpenRouter serves a family of them, from
several vendors, and lists them at
`https://openrouter.ai/api/v1/models?output_modalities=decisions`. `--advise` without
`KAMCHATKA_SYSTEM1_MODEL` stops before the session begins and says where that list is. A provider
that sells one of these and is not on it is reached through OpenRouter's bring-your-own-key, which
needs nothing from this program.

A build with the feature and no `--advise` draws no ratings, and the permissions tab says so.

**The questions go to OpenRouter, and an OpenRouter key pays for them.** `KAMCHATKA_SYSTEM1_API_KEY`
if it is set; otherwise the session's own key, where the session already talks to OpenRouter; and
otherwise `OPENROUTER_API_KEY`. A session pointed anywhere else holds a key that service issued,
so with neither of the other two set it stops before it begins and says which key to set.

`KAMCHATKA_SYSTEM1_BASE_URL` moves the questions somewhere else: an engine of your own that
answers the same route, `/systemone`, which is the one OpenRouter takes them on, or one of the
services the client knows the address of. A borrowed key is only ever sent to OpenRouter, so an
advisor pointed anywhere else is sent `KAMCHATKA_SYSTEM1_API_KEY` if it is set, and no key at all
if it is not — never an OpenRouter one. A service there that does check a key says so on the first
question, where the colour would have been, or at startup where the client knows it checks one.
Which engines are reachable, and how each is addressed and run, is in [the client's
documentation][engines].

It **decides nothing**. What the standing rules allow runs without a question and without
anything being sent, and what they refuse is refused. An advisor that is unreachable or
unparseable costs the colour and nothing else, and the question says why in its place.

**The colour is the model's reading, and the models do not read alike** — not every one answers
what `--advise` asks, the ones that do place some commands a level apart, and they do not mean the
same thing by how sure they are. [Where the models differ](#where-the-models-differ) says what to
do about it.

**What leaves the machine**: for each shell command you are about to be asked about — which in a
default session is every command the model writes, since `exec:run` is a question by default — the
tool's id, the capabilities it declared, and its arguments, capped at 2KB per argument with the
cut named. Not the conversation, not the system instruction, not the model's prose. A call the
rules allow or refuse is sent nowhere.

### an advisor on this machine

An engine running here is reached with `KAMCHATKA_SYSTEM1_BASE_URL` pointed at it, and needs no
key, and then **nothing leaves the machine**. [The client's documentation][engines] names the
engines that serve the route and how to start each.

[engines]: https://docs.rs/nachalnik-providers/latest/nachalnik_providers/system1/#engines

### what the colour says

The rating is drawn in the question, on the line under what the tool wants and above the
arguments: what the advisor reads the command as, in its colour, and how sure it is.

Green, yellow or red, off a three-level rubric — it only reads, lists, searches or changes
directory; it changes files inside the working directory, the way git or a rebuild could undo; it
reaches outside the working directory, destroys something that cannot be got back, or sends
something off this machine. You still have to read the command; the colour is the half-second
before that. A chain is rated stage by stage, and the worst stage is underlined.

**The top of that rubric is asked a second time, as a yes-or-no**, and the worse of the two answers
is drawn, since engines read the two forms differently. A score is read by the level it is nearest,
and a rating the advisor was not sure of is never drawn green and never safer than it scored. The
percentage is on the line, so a yellow you cannot explain is visibly one nobody was sure of.

### where the models differ

Every engine takes the same request and answers in the same shape, and that is where the agreement
ends: whether one answers the rubric at all, where it places a command, what its percentage means
and what it will take all differ, and this program reads every one the same way rather than
correcting any. [The client's documentation][differ] says what has been seen. What it comes to here
is that a colour that surprises you may be the engine's rather than the command's — and that a
command is never drawn green on a reading the engine itself reported as unsure.

To see what one makes of the rubric before relying on it, run `tests/advise.rs` with
`KAMCHATKA_SYSTEM1_MODEL` set, and `KAMCHATKA_SYSTEM1_BASE_URL` for an engine of your own: it puts a
couple of dozen commands to it and says where each landed. A failure there is a reading to know
about rather than a bug — the rubric was worded against the models it was first run with.

[differ]: https://docs.rs/nachalnik-providers/latest/nachalnik_providers/system1/#where-engines-differ

## 💾 a session on disk

Every session is written down as it goes, and the last thing printed is where: a record of its
events and a snapshot of its context, both in a `kamchatka` directory under the system's temporary
one, and the `kamchatka -r` line that carries on from the snapshot. Each event is appended the
moment it happens, and the snapshot is rewritten whenever the session comes to rest and when a
turn begins, so a `kill -9` or a pulled plug leaves the record complete to the last event. A
`SIGTERM` or `SIGHUP` ends the session the way `/quit` does.

A session's name is when it started, in UTC, and is the name of its two files. A resumed session
keeps its name, and its record goes beside the one it carried on from, as `NAME-2`. It carries on
with the model its record names, unless `-m`, `KAMCHATKA_MODEL` or a settings file names another,
but at the address this run is pointed at: a snapshot is a file anybody can hand somebody, and the
address is where this run's key would go. The directory is a safety net, not an archive —
`/save PATH` is how a session goes somewhere it will be next week — it is `0700`, and `--no-record`
turns it off.

`/save` writes two files: a `.jsonl` of every event that happened, and a `.json` snapshot of the
context. Given a directory, it names them after the session, and the first save into one goes
beside a record already there under that name rather than over it — a resumed session has the
name of the one it carried on from. The snapshot has two ways back in.

`kamchatka -r PATH` starts a fresh session from it, which is the faithful one: the item numbers,
the model parameters and what the token counter had learned come back exactly as they were. It
also reads the `.jsonl` of the same name, if it is beside the snapshot, for what an item said before
it was rewritten; without it those pages are empty. The resumed session's own log starts at the
resume, so the record from two hops back is the `.jsonl` you kept.

`/load PATH` brings it into the session you are already in, which is the useful one. It is a
context operation and it plays by the same rule as the rest of them — nothing is destroyed. What
was in the context is **excluded**, keeping its numbers and its contents; anything **pinned**
stays where it is, because a pin is you saying so and `--system` is pinned; the loaded items come
in as new items with new numbers, and the conversation they were is read back onto the chat tab.
`/undo` twice puts the whole thing back, apart from the model parameters the snapshot replaced,
which the load names.

That makes a checkpoint out of a file. `/save good`, let the agent go somewhere useless,
`/load good`, and carry on from where it was still working — without losing the detour, which is
sitting in the context marked `-` excluded if you want to read it.

`kamchatka --check PATH` reads a record without starting anything. PATH is the log, the snapshot, or
their shared name. It says what does not add up: a line that is not a record, an event this version
does not know, a record missing or numbered twice, a call asked for and never finished, and a
snapshot whose items the log does not account for. A session that ended with calls still waiting
says so in its record, and those calls are not findings; a killed run says nothing, so a finding
is not always a fault. Anything found makes the exit status non-zero.

`kamchatka reconcile a.json b.json -o merged` folds several forks of one session — one snapshot
resumed twice and carried on two ways — into one session to carry on from, and starts nothing. The
items the forks still share are kept whole. From the rest of each fork only the notes the agent
wrote down for itself come across; the turns stay behind, because the same work done twice in an
order that never happened is not a conversation. A shared item left in a different state in each
fork gets the most included one, and an item one fork revised is kept as revised — two forks that
revised it differently are refused, naming both. Two notes under one label are both kept, and a
pinned instruction at the point where the forks parted says which notes came from which fork and
which labels more than one of them carries. Two sessions that only begin alike are refused, and
nothing is written over: `kamchatka -r merged.json` carries on, and `/request` shows the first
request before anything is sent.

### starting again

`/restart` is the other end of that. Where `/load` brings a file into the session you are in,
this writes the session out and puts a brand new one in its place — the same thing that would
happen if you quit and ran the program again, without quitting.

The first thing the new session says is where the old one's record and snapshot went, and the
`kamchatka -r` that carries on from it.

**It goes back to the flags, not to where the session had got to.** The model is whatever `--model`
said, the permissions are `--allow` and `--deny` again, every tool `/tools toggle` switched off is
back, `--system` and `--file` are re-read, and the context is empty. What carries over is only
what cannot be rebuilt cheaply or at all: the connection to the provider, the MCP servers — whose
tools are installed into the new session rather than their processes being spawned again — and the
sandbox, which is a ruleset that cannot be lifted once it has been applied. A session started with
`-r` restarts into an *empty* one. A running turn is stopped to do it.

It works wherever a line does, and a piped run carries on reading the same input into the new
session. Clients attached over a socket are **disconnected**: the browser page reconnects into the
new session by itself, and `--connect`, which follows one session, ends with the old one.

## 🧪 the tests

They draw the screen and read it back, against a scripted model:

```rust
harness.tab(Tab::Context);               // over to the context
harness.press(KeyCode::Home).await;      // the first item
harness.press(KeyCode::Char(' ')).await; // out

let after = harness.app.kernel.preview_request().unwrap();
assert!(!format!("{:?}", after.messages).contains("hunter2"));
```

The rest of the program is tested without a screen — the policy, real commands under a real
Landlock ruleset, sessions driven by lines and through a socket, MCP servers — and
`cargo test -p kamchatka --no-default-features --features mcp` runs only those. `--test live`
wants a key, and checks that a real API accepts what was built and a real model can act on what
the tools say.

## 🧰 a toolchain lives in your home directory

`$HOME` is not a system directory, so a confined command cannot read it — and most toolchains keep
their real installation there. `cargo` is a rustup shim, rustup reads `~/.rustup/settings.toml`
before it does anything at all, and a model asked to build a Rust project gets a
`Permission denied` that looks like a missing compiler. Hand it the toolchain, for reading only:

```console
$ kamchatka --sandbox-read ~/.rustup,~/.cargo -m …
```

The same goes for `~/.nvm`, `~/.pyenv`, `~/.rbenv` and the rest. Both flags take a comma-separated
list and may be repeated: `--sandbox-allow /srv/repo,/tmp/work`.

**A daemon you talk to over a socket needs one too**, on Linux 7.1 and up. A confined command may
connect to a unix socket only where it could have written one, so the session bus, the compositor
and a container daemon all come back `Permission denied`, because each of them runs what it is
asked outside the confinement. Hand over the socket rather than the directory it sits in:

```console
$ kamchatka --sandbox-allow /run/docker.sock -m …
```

Connecting is the writing half of the rule, so it is `--sandbox-allow`, and `--deny fs:write` takes
it away with the rest. On an older kernel every socket was reachable all along.

An abstract socket — one with no file, which the X server and some session buses listen on — made
outside the confinement is refused too, on Linux 6.12 and up, and no flag hands one over: a
command may reach only the abstract sockets it made itself. An X client that is refused the
abstract one tries the file next, so `--sandbox-allow /tmp/.X11-unix/X0` is how a command gets a
display back, and with it every window on that display.

**A command may signal what the session started and nothing else**, on Linux 6.12 and up: a
server one call left running is stopped by the next, and `kill` aimed anywhere else is `Operation
not permitted`. The boundary is put on this program's own process before it starts anything, so
everything it starts runs with `no_new_privs`, MCP servers included: a set-user-ID program such
as `sudo` gains nothing in them. `--no-sandbox` leaves it off.

**Under `/dev` a command reaches five devices**, `null`, `zero`, `full`, `random` and `urandom`,
and nothing else: the rest of `/dev` is your other terminals, which a command could read what you
type into, shared memory, and on a desktop the camera and microphone. The list is
`--sandbox-device` and the settings file's `sandbox-device`, which replace it rather than add to
it. A command that needs a pty (`script`, `expect`) needs `/dev/ptmx` and `/dev/pts`, and with
them every other terminal of yours.

**Git needs no flag.** A confined command whose git configuration is out of reach is handed
`GIT_CONFIG_GLOBAL` pointing at nothing, since git would otherwise read a refusal as a corrupt
configuration and fail every command. `--sandbox-read ~/.gitconfig` brings your identity and
aliases in.

**A permission error says where it came from.** When the confinement refuses a path, the tool
result names it as outside what this session reaches, lists every place the session does reach,
read-write or read-only, and says to work there or ask for the path to be opened up. A refusal that
was not the confinement's — `cat /etc/shadow` — gets no such line.

## 🌐 the network, when a command tries

Where the shell is confined, a command is asked about the network when it opens an internet socket
rather than for what it is called. The child that confines itself installs a seccomp filter after
the ruleset, and the filter holds every `socket()` for `AF_INET` or `AF_INET6` until this program
answers — from `net:reach` where it says `allow` or `deny`, and from you where it says `ask`,
once per command. So `--allow exec:run` runs `git status` without a question and asks
about `python3 fetch.py` the moment it looks a name up, which reading the command could never have
told apart.

A run with nobody at it answers with `--on-ask`, while the turn is still running — the command is
waiting on the answer, so waiting for the turn would be waiting for ever:

```console
$ kamchatka --headless --allow exec:run 'fetch the release notes'
```

says on stderr which command reached out and what it was answered. A served session sends the
question to its client as `reaching`, answered with `reach`, and the browser page draws it in the
same panel as the kernel's questions.

`deny` is every internet socket refused, so UDP as well, which the ruleset alone cannot refuse —
and a refused lookup comes back from most programs as `Temporary failure in name resolution`,
which is why the tool result says what it was.

Where the filter cannot be installed — `--no-sandbox`, or a kernel that cannot hold a call — the
program goes back to reading the command: a short list of programs whose point is the network, a
question about them before they run, and UDP not refused. The permissions tab ends a confined
shell's line with `network gated` or `network not gated`, so which one a session has is on the
screen rather than something to work out; under `--no-sandbox` the line is
`shell: a command can do any of these` instead.
