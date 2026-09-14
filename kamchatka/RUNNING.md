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
| `--allow read,shell` | answer `allow` for a capability, a path rule (`--allow 'src/**'`) or one action (`--allow amend:note`) |
| `--deny write,.env*` | the same, refused; the strictest of everything consulted still wins |
| `--on-ask deny` | what happens to a question nobody answered in advance. The default |

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

Somebody else's tools are given the same way. Every tool an MCP server offers arrives carrying
`mcp:<name>` and nothing else, so `--mcp files=… --allow mcp:files` is the whole of granting one
server — the same subject the permissions tab writes when somebody answers **always** at the
prompt, given before the server has been spawned or said what it offers. Without it the tools are
there and every call is refused, which is the right way round: a server named on a command line is
not thereby trusted to run.

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
reading the run as bounded. `--deadline` is the one that needs nobody's cooperation.

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
$ kamchatka --gemini --introspect "what does src/kernel.rs do?"
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

## 🧩 embedding it

The program is a library with a loop on top, and both halves are yours. `wiring::Setup` assembles
a session — the kernel, the policy, the four tools, the sandbox and the `App` around them — in the
order they have to go in, and hands back the two receivers a loop needs:

```rust
let wired = kamchatka::wiring::Setup {
    introspect: true,
    allow: vec![Subject::parse("read")],
    system: Some("you are working in a Rust workspace".into()),
    spend: Some(50_000),
    ..Default::default()
}
.wire(kamchatka::provider::connect("mercury-2").await?)?;
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
  "allow": ["read", "mcp:files"],
  "spend": 200000
}
```

Every key is optional, every one is named after the argument it stands in for, and **anything
given on the command line wins** — including a value that happens to be the default, because
`--requests 8` is somebody saying eight rather than somebody saying nothing. A list on the command
line *replaces* the file's rather than adding to it: one rule for every key is the only kind worth
predicting, and the other way round there is no way to ask for fewer. `--model` is the one setting
with a variable behind it, so the order there is command line, then `KAMCHATKA_MODEL`, then the
file.

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

**A starting point ships with the crate**, as `kamchatka.json` beside this readme: every setting
there is, so you edit rather than remember, and every one of them at the program's own default.
Copying it wholesale changes exactly one thing — it sets a spend ceiling of 200,000 tokens, which
is the only thing a file adopted sight-unseen can safely offer. It grants nothing: `allow` is
empty, both sandbox lists are empty, `on-ask` is `deny`, and none of that is an oversight. A
default that pre-granted `read`, or opened up `~/.cargo` so that `cargo` works, would be this
program deciding on your behalf the one kind of thing it exists not to decide on your behalf —
and `~/.cargo` holds a registry token. The suite holds the file to naming every key, so a setting
added later cannot quietly go missing from it.

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
      --introspect          offer the model the tools that read its own context, its own
                            record and what it is running with, and manage the first of
                            them; /introspect turns them on and off while it runs
      --headless            drive the session from lines on stdin: the session log to
                            stdout, one JSON record a line, and what the model says to
                            stderr. Implied when stdout is not a terminal
      --allow <SUBJECT>     answer `allow` in advance for a capability, a path or one
                            tool action, as read, shell, mcp:files, .env*, amend:note ;
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
      --no-sandbox          run the shell tool unconfined, reaching whatever you can
      --forget-truncated    drop the whole of a shortened tool output instead of
                            keeping it as an archived item you can still read
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
```

That is `--help`, which lists the environment too rather than leaving three settings for the
readme alone to mention.

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
three tools that run in process: their refusal named the working directory and called it as far as
this session goes, so a path opened up with `--sandbox-allow` was one the model then never tried.
It names all of it now, in the same words the `shell` tool's description uses:

```text
/home/you/.ssh/id_rsa: outside what this session reaches, which is /home/you/proj read-write,
/tmp/work read-write, /home/you/.rustup read-only. Work where it does, or ask for this path to
be opened up and say what you need it for.
```
