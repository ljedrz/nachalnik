# where things are

A file-by-file map of the workspace, and the reasoning behind the shapes that are not obvious
from the file names. [AGENTS.md](AGENTS.md) has the one-line version.

---

`nachalnik/src`:

| file | what lives there |
| --- | --- |
| `kernel/mod.rs` | `Kernel`, `State`, `StateChange`, the state machine and every public operation. The big one. |
| `kernel/request.rs` | private: building a request, sending it, and repairing the call identifiers it came back with. |
| `kernel/calls.rs` | private: asking the policy about a model's tool calls, running them, recording what they produced. |
| `context.rs` | `Context`, `ContextItem`, `ContextId`, `ContextKind`, `ContextState`, undo/redo. |
| `model.rs` | `Provider`, `Content`, `Blob`, `Message`, `ModelRequest`/`Response`, `ToolCall`, `Usage`, `TooLong`/`Overrun`, `Params`. |
| `projection.rs` | `Projector`, `LinearProjector`, `Projection`, `Skipped` - context to wire messages. |
| `tool.rs` | `Tool`, `ToolSpec`, `ToolOutput`. |
| `permissions.rs` | `PermissionPolicy`, `Capability`, `Verdict`, `Grant`, `AskAlways`. |
| `tokens.rs` | `TokenCounter`, `BytesPerToken`, `Calibrating`. |
| `compaction.rs` | `Compactor`, `Budget`, `CompactionPlan`/`Report`. |
| `event.rs` | `Event` (the whole observability story), `Delta`, `DeltaSink`, `OutputSink`. |
| `session.rs` | `Session`, `Record`, `Snapshot`. |
| `config.rs`, `error.rs` | `Config` (with the reasoning for each default in the docs), `Error`. |
| `selectors.rs` | feature `selectors`: `17`, `tool:grep:latest`, `all:tool_results`, `file:src/foo.rs`. |
| `test.rs` | feature `test`: `ScriptedProvider`/`TooLongProvider`, `EchoTool`/`ConstTool`/`BrokenTool`, `AllowAll`/`DenyAll`/`Table`, `LargestFirstCompactor`. Use these rather than writing another mock. |

`kamchatka/src`:

`app/` is the state. `mod.rs` is what a caller may ask of it and what a kernel event does to it.
`transcript.rs` is the chat as a person reads it: `Speaker`, `Entry`, `Said`, and what builds a
drawn line out of a context item and the lines that are not one. `views.rs` is what the screen
asks of it and nothing else does - the question waiting, the rows the context and permissions tabs
draw. `going.rs` is what the next request does with each item. That is a reading of the kernel
rather than a fact about this terminal, and it is its own file because the screen, the wire and the
`context` tool all answer out of it, so the model's account of its own budget and the person's
cannot drift apart. `keys.rs` is what the keys do, `command.rs` the slash commands, and `text.rs`
turns a runtime value into a line. `search.rs` is the `/` filter over a pane's rows and `when.rs`
the clock a trace line is stamped with; both are here rather than in `ui/` because a pane that
searched one string and drew another would find nothing where it says there is something.

`wiring.rs` is `Setup`: the nine steps a session is assembled in. Two of them are not guessable -
the subscription has to come before the wiring, and `introspect::install` hands back a handle the
caller has to keep. It also holds `record` and `Setup::relaunch`, where a session goes when it is
over and what `/restart` is, because an example driving a session with a loop of its own had
neither and lost every session it ran.

`headless.rs` is the other loop: a line of stdin where the terminal has a key, the session log on
stdout and what a person reads on stderr. `remote/` is the *third* loop, and a client for it:
`protocol.rs` is the wire, `server.rs` is a session with a socket in front of it and `client.rs` is
`--connect`. `server::Serving` is the half of that loop which is not a loop - the voice, the
questions and the bookkeeping - because the drawn loop in `main.rs` can serve as well, and a
session driven from a desk and a phone at once is one `App` with two things selecting on it.

`help.rs` is the key listing and the selector listing: `/help` and `/exclude` print them, and the
`context` tool hands the selector listing to a model. `config.rs` is `Settings`: the JSON
`--config-file` takes, one field per argument it stands in for. The merge itself is `Args::under` in
`args.rs`, because only clap can say which arguments were typed, and the crate's own
`kamchatka.json` is a starting point the suite holds to naming every field of it. `args.rs` is the
command line and what a session made of it looks like, in the library rather than in `main.rs`
because `examples/phone.rs` builds a session from the same flags. `clipboard.rs` is OSC 52, which is
how `/copy` hands the terminal an answer unwrapped and whole. `stopping.rs` is <kbd>ctrl+c</kbd>
subscribed once rather than once per turn round a loop, so a second press landing between two
iterations is not lost. `mcp.rs` (feature `mcp`) is somebody else's server spawned and its tools
installed.

