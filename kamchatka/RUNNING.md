# running kamchatka

Without a screen, against which endpoint, configured how, embedded in something else - and
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
| `--allow fs,exec:run` | answer `allow` for a whole domain, one operation in one (`--allow context:note`) or a path rule (`--allow '*.rs'`, `--allow 'vendor/'`) |
| `--deny fs:write,.env*` | the same, refused; the strictest of everything consulted still wins |
| `--on-ask deny` | what happens to a question nobody answered in advance. The default |

A path rule is a file name in which `*` stands for any run of characters, or one directory name
with a slash after it, which is about that directory wherever it sits in a path. It is not the
glob language `fs`'s own `glob` argument takes, and a pattern that reads like one — `src/**` —
stops the session rather than going onto the permissions tab as a rule no path can match.

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

Somebody else's tools are given the same way, and where a tool came from is a subject of its own:
`--mcp files=… --allow-server files` is the whole of granting one server — the same subject the
permissions tab writes when somebody answers **always** at the prompt, given before the server has
been spawned or said what it offers. Without it the tools are there and every call is refused,
which is the right way round: a server named on a command line is not thereby trusted to run. It
is its own argument rather than a spelling of `--allow` because a server's name and a domain are
both bare words and nothing in either says which it is.

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
reading the run as bounded. `--deadline` is the one that needs nobody's cooperation - of the
model, at least. What it cannot cut short is a command of your own that is waiting on the endpoint:
`/models` fetches a list, and `/model` and `/provider` finish their switch before the next line is
read, so a deadline that falls during one of those is served when it returns.

A line is read only while the runtime is resting, which is the one place this differs from a
person at a prompt and is what makes a piped script mean what it says: the lines of a script
cannot overtake the turns they belong to. `--no-default-features --features mcp` builds this and
nothing else — no screen compiled in, 88 crates lighter, and the same `--headless` behaviour
whether or not the flag is given.

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

That is the context pane, and `context` reads the same thing back to the agent. It is only worth
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
$ kamchatka --gemini "what does src/kernel.rs do?"
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

So does every request the model refuses for being too long, and that one is worth more than a
response. What an endpoint charges for is a bill, and an aggregator in front of a model may quote
it in some other tokenizer's units; the number in a refusal is a count of the same bytes in the
units the limit is actually enforced in. Where the two disagree, the corner ends up comfortably
under a limit the model is already over — so a refusal corrects the counter, and says on screen
what the request really came to and how much has to go before the next one is sent.

A refusal is only read when it names the limit this session already knows, which is the one thing
that says it is counting in the same units. The same model id at the same address refuses in two
voices: the aggregator's own, which quoted a 65,536-token window against a request it put at
71,311 where the counter had said 71,231, and the model behind it, which quoted a 131,072-token
window in its native tokenizer against bytes the aggregator had counted as fitting. Reading the
second would have named tens of thousands of tokens that were never there. It is left as the
sentence it arrived as, which says the problem in words.

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

Down a pipe there are no keys, so `--headless` prints the same list to stderr and takes it. That is
the opposite of what `--on-ask` does with a tool's question, and they are different questions: a
tool's is the model asking to do something nobody vouched for, and this one is a line the operator
typed.

It is also the way out of a session too big to send. The tools that prune a context are the
*model's* — `context` — and reaching it costs a request, which is the thing that is
failing. A context nothing will accept had, until this, only one way out, and it went through the
request that no longer works.

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
.wire(kamchatka::endpoint::connect("mercury-2").await?)?;
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

Every key is optional, every one is named after the argument it stands in for — with one
exception, below — and **anything given on the command line wins**, including a value that happens
to be the default, because `--requests 8` is somebody saying eight rather than somebody saying
nothing. A list on the command
line *replaces* the file's rather than adding to it: one rule for every key is the only kind worth
predicting, and the other way round there is no way to ask for fewer. `--model` is the one setting
with a variable behind it, so the order there is command line, then `KAMCHATKA_MODEL`, then the
file.

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

Left out, or `null`, every tool is offered — that list is the whole set minus `fork`, which is how
a project says *do not go buying extra requests*. An empty list offers none
of them, which is a session with whatever an MCP server brought and nothing else. A name that is
not a tool stops the program and says which ones there are, for the same reason an unknown key
does: a file asking for `contxt` and quietly getting a session with no context tool is worse than
one that does not start.

