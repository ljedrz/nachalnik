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
command you sent answered. Every verb is there, because a command was never the keyboard's to
begin with. A key with a command behind it — <kbd>u</kbd> and <kbd>U</kbd> being
`/undo` and `/redo`, <kbd>ctrl+l</kbd> being `/cleanup` — is the shorthand, and the line is what
works down a pipe.

Nothing can be asked at a prompt that is not there, so the answers are given in advance:

| flag | what it does |
| --- | --- |
| `--allow fs,exec:run` | answer `allow` for a whole domain, one operation in one (`--allow context:note`) or a path rule (`--allow '*.rs'`, `--allow 'vendor/'`) |
| `--deny fs:write,.env*` | the same, refused; the strictest of everything consulted still wins |
| `--on-ask deny` | what happens to a question nobody answered in advance. The default |

A path rule is a file name in which `*` stands for any run of characters, a name starting with a dot
such as `.env`, or one directory name with a slash after it, which is about that directory wherever
it sits in a path. It is matched against the *last* name in a path, so a rule about a file is that
file's name and nothing else: `--deny 'b.txt*'` is a rule about `b.txt` and `--deny b.txt` is not
one, because a bare name is read as a whole domain — which is what `files` and `shell` are too, so
nothing in the text can say which was meant. A run stopped for naming a domain no call is judged
under says how to write the rule. It is not the glob language `fs`'s own `glob` argument takes, and
a pattern that reads like one — `src/**` — stops the session rather than going onto the permissions
tab as a rule no path can match.

`--on-ask deny` rather than `allow` is deliberate: a run nobody is watching should not be able to
do a thing nobody has allowed. The model is told, and told that it was *this call* rather than a
standing rule — so it works around it rather than retrying. On stderr it is the tool's name and
the answer, `because nobody is here to be asked`.

Somebody else's tools are given the same way, and where a tool came from is a subject of its own.
`--mcp files=… --allow-server files` is the whole of granting one server: the same subject the
permissions tab writes when somebody answers **always** at the prompt, given before the server has
been spawned or said what it offers. Without it the tools are there and every call is refused,
because a server named on a command line is not thereby trusted to run. It is its own argument
rather than a spelling of `--allow` because a server's name and a domain are both bare words and
nothing in either says which it is.

A run nobody is watching has to be told when to stop. `--deadline 300` interrupts
whatever is in flight and leaves by the ordinary door — what arrived is kept and the session is
written out, which a killed process cannot say. <kbd>ctrl+c</kbd> does the same once, and leaves
at once if pressed again. `--deadline 0` is no deadline, as `0` is no ceiling to `--spend` and
`--requests`.

`--spend 50000` stops one too, because time is not the only thing one of these can spend: a model
that has found a loop — a tool that fails the same way, a question it keeps re-asking — will stay
inside any deadline you were willing to give it. The unit is tokens, `input + output` as the
provider reports them, because nothing here carries a price list and a figure in money would be one;
what a `fork` is charged counts the same as the session's own requests.
It is a stopping rule rather than a cap, since what a response cost is known only once it has
arrived. When the session stops, it says what had been spent against what ceiling, and that
`/spend N` raises it. The lines after that are still read: a command runs, which is how the
`/spend` gets there, and a message is passed over unsent rather than kept in the context for the
next turn to carry out.