`ui/` is drawing only, and decides nothing. `mod.rs` is the frame and the chrome on it, `tabs.rs`
the four bodies, `overlay.rs` the panel that floats over one, and `markdown.rs` and `table.rs` a
model's prose turned into styled lines. `text.rs` is the measuring and fitting: `columns` is the
one place a width is answered, and everything deciding what fits goes through it. `bullet`, which
reads a list marker, is there too, because knowing the shape of the thing being fitted is what
fitting it takes.

`tools/`: `fs.rs` is the filesystem tool, dispatching to `files.rs` for the three operations that
open one file and to `search.rs` for the two that walk a directory of them with ripgrep's engine.
`shell.rs` is the one tool that is a process, and holds `joints` - where one stage of a command
line ends and the next begins, which the permission question colours. Where a command comes apart
is a fact about the command rather than about drawing it, so `joints` cannot live in `ui/text.rs`
beside its one caller, behind `tui`. `reaching.rs` is the other kind of question: a running
command that reached for the network, held by the gate and waiting, which `Careful` keeps because
the shell and the `App` already share it. `policy.rs` is `Careful`, `trim.rs` the compactor, and
`ops.rs` what a tool that does several things declares - one table of operations, with the schema,
the refusal and `unread` all made out of it. `mod.rs` holds `Limits`, the domains this program's
own tools act in, and the argument readers every tool here shares - `arg`, `whole` and `truth`,
which `introspect/` reaches for too.

`tools/advice.rs` (feature `shell-advisor`) is what a model is asked about a command somebody is
about to be asked about, and the one file where what leaves this machine is written down. The
answer is `Rating`, which is drawn in the question and folded into nothing: the verdict is the
standing rules' alone. A command with joints in it is placed
stage by stage, every stage a question in the one request, and rated by `Rated::worst_of`. That
folds what each stage is *shown* as rather than what it scored, because the unsure rule is what
keeps a coin toss off green and folding the scores first loses it.

`introspect/` holds the four tools an agent inspects and manages its own session with, one per file
and named for the noun each is about. `context/` is the context, reading it and changing it - a
directory because the changing half is a file of its own, `changes.rs`, holding the journal `undo`
walks, the refusals and what a change cost. `log` is the record beside it, `setup` is what the
session is running with, and `fork` is a copy of the session, asked something. `mod.rs` holds
`install` and the handful of things they all use.

`sandbox.rs` is the Landlock ruleset the `shell` tool is re-executed under, `Reach` for what the
in-process tools will open, and `Confinement` for every way the first of those can fail to be
there - see [SECURITY.md](SECURITY.md) before changing any of it. `gate.rs` is the seccomp filter
the same child installs after the ruleset, which holds every internet socket a command opens until
the process that spawned it answers - the one module in the workspace that writes `unsafe`, and
Linux on x86_64 and aarch64 only. `attach.rs` is one file into the
context: the short table of media types this program is prepared to name, and text for everything
else. `endpoint.rs` is where the requests go: the environment variables this program reads, and
the `connect` functions that turn them into a provider and an advisor.

`advisor.rs` (feature `advise`) is a System One engine running on *this* machine, spoken to over a
pipe in the body `Jev` already sends. It is one long-lived child rather than one per question,
because the open engines load a checkpoint that costs seconds and answers in milliseconds. It is
here rather than in `nachalnik-providers` because that crate opens sockets and does not spawn
processes, which is the line `nachalnik-mcp` is on the other side of. It is how `laya` is reached
today: `contrib/laya_advisor.py` is the script `SYSTEM1_ADVISOR_COMMAND` names, and
`contrib/laya_fit.json` the labelled commands its temperatures are fitted on. Every failure closes
the pipe, because the next read off a doubtful stream is the answer to the question before it. Both
of the child's streams are held rather than inherited: a child sharing the terminal writes over the
frame, and a pipe nobody reads fills and blocks the child writing to it.