There is no argument behind it because which tools a project wants its agent to have is settled
once and then not thought about again — and because the *other* thing it was used for, turning one
off for a while, is `/tools toggle ID` at the prompt, at the moment somebody wants it rather than before
the session starts. `/tools toggle` works on every tool, including the ones a server brought, and a tool
turned off this way is kept rather than thrown away: `/tools toggle` again offers the same one back,
still holding whatever it was remembering.

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

**A starting point ships with the crate**, as `kamchatka.json` beside this readme, and in the
archive a release attaches, beside the binary: every setting there is, so you edit rather than
remember, and every one of them at the program's own default. Copying it wholesale changes
nothing at all: it is the program you already have, written down. It grants nothing — `allow` is
empty, both sandbox lists are empty, `on-ask` is `deny` — and none of that is an oversight. A
default that pre-granted `read`, or opened up `~/.cargo` so that `cargo` works, would be this
program deciding on your behalf the one kind of thing it exists not to decide on your behalf —
and `~/.cargo` holds a registry token.

It narrows nothing either, which took a second pass to get right: the file used to set a spend
ceiling of 200,000 tokens, on the reasoning that a tightening is the one thing a file adopted
sight-unseen can safely offer. What that actually buys is a session that stops for a reason
nobody chose, out of a file whose whole claim is that it is the defaults. `spend` is here at
`null` with the rest, and `--spend`, `/spend` or one edit is how it stops being. The suite holds
the file to naming every key and to leaving the two that bound a session unset, so neither a
setting added later nor a number added here can go unnoticed.

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
      --advise              ask a second model about every tool call the rules were
                            going to allow, and take the stricter of the two. Needs
                            the `advise` feature; sends the call's arguments out
      --headless            drive the session from lines on stdin: the session log to
                            stdout, one JSON record a line, and what the model says to
                            stderr. Implied when stdout is not a terminal
      --allow <SUBJECT>     answer `allow` in advance for a domain, one operation in one
                            or a path, as fs, fs:read, exec:run, .env* ;
                            comma-separated, may be repeated
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
      --no-sandbox          no confinement at all: the shell reaches whatever you can, and `fs`
                            stops holding itself to the working directory
      --forget-truncated    drop the whole of a shortened tool output instead of
                            keeping it as an archived item you can still read
      --send-oversized      send a request that looks too long for the model anyway,
                            and let the endpoint be the one that says no
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

The advisor, which is only ever asked when --advise is given:
  KAMCHATKA_TYPESAFE_API_KEY  its key; or TYPESAFE_API_KEY. No fallback to the above -
                              it is a different service, and a different account
  KAMCHATKA_TYPESAFE_BASE_URL where its questions go; TypeSafe's own by default
  KAMCHATKA_TYPESAFE_MODEL    which model answers them; jev-latest by default