It belongs to the *session* rather than to this loop, which is why it is on
[`wiring::Setup`](#-embedding-it) and not on the headless driver. What stops the turn that crossed
the line is `App`, and so is what refuses the next one, so a screen session, a piped script and a
host with a loop of its own are held to the same number and none of them can get round it by not
asking. `/spend` says what has been spent and against what, `/spend N` raises it, and `/spend 0`
takes it away, which is the way back for whoever set it too low.

The total starts at nothing in every process. `-r` carries a session's context on, and what its
counter had learned, but not what it had spent: the snapshot is the runtime's record, and this
figure is the program's. So a ceiling given to a resumed run bounds that run, not the session
since it began, and `/restart` starts a session with a total of its own.

An endpoint that reports no usage at all says so, once, rather than holding a ceiling that nothing
will ever reach — a limit quietly never met is worse than no limit, because whoever set it is
reading the run as bounded.

`--deadline` is the one that needs nobody's cooperation — of the model, at least. It counts from
the moment the program starts, so an endpoint that never answers and an MCP server that never
finishes its handshake are held to it too, and a `/restart` does not start it again. What it cannot
cut short is a command of your own that is waiting on the endpoint: `/models` fetches a list, and
`/model` and `/endpoint` finish their switch before the next line is read, so a deadline that
falls during one of those is served when it returns.

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
| `124` | ran out of `--deadline`, whether or not the session had started — as `timeout` does |
| `129`, `130`, `143` | was ended by `SIGHUP`, <kbd>ctrl+c</kbd> or `SIGTERM`: `128` and the signal |

The first of them to happen is the one reported, and a pause is the last turn's: one that
`/continue` carried on from is not where the run stopped. A session served with no screen leaves
the same way when a signal ends it — `129`, `130` or `143` — so a service manager stopping one can
tell that from a `/quit`.

A line is read only while the runtime is resting, which is the one place this differs from a
person at a prompt and is what makes a piped script mean what it says: the lines of a script
cannot overtake the turns they belong to. `--no-default-features --features mcp` builds this and
nothing else — no screen compiled in, and the same `--headless` behaviour whether or not the flag
is given.

## 🔌 a session you can walk away from

`--serve` puts a socket in front of a session, and `--connect` attaches to one. The screen stays
where there is one to draw on, so a session started at a desk is the same session a phone picks up
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

`--connect` writes what `--headless` writes — the records to stdout, what a person reads to
stderr — so it is a drop-in for it in a script, and like it, it sends a piped line only once the
turn before it is over; at a terminal, what is typed goes at once. It answers a permission question with the same
three letters the panel takes, `y`, `n` and `a`; `ctrl+c` stops the turn and a second one detaches;
and `?4` prints what item 4 actually holds, which is the one thing a stream of records can never
say, because [the log names things rather than copying them](#-a-session-on-disk). A running command
that [reaches for the network](#-the-network-when-a-command-tries) is answered with the same three
letters, once the kernel's own questions are.

A question still open when a client's input closes is left for somebody else: the next client to
attach, the desk where the session is drawn, or this one coming back. A client cannot tell whose
question it is, so answering it would be deciding for whoever was asked. The lines of its script
that were waiting for that turn to be over go unsent, and it says which as it leaves. A script that drives a session
alone and wants its questions answered on the way out says so with `--on-ask deny` or
`--on-ask allow` on the `--connect` command line; a settings file's `on-ask` is for runs that have
nobody else to ask, and `--connect` does not read it. `leave`, the `--connect` default, is refused
anywhere else, because there nobody else could answer.

**Where it listens is the whole of its authentication, so it refuses to listen anywhere else.**
There is no bearer token in this protocol and there is not going to be one: it carries a `shell`
tool, so reaching the session is reaching the machine, and a scheme that had to be kept in step
would be protecting a channel whose real boundary is the socket. A unix socket's file permissions
are that boundary — the file is made `0600` the moment it exists — and `tcp:127.0.0.1:PORT` is the
machine. Anything else, `--serve tcp:0.0.0.0:7878` for one, is refused with the reason and a way
out: it is not a loopback address and whatever reaches the port runs the `shell` tool as you, so
listen on `tcp:127.0.0.1:PORT` or on `unix:PATH`, and reach it from elsewhere through something
that does authenticate — `ssh -L` is the usual one.

So reaching a session from another machine is a tunnel: SSH already has the key management, so the
session gets an authenticated, encrypted transport without this program growing either.

```console
host$  kamchatka --serve tcp:127.0.0.1:7878 -m qwen/qwen3-coder
other$ ssh -N -L 7878:127.0.0.1:7878 host &
other$ kamchatka --connect tcp:127.0.0.1:7878
```

A port and a socket file are not the same thing to write a protocol over. Frames here are small —
a line somebody typed, a fragment of a sentence — so `TCP_NODELAY` is on, because Nagle would hold
each one back waiting for the last to be acknowledged. Keepalive is on, because a peer whose machine
slept sends no `FIN` and a read on the other side would wait for ever. And a dropped connection is
picked back up for a minute, backing off: a socket file's host either comes back at once or is not
coming back, but over a port the ordinary reason to lose a connection is a laptop changing access
points, and getting it back takes seconds.

**A session serves one client at a time, and the newest wins.** A client that attaches takes the
session, and the one that had it is told it was replaced and let go of: `--connect` exits saying
so, and the browser page stops reconnecting. Neither comes back by itself, because two clients that
each reattached would trade the session back and forth; attaching again on purpose takes it back.
The newest rather than the first, because the usual second connection is the same client coming
back — a laptop that changed access points, a tab reconnecting — while the session still holds its
old connection, which keepalive takes a couple of minutes to notice is gone. Refused, that client
would be locked out of its own session for longer than it keeps trying.

Several people driving one agent is not something this program has a design for, and one at a time
is what stands in for one. A session drawn at a desk and served is still two ways in: both may
submit, interrupt and answer questions, and a message either sends into a running turn waits in one
queue with the other's. Each goes in on its own, in the order it was sent, with a turn of its own.

**A command that reaches for the endpoint is finished before the next line is read**, without the
session standing still for it. `/models` fetches a listing, `/compact` works out a pass and a
`/model` or `/endpoint` settles a switch, each sent off while the session goes on drawing,
answering clients and reading the kernel; the line after one waits until it is back, because the
line after `/models` is often the `/model` it was asked for. A client's waiting line is answered
`queued` at once and handed in afterwards, and anything it opens on a page is said instead.
`ctrl+c` — or a client's interrupt, or `--deadline` — stops a listing or a pass; a switch is
always let finish, because half an `/endpoint` is worse than a wait. Down a pipe and from a client,
a `/compact` is taken when its pass comes back, with what it took said first, and the list from
`/models` is said rather than paged.

The protocol itself is newline-delimited JSON, which is to say `nc` and `jq` read it. It lives in
[`remote`](https://docs.rs/kamchatka/latest/kamchatka/remote/) rather than in a crate of its own,
and the module documentation is where the argument is: why the records are the half that cannot be
lost and the fragments are the half that can, and why attaching answers with a projection rather
than with a snapshot.

`examples/attached.rs` is a client of a served session — attach, ask, follow the turn, refuse a
permission question, read back the item the answer was recorded as:

```console
$ kamchatka --serve tcp:127.0.0.1:7878 -m qwen/qwen3-coder &
$ cargo run --example attached -- tcp:127.0.0.1:7878 "what is 2+2"
```

Its header carries the wire transcript, because a client in another language needs the JSON and
none of the Rust.

And `examples/gateway.rs` with `examples/browser.html` put the session in a browser, which is the
one client that cannot reach it on its own: a browser has no TCP, so something has to terminate
HTTP in front. Three routes, no framework, no build step, and one HTML file.

```console
$ kamchatka --serve tcp:127.0.0.1:7878 -m qwen/qwen3-coder &
$ cargo run --example gateway -- tcp:127.0.0.1:7878 0.0.0.0:8080
```

`examples/phone.rs` is the same thing in one command, for when there is nobody at the machine: it
wires a session of its own, binds it to a loopback port the kernel picks, and serves the same page
in front of it. The HTTP half is `examples/relay/`, shared rather than copied, which is why
`gateway.rs` is still only a relay.

```console
$ KAMCHATKA_PHONE_LISTEN=0.0.0.0:8080 cargo run --example phone -- -m qwen/qwen3-coder
```

It takes the program's own arguments, all of them, because the session it assembles is the
program's: `-m`, `--advise`, `--allow`, `-s`, a first message, a settings file found where you are
standing. Where the *page* listens is the one thing that is not one of them — the positional is the
first message, and `--serve` already means the session's own socket — so it is
`KAMCHATKA_PHONE_LISTEN`, loopback unless it says otherwise.

It is `text/event-stream` rather than a WebSocket because SSE already has this protocol's shape in
it. An event may carry an `id:`, and a browser that loses the stream reconnects **by itself** and
sends `Last-Event-ID:` — which is exactly `attach { since }`. So the browser does resume with no
client code, and the negative space matches too: an event with no `id:` does not move
`Last-Event-ID`, so records get one and the fragments of a model still typing do not. A browser
never tries to resume from something that was never recoverable.

The page has the terminal's four tabs, in the terminal's order: the button in the top right corner
cycles **chat → context → events → permissions**.

The **context** view is the rows the context tab draws — what each item is, what it is estimated to
cost, and what the next request will do with it. Tapping a row fetches the whole of that item,
because the projection names items rather than carrying them; the button on the right of a row is
its state, and tapping *that* moves it to the next one — active, then a marker where it was, then
out of the request entirely, then active again. It is one button rather than three because the
middle step is the one worth having: taking a tool result out makes the projector drop the call
that asked for it, so the model reads a conversation it never had, while an elided one still
answers its call and only the content is gone. That is <kbd>space</kbd> on the context tab, and it
is the same function underneath — including the note it writes, which the model reads.

Moving a state moves the **chat** too, the way it does at the terminal — because the chat is the
context, read back. An excluded item is gone from the conversation; an elided one keeps its place
and reads as its **marker**, the projector's own words in the brackets it put round them, which is
exactly what the model reads there. Both of those are the runtime's rules rather than the page's,
so the page asks for a fresh conversation rather than working them out.

The **events** view is the trace: what happened, in this program's own words, with the gap beside
the lines that took any time — which is what that tab is *for*, since the question somebody brings
to a log is which step was slow. Nothing under a tenth of a second gets a figure, and neither does
a line that ended a wait for a *person*: however long you took to answer a question, it is not a
step the program spent. The **permissions** view is the rules the policy holds and what each
covers, with the confinement over the top and a count of the subjects nobody has decided, which are
the ones that will be asked about.

Switching to context or permissions asks the session for a fresh projection rather than adding up
the records, because `going`, `left_out` and `marker` are answers about the *next request* and no
record carries them. That is `project`, which every client has. The events view asks for one
on every record, because what it draws is the trace, in the program's own words, and that comes
with a projection rather than with the records.

The prompt is on the chat and nowhere else, which is where the terminal keeps it; a view that is a
list of rows has nothing to say to a box that takes a line. A waiting question colours the cycler
rather than following you about — the same thing the tab strip does by going red, and for the same
reason: from another view, the question is not what you are looking at. A mark in the top left
blinks while a turn is running, because a turn can be a minute of nothing arriving and a still page
is otherwise indistinguishable from a dead connection.

A model's answer streams in as it is written and is then **replaced by what was recorded**, which
is the rule the terminal follows: the fragments are the live half and the item is what was kept.
They are worth replacing rather than keeping. A fragment is best-effort, so a client that fell
behind would otherwise hold a truncated answer for ever with nothing to say so; and fragments are
what the provider sent byte for byte, where the context trims the blank lines some of them like to
open with.

`/cleanup` takes this program's own lines off the chat — what it said about what it did, and what it
answered a command with — and leaves the conversation, which is the context. It is the command form
of <kbd>ctrl+l</kbd>, so it works at a terminal, down a pipe, through `--connect` and on the page;
and because a session has one voice, clearing it on one client clears it on all of them.

The gateway has **no authentication and no encryption**, and says so when you point it at anything
but loopback. `--serve` itself still refuses to. Whatever reaches the page reaches the `shell`
tool, so it is a thing for a network you trust while you are watching it, and not a thing to leave
running.

## 🧩 two dialects, and why one of them keeps the order

`--gemini` talks to Google's own API instead of an OpenAI-compatible one. That is not a
convenience — it is the difference between seeing what the model did and seeing a rearrangement of
it.

`generateContent` answers with `content.parts[]`: a thinking part, a sentence, a `functionCall`,
in the order they were produced. The OpenAI-compatible shim in front of the same model flattens
that into a `content` string beside a `tool_calls` array, because the dialect it is imitating has
nowhere to put an order. Everything downstream then reads a turn that has been tidied up.

The context pane reads such a turn out in the order it was produced — the thinking, the sentence,
the call — and `context` hands the agent the same blocks, numbered, with the signed ones marked. It
is only worth having because the order is really in there: the runtime records it as
`Content::Blocks`, counts it, prunes it and elides it like any other content, and a
`LinearProjector` with `send_blocks` set, as `--gemini`'s is, sends it back the same way.

Signatures are the other half. Gemini signs the parts of a turn and answers
`400 Function call is missing a thought_signature` to a request that returns one without it — and
it signs text parts as well as calls, which a message with three slots has nowhere to keep. Here
each part's own fields ride back out on the block they arrived on, unread.

Both providers answer one trait, `Dialect`, so `/model`, `/models`, `/endpoint` and the status
line work the same against either and nothing above them knows which wire format it got.

```console
$ export KAMCHATKA_API_KEY=...        # a Google AI Studio key
$ kamchatka --gemini "what does src/kernel.rs do?"
```

## 🔀 the model, and the address it lives at

`/model` says which model this is talking to, where that is, in what dialect, and how much context
it has.

`/model ID` switches the model and `/endpoint URL [ID]` switches the address — and the model with
it, because a model belongs to the address that serves it. Switching one and keeping the other is
how a session ends up asking the ollama on this machine for `gemini-3.6-flash`; given no model the
old name is kept and the new endpoint is asked whether it has one by that name, which is a notice
now rather than a 404 on the next request. Both matter for different reasons: comparing two hosted
models is one address and two names, while comparing a hosted model with the one running on this
machine is two addresses. A comparison that cannot see the address is a comparison of names.

Every session is written down as it goes, and the last thing printed is where: a record of its
events and a snapshot of its context, both in a `kamchatka` directory under the system's temporary
one, and the `kamchatka -r` line that carries on from the snapshot. Only the lines naming the jobs
it stopped on the way out come after it, since stopping them is the one step that waits. Each event is appended to the
record the moment it happens, and the snapshot is rewritten whenever the session comes to rest
and when a turn begins — so a `kill -9`, an out-of-memory kill or a pulled plug leaves the record
complete to the last event, and a snapshot of where things stood before the turn that was cut
short. A `SIGTERM` or `SIGHUP` — a closed terminal, `timeout`, `docker stop` — ends the session the
way `/quit` does: a running turn is stopped and waited for, and the record says so.

A session's name is when it started, in UTC, so that a list of them says something to whoever is
reading it, and it is also the name of its two files.

Nobody has to type `/save` for any of this, because a session that ended badly is the one worth
reading afterwards. A resumed session keeps its name, and its record goes beside the one it carried
on from, as `NAME-2`, rather than over it. It carries on with the model its record says it was
last talking to, unless `-m`, `KAMCHATKA_MODEL` or a settings file names another — but at the
address this run is pointed at, not the record's. The record says where it was, with credentials
left out, and a session pointed somewhere else is told so along with the `/endpoint` that goes
back; it is not taken there, because that address is where this run's key would go, and a
snapshot is a file anybody can hand somebody. It is a temporary directory because this is a
safety net and not an archive — `/save PATH` is still how a session goes somewhere it will be next
week — and `--no-record` turns it off for anyone who would rather a transcript did not outlive the
terminal. The directory is `0700`: what goes in it is a whole conversation and every byte every tool
produced, written without anybody asking, and under an ordinary umask that would be a
world-readable file on a shared machine.

`kamchatka --check PATH` reads a record without starting anything. PATH is the log, the snapshot, or
their shared name. It says what does not add up: a line that is not a record, an event this version
does not know, a record missing or numbered twice, a call asked for and never finished, and a
snapshot whose items the log does not account for. A killed run leaves a call that never finished,
so a finding is not always a fault; anything found makes the exit status non-zero. The shapes it
reads are in `nachalnik/tests/records/`, one of every event and a snapshot per format, and a
reader written against those does not need this program.

`kamchatka reconcile a.json b.json -o merged` folds several forks of one session — one snapshot
resumed twice and carried on two ways — into one session to carry on from, and starts nothing. The
items the forks still share are kept whole. From the rest of each fork only the notes the agent
wrote down for itself come across; the turns stay behind, because the same work done twice in an
order that never happened is not a conversation. A shared item left in a different state in each
fork gets the most included one, and an item one fork revised is kept as revised — two forks that
revised it differently are refused, naming both. Two notes under one label are both kept, and a
pinned instruction at the point where the forks parted says which notes came from which fork and
which labels more than one of them carries. Each fork's log is read for the model it was talking
to: where they all talked to the same one, the token counter's correction is kept and the session
carries on with that model, and otherwise it is dropped and `-m` says which. Forks of one session
carry its name, so two sessions that only begin alike — the same `-s`, say — are refused. What it
writes is a log and a snapshot that check clean, and nothing is written over: `kamchatka -r
merged.json` carries on, and `/request` is how to read what the first request would be before
anything is sent.

`/models [FILTER]` is what makes `/model` usable, because the ids belong to the endpoint rather
than to the model: the same thing is `google/gemini-3.5-flash` at one address and
`gemini-3.5-flash` at another, and after an `/endpoint` there is no other way to find out which
without guessing. It asks the endpoint, marks the one you are on with `▸` where it stands in the
endpoint's own order, and takes a filter because a list of everything an endpoint serves is not an
answer; a filtered list says how many of how many matched.

The key is *not* switched with the address. It is read from the environment once, at startup, and a
key typed at a prompt would be a key in the transcript — so `/endpoint` is for the addresses that
need no key or take the same one: a local model, a proxy, another base URL on the same account.

What is not switched either way is the context. The same items go to whatever answers next, which
is what makes the answers comparable. `/seams` names the six replaceable parts and what is in each
of them right now, asked of the kernel rather than restated from what this program set up at
startup.

`/params KEY JSON` sets one model parameter, `/params KEY null` takes it away, and `/params` shows
them — with what else this model takes, where the endpoint publishes it. It shows the second
because a parameter a model does *not* take is not refused: it is sent, ignored, and nothing
anywhere says so, which makes a `seed` set for a reproducible run buy no reproducibility and look
exactly like one that worked. The runtime invents none of them — only what you set is sent, apart
from the `thinkingConfig` a `--gemini` request asks for its thinking with, and a
`generationConfig` of yours is merged over that — and a listing that publishes nothing is read as
silence rather than as a prohibition, because ollama and a bare proxy both say nothing here.

Where the listing is everything the model takes, a parameter you set that is not on it is named as
sent and ignored, and the ones it takes that you have not set follow. Where the endpoint publishes
its sampling parameters only, the same absence settles nothing, so it is named as sent and
unchecked instead.

A parameter named after something the request is built from — `messages`, `tools`, `model`, and
`contents` or `systemInstruction` under `--gemini` — is refused. Those are the context's, the
tools' and the session's, and neither dialect lets a parameter replace them.

## 📏 the number in the status line is an estimate, and says which kind

Nothing here has the model's tokenizer, so the figure the status line leads with is an estimate —
it is written `~2,460` for that reason. The percentage beside it names the total it is a
percentage of (`0.9% (128k)`), because a fraction of an unstated number is not something anybody
can act on, and it turns yellow past 70% and red past 90%. Then comes what the provider actually
charged for the last request, and `/budget` is where all of it is reconciled: the next request,
split into context and tool definitions; the same request anchored on the last response; the limit
and how much of it the next request would fill; what the last request really cost, as the provider
counted it; and what the counter has learned.

The first figure is the counter estimating the whole request from scratch. The anchored one is the
one the corner shows once there has been a response to anchor on, and it is a different method
rather than a better guess: it starts from what the provider charged, adds what the context
estimates *now*, and subtracts what the estimator says the items that figure already covered would
cost now. An item that has not moved appears in both estimates and cancels — so it contributes its
measured cost and no error at all, and only what has changed since the last request is being guessed
at. The error is a few percent of the change instead of a few percent of the context, which is the
difference between a thousand tokens and thirty on a context of a hundred thousand.

It also absorbs, exactly and for nothing, the three things the counter is structurally blind to:
per-message framing, the tool schemas as they stood when the request went out, and any payload it
refuses to price. A PDF the counter cannot put a number on is inside the provider's figure the
moment it has gone out once.

The counter is the runtime's `Calibrating` one: every response to a request it could price in full
tells it what that request really cost, and it adjusts. `/budget` says from how many requests, by
what scale, and what its own guesses came to against the provider's count.

So does every request the model refuses for being too long, and that one is worth more than a
response. What an endpoint charges for is a bill, and an aggregator in front of a model may quote
it in some other tokenizer's units; the number in a refusal is a count of the same bytes in the
units the limit is actually enforced in. Where the two disagree, the corner ends up comfortably
under a limit the model is already over — so a refusal corrects the counter, and says on screen
what the request really came to and how much has to go before the next one is sent.

A refusal is only read when it names the limit this session already knows, which is the one thing
that says it is counting in the same units. The same model id at the same address can refuse in
two voices: the aggregator's own, quoting the window this session knows, and the model behind it,
quoting a larger window in its native tokenizer against bytes the aggregator had counted as
fitting. Reading the second would name tens of thousands of tokens that were never there, so it is
left as the sentence it arrived as, which says the problem in words.

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

The compactor sheds what the conversation is done with, by two rules. The first runs before every
request, however empty the context is: a tool result whose turn is over — the model has answered
you with it in front of it, and you have said something since — goes to a marker, and so does a
picture or a document the model has been shown in an exchange that is over. The second waits for
room: once the context passes `--compact` (0.8 by default) of the limit, the oldest exchanges go
whole — your message, the model's turns answering it, their results and any file you attached for
it — until it is down to the target, the second fraction in `--compact 0.8,0.6` (`[0.8, 0.6]` in a
settings file). Left out, that is twenty points under the first or half of it, whichever is more;
the gap is what makes the drops come in bursts rather than an exchange before every turn, each of
which would move the start of the request a provider's prompt cache keys on. Set the two equal for
just enough each time. They are one setting because the second is about the first: a `--compact`
typed beside a settings file that names both replaces both.

Neither rule touches the turn in progress — what the model is in the middle of is what it has not
finished using, and a result taken from under it was read again, and taken again — nor anything
that is pinned, which the kernel refuses, nor a note the model wrote for itself, and neither
summarizes what it takes: it never read it. Every item is still on the context tab, still holding every byte it held: an elided one marked `…`, one
<kbd>space</kbd> from coming back, and a dropped one excluded, `/restore` from coming back. A result
you bring back by hand is left alone by the first rule rather than elided again before the next
request; when the context fills, it goes with its exchange like anything else, and a pin is what
keeps it.

Nothing either rule takes has gone unread, and the marker says so — a model reading a marker that
said only "compacted" decided it had never seen the files it had just summarised. That holds when
the request would not fit the limit, too: what the model has not been shown is never taken to make
it fit, and the request is not sent.

A marker costs something too — it is a line of text where the content was — so the compactor
counts what each elision actually recovers and leaves alone any result no bigger than the marker
that would replace it: eliding a `wrote 412 bytes to …` would make the request *bigger*. A picture
is the exception, and goes once it has been shown whatever its size: the counter has no number for
it, so no arithmetic would ever pick it.

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

`--send-oversized` sends it anyway. Both halves of that check can be wrong: the figure is an
estimate, and the limit is whatever the endpoint advertised. `liquid/lfm-2.5-2.6b` is quoted at
65,536 tokens on OpenRouter and routes to a provider whose own window is twice that, so a session
holding itself to the smaller number refuses requests that would have been answered. The flag
costs a round trip and buys the endpoint's own count, which is worth more than any guess made
here.

Down a pipe there are no keys, so `--headless` prints the `/compact` list to stderr and takes it.
That is the opposite of what `--on-ask` does with a tool's question, and they are different
questions: a tool's is the model asking to do something nobody vouched for, and this one is a line
the operator typed.

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
event loop of its own wants neither: it holds the `App`, pumps `wired.events` into `on_event` and
`wired.finished` into `on_outcome`, and hands in a line whenever it has one.

`spend` above is the one thing a host gets whether it asks or not. `on_event` is the door every
loop comes through, so that is where the provider's own figures are added up; once they pass the
ceiling the turn in flight is interrupted and `App::start_turn` refuses the next one, so a host
that keeps handing in lines is told rather than quietly billed. `App::spent`, `App::spend` and
`App::set_spend` are the figure, the ceiling and the way to move it.

## 🗂️ a settings file

`--config-file kamchatka.json` stands in for the arguments you would otherwise type every time.
JSON, because a project's settings are a handful of strings, numbers and lists and there is a
parser for that in the tree already:

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

Every key is optional, every one is named after the argument it stands in for — with two
exceptions, below — and **anything given on the command line wins**, including a value that happens
to be the default, because `--requests 8` is somebody saying eight rather than somebody saying
nothing. A list on the command line *replaces* the file's rather than adding to it: one rule for
every key is the only kind worth predicting, and the other way round there is no way to ask for
fewer. `--model` is the one setting with a variable behind it, so the order there is command line,
then `KAMCHATKA_MODEL`, then the file.

**`border-color` and `tools` are the exceptions**, the two settings with no argument behind them:

```json
{ "border-color": "#7aa2f7" }
```

Six hex digits, `#` optional, and it is the colour of the window's frame — along with everything
else drawn in it to say *the keys are here*: the active tab, the prompt while it has them, a
permission question you can answer where you stand. It exists because that is the one colour in
the program picked to sit beside *your* terminal theme rather than to mean something, and there is
no argument for it because nobody types a colour twice. Left out, it is `#1A936F`; `null` is the
terminal's own foreground colour, for a window that belongs to whatever palette it is opened in.

What it does not touch is the vocabulary. `ask` on the permissions tab, a budget bar past seven
tenths, a command that was killed, a pinned row — those are yellow because yellow *means*
something there, and they stay yellow against a frame of any colour. Red is left alone for the
same reason: a question nobody has come back to is red whatever you set, or you could configure
away the difference between *answerable now* and *still waiting*.

A colour that is not six hex digits stops the program and says so, naming the file and the form —
including in a headless run, which has no frame to draw. One file is valid everywhere or invalid
everywhere, rather than one that works until somebody opens it on a terminal.

**`tools` is the other one**, and it says which of the six a session starts with:

```json
{ "tools": ["fs", "shell", "context", "log", "setup"] }
```

Left out, every tool is offered, and the shipped file lists all six. `null` is refused: beside
`[]` for none it could be read either way. The list above is the whole set minus `fork`, which is
how a project says *do not go buying extra requests*. An empty list offers none of them, which is
a session with whatever an MCP server brought and nothing else. A name that is
not a tool stops the program and says which ones there are, for the same reason an unknown key
does: a file asking for `contxt` and quietly getting a session with no context tool is worse than
one that does not start.

There is no argument behind it because which tools a project wants its agent to have is settled
once and then not thought about again — and because the *other* thing it could be for, turning one
off for a while, is `/tools toggle ID` at the prompt, at the moment somebody wants it rather than
before the session starts. `/tools toggle` works on every tool, including the ones a server
brought, and a tool turned off this way is kept rather than thrown away: `/tools toggle` again
offers the same one back, still holding whatever it was remembering.

A leading `~` in `sandbox-allow` and `sandbox-read` is your home directory. That is the one place
this program expands one, because every other way of giving those paths has a shell in front of it
that expanded `~` before the program saw anything, and a file has nothing in front of it. The tools
still refuse a leading `~` rather than expanding it, because those paths are written by a *model*.

A key nothing reads is an error naming it, not a line that quietly does nothing: the program stops
and names the file, the key it did not know, and the keys it would have.

A key that is read but does not apply to this run is an error too, and it says which file it came
from. `deadline` and `on-ask` are the two, and `--serve` is the run they do not apply to: a served
session goes on for as long as somebody wants it, and a question in it is answered by whoever is
attached. An argument dropped on the floor is worse than one refused — `deadline: 300` beside
`--serve` reads as a run that ends by itself — and a project's file is the most likely place for
one to be dropped without a word, because whoever wrote it is not looking at this run's command
line. So `--serve` refuses both, from the file as well as from the flags, naming the path.

`on-ask: deny` in a file is not refused, because that is what the run would have used anyway, and
`--print-config` below writes it: a file that is the shipped one with nothing changed in it is
still a file a reader is meant to be able to serve from. Everything else in that key's place is
refused, `allow` included.

What it deliberately does not carry is anything belonging to one invocation rather than to the
project: a message, `-r`, `-f`, and `--headless`, which decides for itself from whether stdout is a
terminal.

**Given no `--config-file`, two places are looked in**: `./kamchatka.json`, and then
`kamchatka/kamchatka.json` under `XDG_CONFIG_HOME` or `~/.config`. The working directory first,
because a file sitting next to the thing it describes is the one you mean; nothing walks *up* from
there, because the surprise grows with the distance and typing the flag costs one flag. A file that
applies because of where you are standing is one that can surprise you, and the answer to that is
not to hide it — a session that picked one up says which file it read before anything else
happens: on standard error before a server is started or a provider reached, and again in the
conversation.

**`./kamchatka.json` is asked about before it is read**, because whoever wrote the directory wrote
it, and it may start MCP servers, turn the sandbox off and grant permissions. The question names
which of those it sets, and anything but `y` runs without it. A run with no terminal to ask at — a
pipe, a script, a served session with no screen — is refused instead, and `--config-file
kamchatka.json` is how it says the file is meant. The one under your config directory is yours,
and is read without asking.

A path you typed is not announced: you already know which file it was.

**A starting point ships with the crate**, as `kamchatka.json` beside this readme, and in the
archive a release attaches, beside the binary: every setting there is, so you edit rather than
remember, and every one of them at the program's own default. `cargo install` copies no files, so
the binary carries a copy too — `kamchatka --print-config > kamchatka.json` is the same bytes,
wherever you installed from. It holds the defaults, so a session run with it unchanged is the
session run with no file, whatever is typed beside it. It grants nothing — `allow` is empty, both sandbox lists are empty, `on-ask` is `deny` — and none of that is an oversight. A
default that pre-granted `read`, or opened up `~/.cargo` so that `cargo` works, would be this
program deciding on your behalf the one kind of thing it exists not to decide on your behalf —
and `~/.cargo` holds a registry token.

It narrows nothing either. A spend ceiling can look like the one thing a file adopted sight-unseen
can safely offer, but what it buys is a session that stops for a reason nobody chose, out of a
file whose whole claim is that it is the defaults. `spend` is here at
`null` with the rest, and `--spend`, `/spend` or one edit is how it stops being. The suite holds
the file to naming every key and to leaving the two that bound a session unset, so neither a
setting added later nor a number added here can go unnoticed.

## 🎛️ options

`kamchatka --help` lists every option, and the environment too rather than leaving its variables
for the readme alone to mention: the key, as `KAMCHATKA_API_KEY` or else `OPENROUTER_API_KEY` or
`OPENAI_API_KEY`; where the requests go, as `KAMCHATKA_BASE_URL`, which is OpenRouter unless it
says otherwise, or Google's own `v1beta` with `--gemini`; `KAMCHATKA_CONTEXT_LIMIT`, for a
provider that will not say how much context its model has; and `KAMCHATKA_NO_ATTRIBUTION`, which
stops the program naming itself to OpenRouter. The advisor's variables are listed by a build that
has an `--advise` to use them and by no other.

## 🚦 an advisor on the question

### a colour on the question

`--advise` is off unless the program was built with `--features shell-advisor`, and then it still
has to be asked for. What it adds is a question put to [TypeSafe](https://docs.typesafe.ai)'s
`jev` — a model that answers typed questions rather than writing text — about every shell command
you are about to be **asked** about, and its answer drawn in that question:

```console
$ export KAMCHATKA_SYSTEM1_API_KEY=apikey_...
$ kamchatka --advise "tidy up the build artifacts"
```

The feature puts the advisor in the binary and the flag is what starts one, so a build with
`shell-advisor` and a key in the environment but no `--advise` draws no ratings at all. The
permissions tab says so when that is the case, rather than leaving you to work it out from your
own build flags.

**Without a dedicated key it can borrow yours, in one case.** `jev` is served through OpenRouter as
well as by TypeSafe, so a session whose requests *already go to OpenRouter* can have an advisor
with only `KAMCHATKA_API_KEY` set — it asks `typesafe/jev-1.13` at
`https://openrouter.ai/api/alpha`, and the key paying for the conversation pays for the questions
too.

Any other session is refused and told why. A key is an OpenRouter key because it is being sent to
OpenRouter, not because of the variable it was read from — so a session pointed at ollama, at
Google with `--gemini`, or at a gateway of your own holds a key that service issued, and spending
it here would hand a third party a credential with no business with them. Those need
`KAMCHATKA_SYSTEM1_API_KEY`, and one started without it

```console
$ KAMCHATKA_BASE_URL=http://localhost:11434/v1 kamchatka --advise "…"
```

stops before the session begins, saying that it could not reach the advisor, which key to set,
and which address the session talks to.

The dedicated key is checked first, so setting it is what moves the questions to TypeSafe's own API
from anywhere. What the fallback changes is who is told: the arguments below go to OpenRouter as
well as to the model behind it.

`KAMCHATKA_SYSTEM1_BASE_URL` and `KAMCHATKA_SYSTEM1_MODEL` follow whichever key was found, and the
first moves the address without moving the account — pointing it at the other service means
naming that service's model with the second as well. TypeSafe resolves `jev-latest` to whatever
version is current; OpenRouter serves versions under their own names, which is why the identifier
this program sends there names one.

A borrowed key is only ever sent to OpenRouter. With `KAMCHATKA_SYSTEM1_BASE_URL` pointing anywhere
else - a local `laya-serve`, a proxy of your own - the advisor needs a dedicated key, and the
session stops before it begins saying so rather than hand that address the conversation's
OpenRouter key. Where the service there checks no key, any value will do.

Those two are also the whole of what a *third* service takes. The variables say `SYSTEM1` rather
than naming a company because the three question types are the category's — a claim to weigh, a
closed set, an ordered rubric — and an address this program does not recognise is read as keeping
TypeSafe's paths, which is the shape a self-hosted one has. So anything answering a `state` and a
map of typed questions there is reachable with those two set and nothing built. A service with a
*different* request shape is not, and is not planned; see [POSTPONED.md](../POSTPONED.md).

It **decides nothing**. What the standing rules allow runs without a question and without
anything being sent, what they refuse is refused, and what they ask about is asked about — an
`allow` is your decision, and a second model does not get to reopen it. So an advisor that is
unreachable, out of quota or unparseable costs the colour and nothing else, and the question says
so where the colour would have been: `the advisor could not rate this`, and why.

**Some commands are refused before the advisor reads them.** TypeSafe's API and OpenRouter's both
sit behind a firewall that turns a request away by what is in it, and what it turns away is the
command an advisor is most for: anything naming `/etc/shadow`, even in an `echo`, and a pipeline
that reads a secret into `curl`. The question says the advisor could not rate it and quotes the
firewall's page, so what is missing is the colour rather than the fact that it is missing. An
advisor on this machine, below, has no firewall in front of it.

**What leaves the machine**: for each shell command you are about to be asked about — which in a
default session is every command the model writes, since `exec:run` is a question by default — the
tool's id, the capabilities it declared, and its arguments, capped at 2KB per argument with the
cut named. Not the conversation, not the system instruction, not the model's own prose about why
it wants the call. A call the rules allow or refuse is sent nowhere. That disclosure is the reason
this is behind both a feature and a flag rather than on for anyone with a key in their
environment.

### an advisor on this machine

An engine running here is reached the way a third service is: `KAMCHATKA_SYSTEM1_BASE_URL` pointed
at it. [`laya`](https://github.com/NandhaKishorM/laya) serves itself over HTTP at the path the
hosted engine uses:

```console
$ pip install "laya[serve]"
$ laya-serve
$ export KAMCHATKA_SYSTEM1_BASE_URL=http://127.0.0.1:8000/v1 KAMCHATKA_SYSTEM1_API_KEY=local
$ kamchatka --advise -m qwen/qwen3-coder
```

The key is there because a borrowed one is only ever sent to OpenRouter, and laya checks none, so
any value will do. The point is not that it is free, though it is: **nothing leaves the machine**.
Everything the section above says about a third party reading a command stops applying, because
the command goes to a server you started, under your own user, and comes back as numbers.

What laya answers is laya's own, passed through: whether its `confidence` is the quantity
kamchatka reads has not been checked against a running `laya-serve` - see
[POSTPONED.md](../POSTPONED.md). If it is not, every command is drawn yellow, because a reading
nobody is sure of is never drawn green.

### what the colour says

The rating is drawn in the question, on the line under what the tool wants and above the
arguments: what the advisor reads the command as, in its colour, and how sure it is.

Green, yellow or red, off a three-level rubric — it only reads, lists, searches or changes
directory; it changes files inside the working directory, the way git or a rebuild could undo; it
reaches outside the working directory, destroys something that cannot be got back, or sends
something off this machine. You still have to read the command, which is what the panel under it
is for. What the colour buys is the half-second before that: whether this is the fifteenth
`cargo test` of the afternoon or the one call in fifty worth stopping on.

**The top of that rubric is asked a second time, as a claim rather than as a position**, and the
worse of the two answers is what gets drawn. The two engines are good at different halves of it:
an ordinal `score` is the primitive laya's own card calls its weakest, and asking the same reading
as a yes-or-no finds destructive commands the rubric misses; `jev` reads the rubric well and
misses some of them when the rubric is taken away. Folded, each draws at least as many of them
red as either reading alone. It costs a question and not a round trip — every question in a call
is answered in one pass at both engines, which is the same property that makes placing a command
stage by stage affordable.

Two rules keep it honest: a score is read by the level it is nearest rather than the one it has
passed, and a rating the advisor was not sure of is never drawn green and never drawn safer than it
scored — a distribution spread across a safety rubric is the advisor saying it could not tell,
which is not the same as a clean bill. The percentage is on the line so that a yellow you cannot
explain is visibly a yellow nobody was sure of.

## 💾 a session on disk

`/save` writes two files: a `.jsonl` of every event that happened, and a `.json` snapshot of the
context. Given a directory, it names them after the session, and the first save into one goes
beside a record already there under that name rather than over it - a resumed session has the
name of the one it carried on from. The snapshot has two ways back in.

`kamchatka -r PATH` starts a fresh session from it, which is the faithful one: the item numbers,
the model parameters and what the token counter had learned all come back exactly as they were,
because `Kernel::resume` is a constructor and builds the session around them. It also reads the
`.jsonl` of the same name, if it is still beside the snapshot, for the one thing a snapshot
cannot carry: what an item said before somebody rewrote it is an *event*, and without the record
a resumed session's `v1` pages would be empty. A record that is missing or was cut off mid-line
costs those pages and nothing else. What it cannot do is carry the lineage on: the resumed session's
log starts where the resume did, so a `/save` of it writes a record without the earlier rewrites in
it — the way two hops back is the `.jsonl` you kept.

`/load PATH` brings it into the session you are already in, which is the useful one. It is a
context operation and it plays by the same rule as the rest of them — nothing is destroyed. What
was in the context is **excluded**, keeping its numbers and its contents; anything **pinned**
stays where it is, because a pin is you saying so and `--system` is pinned; the loaded items come
in as new items with new numbers, and the conversation they were is read back onto the chat tab.
`/undo` twice puts the whole thing back — one <kbd>u</kbd> where there are keys to press. The
snapshot's model parameters replace yours, and neither puts those back, so the load says so when
that changes them.

That makes a checkpoint out of a file. `/save good`, let the agent go somewhere useless,
`/load good`, and carry on from where it was still working — without losing the detour, which is
sitting in the context marked `-` excluded if you want to read it.

### starting again

`/restart` is the other end of that. Where `/load` brings a file into the session you are in,
this writes the session out and puts a brand new one in its place — the same thing that would
happen if you quit and ran the program again, without quitting.

The first thing the new session says is that the old one ended, with the old one's name, where its
record and its snapshot went, and the `kamchatka -r` that carries on from it. It is the only place
those are still written down — so the run you just abandoned is a `-r` away for as long as
the temporary directory lasts. `--no-record` says so instead and writes nothing, as it does at
the end of a run.

**It goes back to the flags, not to where the session had got to.** The model is whatever `--model`
said, the permissions are `--allow` and `--deny` again, every tool `/tools toggle` switched off is
back, `--system` and `--file` are re-read, and the context is empty. What carries over is only
what cannot be rebuilt cheaply or at all: the connection to the provider, the MCP servers — whose
tools are installed into the new session rather than their processes being spawned again — and the
sandbox, which is a ruleset that cannot be lifted once it has been applied. A session started with
`-r` restarts into an *empty* one rather than back into the snapshot: the snapshot is where you
began, and this is you saying you are done with it.

A turn that is running is stopped to do it, rather than the command refusing until it finishes —
a model that has found a loop is the commonest reason to want a fresh session, and being told
*not while busy* is being told to wait for the thing you are escaping.

It works wherever a line does: at the prompt, down a pipe, and from a browser. A piped run
carries on reading the same input, so `do this` / `/restart` / `do that` runs the second half in
the new session. Clients attached over a socket are **disconnected** — their place in the log is
a record number in a log that no longer exists, so there is nothing to carry across — and they
reconnect into the new session by themselves if they retry, which `examples/browser.html` does.
`--connect` does not: it follows one session, so it ends with the old one, says so, and is run
again to attach to the new one. A host restarted at the same address is the same case from the
client's side, and `--connect` stops there too, naming the session it was following, rather than
carry its record stream on into another session's.

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
questions, real commands under a real Landlock ruleset, the introspection tools through the
real loop, a whole session driven by lines, the same session driven through a socket by two
clients at once, somebody else's MCP server spawned as a child process, and the settings file.
`cargo test -p kamchatka --no-default-features --features mcp` runs those and nothing else — which
is also the check that the screen really is optional, since a suite that only ever compiled with
it could not tell you.

`cargo test -p kamchatka --test live` is the third kind and wants a key: it asks whether a request
these keys produced is one a real API accepts, and whether the sentences these tools write are ones
a model can act on. A scripted provider agrees with every refusal it is handed.

## 🧰 a toolchain lives in your home directory

`$HOME` is not a system directory, so a confined command cannot read it — and most toolchains keep
their real installation there. `cargo` is a rustup shim, rustup reads `~/.rustup/settings.toml`
before it does anything at all, and a model asked to build a Rust project therefore gets a
`Permission denied` on that file, which looks exactly like a missing compiler. Hand it the
toolchain, for reading and no more:

```console
$ kamchatka --sandbox-read ~/.rustup,~/.cargo -m …
```

Read-only rather than `--sandbox-allow`, because a model that can *replace* the toolchain it is
about to run is not the trade anybody meant to make. The same goes for `~/.nvm`, `~/.pyenv`,
`~/.rbenv` and the rest. Both flags take a comma-separated list and may also be repeated, so a
checkout elsewhere and a scratch directory are one flag: `--sandbox-allow /srv/repo,/tmp/work`.

**A daemon you talk to over a socket needs one too**, on Linux 7.1 and up. A confined command may
connect to a unix socket only where it could have written one, so the session bus, the compositor
and a container daemon all come back `Permission denied`, because each of them runs what it is
asked outside the confinement. Where the error names the socket the shell says which it was, and
the way out is to hand over the socket rather than the directory it sits in:

```console
$ kamchatka --sandbox-allow /run/docker.sock -m …
```

`--sandbox-allow` rather than `--sandbox-read`, because connecting is the writing half of the rule:
what comes back from a socket is whatever the process behind it was willing to do. On an older
kernel there is no such right and every one of them was reachable all along. Under `--deny
fs:write` a path given to `--sandbox-allow` is read-only, as the working directory is, and a
socket among them is out of reach with the rest: refusing writes refuses the writing half here too.

An abstract socket - one with no file, which the X server and some session buses listen on - made
outside the confinement is refused too, on Linux 6.12 and up, and no flag hands one over: a
command may reach only the abstract sockets it made itself. An X client that is refused the
abstract one tries the file next, so `--sandbox-allow /tmp/.X11-unix/X0` is how a command gets a
display back, and with it every window on that display.

**Under `/dev` a command reaches five devices**, `null`, `zero`, `full`, `random` and `urandom`,
and nothing else: the rest of `/dev` is your other terminals, which a command could read what you
type into, shared memory, and on a desktop the camera and microphone. The list is
`--sandbox-device` and the settings file's `sandbox-device`, which replace it rather than add to
it, and the shipped `kamchatka.json` spells it out. A pty is the one thing a command may miss -
`script` and `expect` make one - and its far end is a file in `/dev/pts`, so having it back means
naming `/dev/ptmx` and `/dev/pts`, and with them every other terminal of yours.

**Git needs no flag.** Under Landlock `access(2)` still answers from the file's own permissions, so
git would ask whether `~/.gitconfig` is readable, be told yes, open it, get `EACCES` and take the
*unreadable configuration* branch — `fatal: unknown error occurred while reading the configuration
files`, and every git command in the session dead. So a confined command is handed
`GIT_CONFIG_GLOBAL` pointing at nothing when its configuration is out of reach, and git gets the
*no configuration* case, which it handles. A path you have set in `GIT_CONFIG_GLOBAL` yourself is
treated the same way: a reachable one is left as you set it, one out of reach is replaced rather
than left to kill every git command. Pass `--sandbox-read ~/.gitconfig` if you want your identity
and aliases in there too.

**A permission error says where it came from.** When a confined command is refused a path outside
its reach, the tool result names the path and says it is outside what this session reaches, so
the permission error is the confinement rather than the file's own permissions — along with what
the command runs with, and to work inside the working directory or ask for the path to be opened
up.

A refusal that names only paths the command *can* reach gets no such line: `cat /etc/shadow` is
refused with or without a sandbox, and hedging about it would send a model looking for a boundary
that had nothing to do with it. Reaching a path for reading is not reaching it for writing,
though: a write refused where the session reads and does not write - the working directory under
`--deny fs:write`, a `--sandbox-read` path - and that you could have made is named as the
confinement too.

**And it says where the session does reach**, in the tools that run in process too, in the same
words the `shell` tool's description uses. A refusal that named only the working directory would
read as the whole boundary, and a path opened up with `--sandbox-allow` would be one the model
never tried. So a refused path is answered with every place the session reaches, each marked
read-write or read-only, and told to work where it does or to ask for the path to be opened up and
say what it needs it for.

## 🌐 the network, when a command tries

Where the shell is confined, a command is asked about the network when it opens an internet socket
rather than for what it is called. The child that confines itself installs a seccomp filter after
the ruleset, and the filter holds every `socket()` for `AF_INET` or `AF_INET6` until this program
answers — from `net:reach` where it says `allow` or `deny`, and from you where it
says `ask`, once per command. So `--allow exec:run` runs `git status` without a question and asks
about `python3 fetch.py` the moment it looks a name up, which reading the command could never have
told apart.

A run with nobody at it answers with `--on-ask`, while the turn is still running — the command is
waiting on the answer, so waiting for the turn would be waiting for ever:

```console
$ kamchatka --headless --allow exec:run 'fetch the release notes'
```

says on stderr which command reached out and what it was answered, and the tool result the model
reads says it was asked and refused. A `--connect` client
given `--on-ask` answers the same way once its input has closed, and leaves it for another client
otherwise; a served session sends the question to its
client as it waits: `reaching` is the list, whole each time it changes, and `reach` answers one of
them the way `decide` answers the kernel's. They are two commands because the two questions are
numbered by different things — the kernel's by the kernel, and a running command's by the policy
holding it. `examples/browser.html` draws either kind in the same panel.

`deny` is every internet socket refused, so UDP as well, which the ruleset alone cannot refuse —
and a refused lookup comes back from most programs as `Temporary failure in name resolution`,
which is why the tool result says what it was.

Where the filter cannot be installed — `--no-sandbox`, or a kernel that cannot hold a call — the
program goes back to reading the command: a short list of
programs whose point is the network, a question about them before they run, and UDP not refused.
The permissions tab ends a confined shell's line with `network gated` or `network not gated`, so
which one a session has is on the screen rather than something to work out; under `--no-sandbox`
the line is `shell: a command can do any of these` instead.