`main.rs` is which loop drives the session, the loop that draws, and where the record went. The
crate is a library plus a binary so the screen can be drawn against a `TestBackend` in tests, and
because the screen is not the program.

**`main.rs` is a choice of loop, and that is the shape to keep it in.** Everything it used to
assemble is `wiring::Setup`, because it was assembled twice - here and in `examples/recorded.rs` -
and an embedder would have written it a third time out of reading `main.rs`. Its callers now are
the program, that example, and the suites that drive a session with no screen. If something else
needs setting up, it is a field on `Setup` rather than a line in `main`.

**A guard on a session belongs to the session, not to the loop driving it.** The spend ceiling is
on `App` rather than on `Headless`, where `--deadline` lives: on `Headless`, every caller but the
program's own headless loop was unguarded by it, and that is the embedder who most needs one.
`on_event` is the door all three loops come through, so that is where the provider's figures are
added up, and `start_turn` is where the next turn is refused, so a caller cannot get round it by
not asking. The deadline stays on `Headless`, because wall-clock time is a property of a *run*,
and a screen session waiting for somebody to type is not overrunning anything.

**`remote/` passes the `nachalnik-mcp` test too.** Remote control usually wants a crate, a runtime
that knows about connections, and a second event vocabulary; this was written with **no change to
`nachalnik` at all**, and that is the acceptance criterion to hold anything in there to. What did
the work was already in the runtime: `Record`'s sequence numbers and `history_since`, so the
numbered stream is read out of the log per connection rather than fanned out by the server;
`Config::record_progress`, which is the runtime's own name for the line between what is recoverable
and what is not; `Kernel` being a cheap `Arc` handle, so a connection holds one of its own; and
`App::submit`, so every verb a person can type is one `Submit` rather than a command of its own.