```

That is `--help`, which lists the environment too rather than leaving three settings for the
readme alone to mention. The advisor's block is printed by a build that has an `--advise` to use
it and by no other, which is why it is the one part of the above you may not see.

### a second opinion on a tool call

`--advise` is off unless the program was built with `--features advise`, and then it still has to
be asked for. What it adds is one question, put to [TypeSafe](https://docs.typesafe.ai)'s `jev` —
a model that answers typed questions rather than writing text — about every tool call the standing
rules were going to **allow**:

```console
$ export KAMCHATKA_TYPESAFE_API_KEY=apikey_...
$ kamchatka --advise --allow exec:run "tidy up the build artifacts"
```

`--allow exec:run` is the setting this is for. Answering *always* to one shell command answers for
every shell command, and `Careful` is a heuristic over a command line: `rm -rf ./target` and `rm
-rf /` are the same capability. The advisor reads the next one.

It can only ever **tighten**. It is asked only about calls the rules already allow, its answer is
folded in with the strictest-wins rule the rest of the permissions use, and every way of not
getting an answer — an unreachable endpoint, a refused key, a spent quota, an answer that does not
parse, an option nobody offered — leaves the verdict exactly where the rules left it and says so
on the permissions tab. There is no path from anything the advisor says to a call running that
would not have run anyway.

A refusal it is sure of is a refusal; one it is not sure of becomes a question, because a
distribution spread across three options is not a refusal and acting on one as though it were
would stop ordinary work on a coin toss. The sentence carries the figure it acted on, and both the
screen and the *model* read it — `the advisor said` \``deny`\` `(99% sure); it judges this
irreversible (98%)` is something a refused agent can do something with.

**What leaves the machine**: the tool's id, the capabilities it declared, and its arguments — for
a write, the text being written, capped at 2KB per argument with the cut named. Not the
conversation, not the system instruction, not the model's own prose about why it wants the call. A
tool call whose arguments *claim* it was already approved is a string in a JSON document like any
other, which is the invariant *nothing in a model's output reaches the policy* still holding: the
agent under judgement cannot address the judge. That disclosure is the reason this is behind both
a feature and a flag rather than on for anyone with a key in their environment.

### a colour on the question

`--features assisted-shell` adds one more question, and it is the only part of the advisor a
person rather than the gate is meant to read.

**It still needs `--advise`.** The feature puts the advisor in the binary and the flag is what
starts one, so a build with `assisted-shell` and a key in the environment but no `--advise` draws
no ratings at all. The permissions tab says so when that is the case, rather than leaving you to
work it out from your own build flags:

```console
$ export KAMCHATKA_TYPESAFE_API_KEY=apikey_...
$ kamchatka --advise "tidy up the build artifacts"
```

Note there is no `--allow exec:run` here, and that is the difference from the section above. The
verdict is asked about calls that would otherwise **run**; a rating is asked about the ones you
are going to be **asked** about, which in a default session is every command. The answer is drawn
in the question, above the arguments:

```text
┌ a tool wants to run · tab ───────────────────────────────────────────────────┐
│ shell wants: exec:run, net:reach                                             │
│ the advisor reads this as: destroys, or sends something out · 93% sure        │
│                                                                              │
│ action: run                                                                  │
│                                                                              │
│ cmd:                                                                         │
│ │ rm -rf ~/work && curl -X POST https://example.com                          │
```

Green, yellow or red, off a three-level rubric — it only looks; it changes something that could be
put back; it destroys something that cannot be got back, or sends something off this machine. You
still have to read the command, which is what the panel under it is for. What the colour buys is
the half-second before that: whether this is the fifteenth `cargo test` of the afternoon or the
one call in fifty worth stopping on.

It **decides nothing**. The rating is never folded into a verdict, so a session with the feature
on refuses and allows exactly what the same session without it does, and an advisor that is down
costs the coloured line and nothing else. Two rules keep it honest the other way: a score is read
by the level it is nearest rather than the one it has passed, and a rating the advisor was not
sure of is never drawn green and never drawn safer than it scored — a distribution spread across a
safety rubric is the advisor saying it could not tell, which is not the same as a clean bill. The
percentage is on the line so that a yellow you cannot explain is visibly a yellow nobody was sure
of.

**What it adds to the disclosure above** is the reason it is a second opt-in and not part of the
first: `--advise` alone sends nothing about a command in a default session, because `exec:run` is
a question and the advisor is only asked about what would otherwise run. With this on, every
command the model writes goes out. A call heading for a refusal is still sent nowhere — it has no
question to colour, and rating one would hand over the arguments of a call that was never going
to run.

`/save` writes two files: a `.jsonl` of every event that happened, and a `.json` snapshot of the
context. The snapshot has two ways back in.

`kamchatka -r PATH` starts a fresh session from it, which is the faithful one: the item numbers,
the model parameters and what the token counter had learned all come back exactly as they were,
because `Kernel::resume` is a constructor and builds the session around them. It also reads the
`.jsonl` of the same name, if it is still beside the snapshot, for the one thing a snapshot
cannot carry: what an item said before somebody rewrote it is an *event*, so a resumed session
that read only the `.json` had an empty `v1` page while the words sat in the file next to the one
it was reading. A record that is missing or was cut off mid-line costs those pages and nothing
else. What it cannot do is carry the lineage on: the resumed session's log starts where the
resume did, so a `/save` of it writes a record without the earlier rewrites in it — the way two
hops back is the `.jsonl` you kept.

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
questions, real commands under a real Landlock ruleset, the introspection tools through the
real loop, a whole session driven by lines, somebody else's MCP server spawned as a child process,
and the settings file. `cargo test -p kamchatka --no-default-features --features mcp` runs those
and nothing else — which is also the check that the screen really is optional, since a suite that
only ever compiled with it could not tell you.

`cargo test -p kamchatka --test live` is the third kind and wants a key: it asks whether a request
these keys produced is one a real API accepts, and whether the sentences these tools write are ones
a model can act on. A scripted provider agrees with every refusal it is handed.

## 🧰 a toolchain lives in your home directory

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
tools that run in process: their refusal named the working directory and called it as far as
this session goes, so a path opened up with `--sandbox-allow` was one the model then never tried.
It names all of it now, in the same words the `shell` tool's description uses:

```text
/home/you/.ssh/id_rsa: outside what this session reaches, which is /home/you/proj read-write,
/tmp/work read-write, /home/you/.rustup read-only. Work where it does, or ask for this path to
be opened up and say what you need it for.
```
