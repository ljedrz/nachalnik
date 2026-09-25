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
    | kamchatka --headless -m mercury-2 > session.jsonl
```

**stdout is the session log**, one JSON record per line — the same bytes `/save` writes, so
`jq 'select(.event.event == "tool.requested")' session.jsonl` is the whole of reading a run back.
**stderr is a person's half**: what the model said, what a tool was asked to do, and what any
command you sent answered. Every verb is there, because a command was never the keyboard's to
begin with.

Nothing can be asked at a prompt that is not there, so the answers are given in advance:

| flag | what it does |
| --- | --- |
| `--allow fs,exec:run` | answer `allow` for a whole domain, one operation in one (`--allow context:note`) or a path rule (`--allow '*.rs'`, `--allow 'vendor/'`) |
| `--deny fs:write,.env*` | the same, refused; the strictest of everything consulted still wins |
| `--on-ask deny` | what happens to a question nobody answered in advance. The default |

A path rule is a file name in which `*` stands for any run of characters, or one directory name
with a slash after it, which is about that directory wherever it sits in a path. It is not the
glob language `fs`'s own `glob` argument takes, and a pattern that reads like one — `src/**` —
stops the session rather than going onto the permissions tab as a rule no path can match.

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
at once if pressed again.

`--spend 50000` stops one too, because time is not the only thing one of these can spend: a model
that has found a loop — a tool that fails the same way, a question it keeps re-asking — will stay
inside any deadline you were willing to give it. The unit is tokens, `input + output` as the
provider reports them, because nothing here carries a price list and a figure in money would be one;
what a `fork` is charged counts the same as the session's own requests.
It is a stopping rule rather than a cap, since what a response cost is known only once it has
arrived. When the session stops, it says what had been spent against what ceiling, and that
`/spend N` raises it.

It belongs to the *session* rather than to this loop, which is why it is on
[`wiring::Setup`](#-embedding-it) and not on the headless driver. What stops the turn that crossed
the line is `App`, and so is what refuses the next one, so a screen session, a piped script and a
host with a loop of its own are held to the same number and none of them can get round it by not
asking. `/spend` says what has been spent and against what, `/spend N` raises it, and `/spend 0`
takes it away, which is the way back for whoever set it too low.

An endpoint that reports no usage at all says so, once, rather than holding a ceiling that nothing
will ever reach — a limit quietly never met is worse than no limit, because whoever set it is
reading the run as bounded.

`--deadline` is the one that needs nobody's cooperation — of the model, at least. What it cannot
cut short is a command of your own that is waiting on the endpoint: `/models` fetches a list, and
`/model` and `/provider` finish their switch before the next line is read, so a deadline that
falls during one of those is served when it returns.

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
$ kamchatka --serve unix:/run/user/1000/kamchatka.sock -m mercury-2
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
stderr — so it is a drop-in for it in a script. It answers a permission question with the same
three letters the panel takes, `y`, `n` and `a`; `ctrl+c` stops the turn and a second one detaches;
and `?4` prints what item 4 actually holds, which is the one thing a stream of records can never
say, because [the log names things rather than copying them](#-embedding-it). A running command
that [reaches for the network](#-the-network-when-a-command-tries) is answered with the same three
letters, once the kernel's own questions are.

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
host$  kamchatka --serve tcp:127.0.0.1:7878 -m mercury-2
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

Several clients can watch one session, and they see the same thing: the program has one voice, so
what a command answers and what the runtime says about a turn reach all of them. What it is *not*
yet is arbitration — every attached client may submit, interrupt and answer questions, there is
room for exactly one message queued into a running turn, and when a second client's line replaces
a first one's the session says so rather than letting a line disappear.

**A command that reaches for the endpoint is answered before the next one is**, because there is
one session and answering anybody needs it: `/models` fetches a listing, `/compact` runs a whole
pass, and a `/model` still settling is waited for before the next line is read. So one client's
`/models` at an endpoint that has gone quiet is every other client's wait, and where the session is
also drawn at a desk, the screen there does not redraw until it comes back. What that costs is
patience and nothing else — the kernel's own stream is read throughout, so nothing that happened
while it waited is lost to anybody.

The protocol itself is newline-delimited JSON, which is to say `nc` and `jq` read it. It lives in
[`remote`](https://docs.rs/kamchatka/latest/kamchatka/remote/) rather than in a crate of its own,
and the module documentation is where the argument is: why the records are the half that cannot be
lost and the fragments are the half that can, and why attaching answers with a projection rather
than with a snapshot.

`examples/attached.rs` is a client of a served session — attach, ask, follow the turn, refuse a
permission question, read back the item the answer was recorded as:

```console
$ kamchatka --serve tcp:127.0.0.1:7878 -m mercury-2 &
$ cargo run --example attached -- tcp:127.0.0.1:7878 "what is 2+2"
```

It reaches for `remote::protocol` and three plain data types, and for no part of this program that
runs a session — which is the check on the claim that `protocol` is what moves if something else
needs to speak this. Its header carries the wire transcript, because a client in another language
needs the JSON and none of the Rust.

And `examples/gateway.rs` with `examples/browser.html` put the session in a browser, which is the
one client that cannot reach it on its own: a browser has no TCP, so something has to terminate
HTTP in front. Three routes, no framework, no build step, and one HTML file.

```console
$ kamchatka --serve tcp:127.0.0.1:7878 -m mercury-2 &
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
record carries them. That is `project`, which every client has: it answers with the figures and
leaves the stream where it is, where an `attached` means *start again* — so a client that could not
tell the two apart would wipe its own screen to refresh a token count. The events view asks for one
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

Both providers answer one trait, `Dialect`, so `/model`, `/models`, `/provider` and the status
line work the same against either and nothing above them knows which wire format it got.

```console
$ export KAMCHATKA_API_KEY=...        # a Google AI Studio key
$ kamchatka --gemini "what does src/kernel.rs do?"
```

## 🔀 the model, and the address it lives at

`/model` says which model this is talking to, where that is, in what dialect, and how much context
it has.

`/model ID` switches the model and `/provider URL [ID]` switches the address — and the model with
it, because a model belongs to the address that serves it. Switching one and keeping the other is
how a session ends up asking the ollama on this machine for `gemini-3.6-flash`; given no model the
old name is kept and the new endpoint is asked whether it has one by that name, which is a notice
now rather than a 404 on the next request. Both matter for different reasons: comparing two hosted
models is one address and two names, while comparing a hosted model with the one running on this
machine is two addresses. A comparison that cannot see the address is a comparison of names.

Every session is written out when it ends, whether or not it ended well, and the last thing
printed is where: a record of its events and a snapshot of its context, both in a `kamchatka`
directory under the system's temporary one, and the `kamchatka -r` line that carries on from the
snapshot. A `SIGTERM` or `SIGHUP` — a closed terminal, `timeout`, `docker stop` — ends the session
the way `/quit` does: a running turn is stopped and waited for, and then the record is written.

A session's name is when it started, in UTC, so that a list of them says something to whoever is
reading it, and it is also the name of its two files.

Nobody has to type `/save` for any of this, because a session that ended badly is the one worth
reading afterwards. A resumed session keeps its name, and its record goes beside the one it carried
on from, as `NAME-2`, rather than over it. It is a temporary directory because this is a
safety net and not an archive — `/save PATH` is still how a session goes somewhere it will be next
week — and `--no-record` turns it off for anyone who would rather a transcript did not outlive the
terminal. The directory is `0700`: what goes in it is a whole conversation and every byte every tool
produced, written without anybody asking, and under an ordinary umask that would be a
world-readable file on a shared machine.

`/models [FILTER]` is what makes `/model` usable, because the ids belong to the endpoint rather
than to the model: the same thing is `google/gemini-3.5-flash` at one address and
`gemini-3.5-flash` at another, and after a `/provider` there is no other way to find out which
without guessing. It asks the endpoint, marks the one you are on with `▸` where it stands in the
endpoint's own order, and takes a filter because a list of everything an endpoint serves is not an
answer; a filtered list says how many of how many matched.

The key is *not* switched with the address. It is read from the environment once, at startup, and a
key typed at a prompt would be a key in the transcript — so `/provider` is for the addresses that
need no key or take the same one: a local model, a proxy, another base URL on the same account.

What is not switched either way is the context. The same items go to whatever answers next, which
is what makes the answers comparable. `/seams` names the six replaceable parts and what is in each
of them right now, asked of the kernel rather than restated from what this program set up at
startup.

`/params KEY JSON` sets one model parameter and `/params` shows them — with what else this model
takes, where the endpoint publishes it. It shows the second because a parameter a model does *not*
take is not refused: it is sent, ignored, and nothing anywhere says so, which makes a `seed` set
for a reproducible run buy no reproducibility and look exactly like one that worked. The runtime
invents none of them — only what you set is sent, apart from the `thinkingConfig` a `--gemini`
request asks for its thinking with, and a `generationConfig` of yours is merged over that — and a
listing that publishes nothing is read as silence rather than as a prohibition, because ollama and
a bare proxy both say nothing here.

Where the listing is everything the model takes, a parameter you set that is not on it is named as
sent and ignored, and the ones it takes that you have not set follow. Where the endpoint publishes
its sampling parameters only, the same absence settles nothing, so it is named as sent and
unchecked instead.

## 📏 the number in the status line is a guess, and says which kind

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
per-message framing, the tool schemas, and any payload it refuses to price. A PDF the counter
cannot put a number on is inside the provider's figure the moment it has gone out once.

The counter is the runtime's `Calibrating` one: every response tells it what the request it just
estimated really cost, and it adjusts. `/budget` says from how many requests, by what scale, and
what its own guesses came to against the provider's count. Over a real session against Gemini
it went from 13% low to within 0.3%.

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
for; see [the guards on a run nobody is watching](#-the-same-program-without-a-screen).

The compactor shortens the oldest tool results to a marker once the context passes `--compact`
(0.8 by default) of the limit. It does not summarize them — it never read them — and it touches
nothing that is pinned, because the kernel refuses. Every one of them is still on the context tab,
marked `…`, still holding every byte it held, one <kbd>space</kbd> from coming back.

A marker costs something too — it is a line of text where the content was — so the compactor
counts what each elision actually recovers and leaves alone any result no bigger than the marker
that would replace it: eliding a `wrote 412 bytes to …` would make the request *bigger*.

`/compact` asks that same compactor by hand, and shows its answer before anything happens: every
item it would take, with the identifier, what it is and what it is holding. It then waits, in the
prompt's place, for <kbd>y</kbd> or <kbd>n</kbd> — pinned rather than modal, like a tool's
question, so the context tab is a keystroke away while it stands and <kbd>p</kbd> there is the
answer to "not that one". Saying yes works the pass out again, so a pin made while reading the
list is honoured rather than refused after the fact.

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
.wire(kamchatka::endpoint::connect(Some("mercury-2")).await?)?;
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
  "model": "mercury-2.5",
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

**`border` and `tools` are the exceptions**, the two settings with no argument behind them:

```json
{ "border": "#7aa2f7" }
```

Six hex digits, `#` optional, and it is the colour of the window's frame — along with everything
else that is yellow to say *the keys are here*: the active tab, the prompt while it has them, a
permission question you can answer where you stand. It exists because that yellow is the one
colour in the program picked to sit beside *your* terminal theme rather than to mean something,
and there is no argument for it because nobody types a colour twice. Left out, or `null`, it stays
the terminal's own yellow — which is the right default precisely because it is not a hex: a window
with nothing configured belongs to whatever palette it is opened in.

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

Left out, or `null`, every tool is offered. The list above is the whole set minus `fork`, which is
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

What it deliberately does not carry is anything belonging to one invocation rather than to the
project: a message, `-r`, `-f`, and `--headless`, which decides for itself from whether stdout is a
terminal.

**Given no `--config-file`, two places are looked in**: `./kamchatka.json`, and then
`kamchatka/kamchatka.json` under `XDG_CONFIG_HOME` or `~/.config`. The working directory first,
because a file sitting next to the thing it describes is the one you mean; nothing walks *up* from
there, because the surprise grows with the distance and typing the flag costs one flag. A file that
applies because of where you are standing is one that can surprise you, and the answer to that is
not to hide it — a session that picked one up says which file it read, in the conversation, before
anything else happens.

A path you typed is not announced: you already know which file it was.

**A starting point ships with the crate**, as `kamchatka.json` beside this readme, and in the
archive a release attaches, beside the binary: every setting there is, so you edit rather than
remember, and every one of them at the program's own default. `cargo install` copies no files, so
the binary carries a copy too — `kamchatka --print-config > kamchatka.json` is the same bytes,
wherever you installed from. Copying it wholesale changes nothing at all: it is the program you
already have, written down. It grants nothing — `allow` is
empty, both sandbox lists are empty, `on-ask` is `deny` — and none of that is an oversight. A
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
firewall's page, so what is missing is the colour rather than the fact that it is missing. A local
advisor, below, has no firewall in front of it.

**What leaves the machine**: for each shell command you are about to be asked about — which in a
default session is every command the model writes, since `exec:run` is a question by default — the
tool's id, the capabilities it declared, and its arguments, capped at 2KB per argument with the
cut named. Not the conversation, not the system instruction, not the model's own prose about why
it wants the call. A call the rules allow or refuse is sent nowhere. That disclosure is the reason
this is behind both a feature and a flag rather than on for anyone with a key in their
environment.

### an advisor on this machine

`SYSTEM1_ADVISOR_COMMAND` points at a System One engine running here, and it is checked **before**
the key — set it and the three `KAMCHATKA_SYSTEM1_` variables are not read at all. The point is not
that it is free, though it is: **nothing leaves the machine**. Everything the section above says
about a third party reading a command stops applying, because the command goes to a process you
started, under your own user, and comes back as numbers.

```console
$ pip install laya
$ export SYSTEM1_ADVISOR_COMMAND="$HOME/ai/venv/bin/python kamchatka/contrib/laya_advisor.py"
$ kamchatka --advise -m qwen/qwen3-coder
```

[`laya`](https://github.com/NandhaKishorM/laya) is a library first, so the command is an
interpreter and a script, and `contrib/laya_advisor.py` is the script. laya also serves itself over
HTTP now, as `laya-serve`, at the path the hosted engine uses; POSTPONED.md says what pointing
`KAMCHATKA_SYSTEM1_BASE_URL` at it still wants. Most of it is comments, and the protocol is one JSON
object per line in and one per line out, in the body kamchatka already builds for the hosted
engine, because laya's question dicts and answers use the same three types under the same names.

The process is started once and kept, because a 421M-parameter checkpoint costs seconds to load
and milliseconds to run — loading it per question would put that wait in front of you every time
you were asked to press `y`. It is killed when the session ends.

The shim is an adapter and not a pipe. The two engines agree on the *question* shape and not on
the answer: laya keys its answers by the primitive with no `type`, and its `confidence` is its own
quantity rather than how concentrated the distribution is. Passed through, that is what turns `ls`
into a yellow line at 1% — kamchatka will not draw a reading nobody is sure of green, so a number
that is not a confidence makes every command yellow whatever it scored. The shim computes the
field the caller means and stamps the type from the question it asked.

If your `laya` answers under keys this does not expect, `--probe` says so without guessing:

```console
$ ~/ai/venv/bin/python kamchatka/contrib/laya_advisor.py --probe "ls -la"
```

It prints the state kamchatka sends and then what laya answered verbatim and what this shim would
send on. It asks the program's questions, word for word, of the state the
program sends: a probe that makes up its own measures something nobody runs. That goes for the
state as much as the rubric — a bare command line where the program sends the whole call answers
differently enough to be mistaken for a fact about the rubric. The state and the questions both
come from the same constants the program sends, and a test fails if either drifts.

An empty *would send on* is the translation not recognising what laya sent; a `score`
in the right place under a flat distribution is the engine finding the question hard, which is a
different problem and not one this file can fix. `--selftest` checks the translation against a
recorded answer and needs no checkpoint; `cargo test` runs it.

**Three of laya's own settings are not the ones it ships with**, because its model card says so:

- **The temperatures are refitted.** The card is explicit that the checkpoint ships over-confident
  and that one temperature per question type and option count has to be refitted on your own data
  before the probabilities mean anything — the shipped numbers were fitted on its domain, not this
  one. `contrib/laya_fit.json` is a set of labelled commands and `--fit` is what recomputes them
  from it, printing the working: for each bucket, the shipped temperature and the fitted one, and how
  each scores. Point it at a file of your own traffic if you have one:

  ```console
  $ python3 laya_advisor.py --fit           # or --fit path/to/your-own.json
  ```

  A temperature moves confidence and never the answer — accuracy is identical at every value — so
  what this changes is only whether a reading is drawn as sure. `noul` was too *sharp* and the fit
  pushes it the other way, and the rubric barely moved.
- **The token budget is raised** to 512 for the question and 1024 for the whole sequence. A stage
  of a command line travels in the question rather than in the state, and at the shipped 192 a
  long one is cut there — silently, unlike the `(cut; …)` the state's own cap leaves.
- **The checkpoint is chosen by script alone.** laya's router also guesses the language of Latin
  text from stopwords, which its card calls best-effort and which is meaningless on a command
  line: `python -c 'import os, sys'` reads as Portuguese, because `os` is a Portuguese stopword,
  and goes to a checkpoint the card's own table rates worse on English.

None of it makes laya good at this. Measured before the rubric drew its line at the working
directory, against the sixty commands the set held then, 21 of 30 destructive commands came out
red but *nothing* came out green — laya could not bring itself to say a command was safe, so 37
of 60 sat on the middle band. The card's own summary is the one to read — *a fast
base to specialise, not a zero-shot decision engine* — and its base checkpoint scores 0.362 on the
typed-decisions benchmark against a 0.461 majority-class baseline. What the settings above buy is
an advisor that draws anything red at all; what would buy more is fine-tuning, which is what
laya's notebook is for.

**Nothing it writes reaches your terminal.** Both its streams are held by kamchatka, which
matters most on the first run: `laya` downloads a checkpoint and says so at length, and a child
sharing your terminal would be writing over the screen ratatui is drawing. What the session is
told instead is two lines and no more — that the advisor is not ready yet, and then that it is.

The second one means it answered a question, not that it printed a word: the advisor is asked
one trivial thing as soon as it starts, and readiness is that coming back. So a shim that cannot
answer is found before a question depends on it, rather than at the first `y` — the
same startup check the hosted advisor gets from `Jev::probe`.

The last twenty lines of whatever the engine wrote are kept and hung on the end of the note that
says it failed, so a traceback shows up in the session rather than having scrolled past.

If it fails — the command is not there, it stops answering, a line does not parse, a question
takes longer than 30s — the pipe is closed and every later question says the advisor is gone,
rather than risking an answer being paired with the question before it. No question is coloured
from then on, which is what happens when the hosted one is unreachable too.

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

`/save` writes two files: a `.jsonl` of every event that happened, and a `.json` snapshot of the
context. The snapshot has two ways back in.

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
was in the context is **archived**, keeping its numbers and its contents; anything **pinned**
stays where it is, because a pin is you saying so and `--system` is pinned; the loaded items come
in as new items with new numbers, and the conversation they were is read back onto the chat tab.
<kbd>u</kbd> twice puts the whole thing back.

That makes a checkpoint out of a file. `/save good`, let the agent go somewhere useless,
`/load good`, and carry on from where it was still working — without losing the detour, which is
sitting in the context marked `▫` if you want to read it.

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

**Git needs no flag.** Under Landlock `access(2)` still answers from the file's own permissions, so
git would ask whether `~/.gitconfig` is readable, be told yes, open it, get `EACCES` and take the
*unreadable configuration* branch — `fatal: unknown error occurred while reading the configuration
files`, and every git command in the session dead. So a confined command is handed
`GIT_CONFIG_GLOBAL` pointing at nothing when its configuration is out of reach, and git gets the
*no configuration* case, which it handles. Pass `--sandbox-read ~/.gitconfig` if you want
your identity and aliases in there too.

**A permission error says where it came from.** When a confined command is refused a path outside
its reach, the tool result names the path and says it is outside what this session reaches, so
the permission error is the confinement rather than the file's own permissions — along with what
the command runs with, and to work inside the working directory or ask for the path to be opened
up.

A refusal that names only paths the command *can* reach gets no such line: `cat /etc/shadow` is
refused with or without a sandbox, and hedging about it would send a model looking for a boundary
that had nothing to do with it.

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
answers the same way once its input has closed, and a served session sends the question to every
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