**An event stream cannot render a conversation** - the log names things rather than copying them, so
a client fed nothing but records can follow a turn as it streams and cannot render a word of what
happened before it connected; attaching therefore answers with a *projection* (and deliberately not
a `Snapshot`, which carries every item's content), and `inspect` fetches the whole of one item on
demand. And **there is no authentication in the protocol and none is planned**: where it listens is
the whole of the boundary, a non-loopback bind is refused rather than documented against, and a
token in every message would be a scheme to keep in step protecting a channel whose real boundary is
the socket.

**No feature gate, alone among the optional-looking things here**, because of what it costs: one
`tokio` feature, `net`, and one method of `socket2` on a port - a crate `tokio` already builds for
that same feature. `tui` and `mcp` are features to keep the screen's dependencies and a child
process out of builds that want neither, and that argument does not transfer to a line in a
manifest.

**`tui` is a default feature, and the line it draws is load-bearing.** `ui/`, `app/keys.rs`, the
prompt (`App::input`) and the two `ListState`s are behind it; `App` and everything else - the
session, the tools, the policy, the trace, `submit`, `interrupt`, `on_event`, `write_session` - is
not, and neither is `headless.rs`, which is the second caller that proves the first one is not
privileged. The rule for anything new: if it takes a `KeyEvent` or a `ratatui` type it goes behind
the feature, and if a *command* can reach it, it cannot. That is what `help.rs` and the two
formatters in `app/text.rs` are doing where they are: `/exclude` prints the selector listing and
`context` formats token counts for a model to read, so neither can live in the module that draws.

`cargo test -p kamchatka --no-default-features` is the check, and every suite it runs is about the
program rather than the screen: `policy`, `sandbox` and `introspect` never draw, `headless` drives a
whole session - a message, a command, a tool call, a question nobody can answer - through an `App`
that has no screen at all, and `remote` drives one through a socket, from two clients at once, and
in `remote/program.rs` with the binary itself at both ends. CI runs it. Adding `--features mcp` adds
the `mcp` suite, which spawns a real server and grants it as `--allow-server py` would: somebody
else's tools with no terminal anywhere, which is the configuration an embedder is most likely to be
in. The binary builds in all of that too and is headless in it, so `--no-default-features` is a
program rather than a library.

`nachalnik-providers/src`: `openai/mod.rs` (`OpenAiCompatible`, where the requests go and what the
endpoint says it serves), `openai/wire.rs` (one request sent and read back, streamed or whole),
`gemini.rs` (Google's own, the one that keeps the order of a turn), `endpoint.rs` (the `Endpoint`
trait both answer), `waiting.rs` (the send loop, the stall watch and the retry rules),
`reading.rs` (a stream read an event at a time, and a server's sentence out of its error object),
`conformance.rs` (the suite, behind its own feature), `system1.rs` (feature `system1`: `Jev`,
TypeSafe's engine for typed questions answered with numbers, and the one thing here that is not a
`Dialect` - it drives no turn). Each dialect is a feature, and what it owns is what its events
*say*; `waiting.rs` and `reading.rs` are everything else, `pub(crate)` and shared, which is what
makes them one crate rather than two.

This crate **reads no environment**. Where the requests go, which key pays for them and what limit
to measure against are arguments, and the two callers in this workspace supply them:
`kamchatka/src/endpoint.rs` reads `KAMCHATKA_*` and `nachalnik-utils` reads `NACHALNIK_*`. A
library that quietly picked up `OPENAI_API_KEY` would be spending somebody's money on the strength
of a variable they exported for another reason.

Two dialects, one trait. `Provider` is the kernel's half - ask, and be answered - and `Endpoint`
is the caller's: where the requests go, what is served there, what the last retry was about.
`kamchatka`'s `App` holds an `Arc<dyn Dialect>` - both traits in one - and never finds out which
wire format is behind it, which is the claim `/seams` makes about every other part of the runtime.
`--gemini` picks the second and turns on `LinearProjector::send_blocks` with it. That dialect's
turn *is* an order, and projecting three slots at it would flatten on the way out every turn whose
order was recorded on the way in.

`introspect/` is the second `nachalnik-mcp`: **written with no change to the runtime at all**.
Forking a context is `Kernel::snapshot` and `Kernel::resume`; previewing a request is
`preview_request`; pruning is `set_state`; reading the session's own record is `with_history`, which
is the mirror of `with_context` and is named for what it holds rather than for its argument, so a
search for "session" on the kernel misses it. What `introspect/` adds is the part the runtime has no
opinion about - which of those a *model* may do. A pinned item, a system instruction and the
assistant turn carrying the call in flight are refused, it may unpin only what it pinned itself, and
`undo` walks that tool's own journal rather than `Kernel::undo`, whose stack belongs to the person
and whose top during a turn is always the model's own question. There is a tool per noun - the
context, the record beside it, what the session is running with, a copy of the session asked
something - and the noun is what the tool is *about* rather than what it does to it. Reading a
context and rewriting it can be one tool because a subject is `<domain>:<operation>`, so
`context:look` and `context:revise` are separately grantable under one tool's name. What the tool
boundary decides is what is separately *revocable*, and `fork` is on the other side of it because
buying a request is not reading.

`nachalnik-mcp/src`: `server.rs` is a connection to one MCP server and what installing its tools
into a kernel did; the connection lives as long as the `Server` does, and a server running as a
child process goes with it. `tool.rs` is one of that server's tools as a `Tool`, and `Trust`, how
much of what the server says about it is believed.

`nachalnik-eval/src` is the third instance of the same test, and the one that is furthest from the
runtime's own concerns: `subject.rs` (a `Kernel` plus "ask, and wait for the turn to end"),
`probe.rs` (a question whose answer shape is declared, so a reading can parse it without a judge
model), `intervene.rs` and `fork.rs` (a frozen `Snapshot`, a `ContextState` moved on a copy of it,
and the copy run once with no tools), `trial.rs` (an append-only record, the way `Session` is, plus
`Act` - what a subject *did*), `score.rs` (the arithmetic, computed *from* the record),
`experiment.rs` (one trait method, a runner, and `Instrument`), `abreast.rs` (independent work run
at once under a ceiling, written here rather than taken from `futures-util` so that nothing enters
the tree the runtime did not already need), `error.rs` (what can stop a measurement, as against what
a measurement finds), and `suite/` (the nine experiments, the six dossiers in `dossier.rs`,
`script.rs`, and `handles.rs`).

**What the crate is for is a ladder, and it is easy to miss the top of it.** `attribution`,
`recursion`, `lie`, `conflict`, `privilege` and `feedback` measure introspection *by report* - ask
a model what its answer rests on, and score the answer. That is all any harness can do.
`instrumented` and `repair` measure introspection *by experiment*: `suite/handles.rs` installs two
tools a subject can call, one that forks its own context and ablates an item and one that rewrites
it, and the argument is the difference between what a model *says* and what it finds out. Those two
experiments are the reason this crate is on this runtime rather than beside it.

`conflict` is the one of the report experiments worth reading before writing another, because it is
the one that carries a negative control. `lie` plants a contradiction the context can settle - the
brief says the records are accurate and the false note is not a record - so *which note is wrong?*
has an answer known before any model is asked. `conflict` plants the same contradiction with the
tiebreak taken away, both sides `records/...`, and asks whether the subject says so at all. The
detection question is put to copies that have the conflict in front of them, where yes is right,
*and* to copies with one side removed, where no is; a subject that says yes to both has reported
nothing, and without the second arm a detection rate is a count of the times a model said yes.

`provenance` is off the ladder rather than on a rung of it. Every other experiment here asks the
subject something and scores what it said; this one asks it nothing at all. The harness writes a
tool call, its result and the answer drawn from it into a context, then runs copies with the
result left alone, elided and excluded, and asks each of them whether anything was run and
whether that is the whole of the conversation. Both answers have a ground truth because the
harness wrote the record, so nothing here is scored against a fork, and the `standing` arm is a
base rate rather than a control: a model that suspects tampering in an untouched context has not
detected anything. Six requests, the cheapest thing in the suite. The runbook and the methods
document belong to a study rather than to the instrument, and live in whichever repository ran it.

The one place it departs from the runtime's rules is prompt text, and the departure is contained:
everything above `suite/` does not know what a question is about, and the two tool descriptions
are prompt text too and are hashed like the rest. A figure means something only because the
control condition is a *copy* rather than the live session, both arms are blinded to the exchange
in which the subject already answered, every claim is elicited before any copy is run
(`tests/harness.rs` asserts the ordering), and accuracy is never reported without the majority
baseline beside it.

`Instrument` is the part to be careful with. Every `Outcome` carries a stated version and an
FNV-1a digest over every sentence the experiment says, and `tests/machinery.rs` pins all nine. If
that test fails, a question changed and every run recorded before the change measured something
else. Adding a template nothing existing reads is safe and leaves the other digests alone; editing
one is not.

`nachalnik/examples`: `transparency`, `compaction` and `pricing_a_picture` need no key and run in
CI; `compare_models` and `panel` talk to a real API through `examples/common`.
`nachalnik-eval/examples/bench.rs` runs the introspection suite against any OpenAI-compatible
endpoint and writes the whole record out as JSON; a local ollama works and costs nothing.

`nachalnik-eval`'s other two examples are the analysis half, and neither asks a model anything: they
read the saved `report.json` files back, and `--json` holds every question and every answer so that
they can. `compare` puts runs side by side and groups them by `Instrument::digest`, so runs whose
questions differ by a word are reported apart rather than averaged together. `pool` computes the
figures that are about *models* rather than about items - the sign test over one run per model,
which is honest there and nowhere else in the crate, since models are independent of each other in a
way that items sharing a dossier never are.

`kamchatka/examples/`: `attached.rs` is a client of a served session, written against
`remote::protocol` and none of the code that serves one, which is the check on the claim that
`protocol` is all that moves between them. `gateway.rs` relays a browser to somebody else's
session; `phone.rs` is a session and a relay in one process, for when there is nobody at the
machine. The HTTP half of both is `relay/mod.rs` - `mod relay;` from each, the way `tests/common`
is included, and cargo builds no example out of a directory with no `main.rs` in it - and the page
it serves is `browser.html`, compiled in.
`jev_assisted_compaction.rs` (feature `advise`) asks TypeSafe's `jev` which tool results a full
context can afford to lose, against `Trim`'s oldest-first.

`kamchatka/examples/recorded.rs` runs a session headless and writes it out four ways - readable,
as events, as a snapshot, as the raw stream. It takes a brief and two tasks rather than a
conversation, and none of the write-ups under `docs/` came from it: those are ordinary sessions in
`kamchatka`, typed by hand. `PLANT`, `TASK`, `TASK2`, `BRIEF`, `DIALECT`, `INTROSPECT` and `OUT`
parameterise it, and it needs `KAMCHATKA_CONTEXT_LIMIT` set rather than setting one itself. It
writes into `recorded/`, which is ignored: a run measuring the repository it sits in must not find
previous transcripts lying in it.

`docs/` is the write-ups, served by GitHub Pages from `master` `/docs`. `index.html` lists them and
each piece is a directory with an `index.html` in it; `style.css` is shared by all of them. No
build step, no scripts. Every number in every one of them is copied out of a recorded event log;
if a claim in there stops being true, the fix is a new recording rather than a new sentence.
