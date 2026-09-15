# where things are

A file-by-file map of the workspace, and the reasoning behind the shapes that are not
obvious from the file names. [AGENTS.md](AGENTS.md) has the one-line version.

---

`nachalnik/src`:

| file | what lives there |
| --- | --- |
| `kernel/mod.rs` | `Kernel`, `State`, `StateChange`, the state machine and every public operation. The big one. |
| `kernel/request.rs` | private: building a request, sending it, and repairing the call identifiers it came back with. |
| `kernel/calls.rs` | private: asking the policy about a model's tool calls, running them, recording what they produced. |
| `context.rs` | `Context`, `ContextItem`, `ContextId`, `ContextKind`, `ContextState`, undo/redo. |
| `model.rs` | `Provider`, `Content`, `Blob`, `Message`, `ModelRequest`/`Response`, `ToolCall`, `Usage`, `Params`. |
| `projection.rs` | `Projector`, `LinearProjector`, `Projection`, `Skipped` - context to wire messages. |
| `tool.rs` | `Tool`, `ToolSpec`, `ToolOutput`. |
| `permissions.rs` | `PermissionPolicy`, `Capability`, `Verdict`, `Grant`, `AskAlways`. |
| `tokens.rs` | `TokenCounter`, `BytesPerToken`, `Calibrating`. |
| `compaction.rs` | `Compactor`, `Budget`, `CompactionPlan`/`Report`. |
| `event.rs` | `Event` (the whole observability story), `Delta`, `DeltaSink`, `OutputSink`. |
| `session.rs` | `Session`, `Record`, `Snapshot`. |
| `config.rs`, `error.rs` | `Config` (with the reasoning for each default in the docs), `Error`. |
| `selectors.rs` | feature `selectors`: `17`, `tool:grep:latest`, `all:tool_results`, `file:src/foo.rs`. |
| `test.rs` | feature `test`: `ScriptedProvider`, `EchoTool`/`ConstTool`/`BrokenTool`, `AllowAll`/`DenyAll`/`Table`, `LargestFirstCompactor`. Use these rather than writing another mock. |

`kamchatka/src`: `app/` (the state - `mod.rs` is what a caller may ask of it and what a kernel
event does to it, `keys.rs` is what the keys do, `command.rs` is the slash commands and `text.rs`
turns a runtime value into a line), `wiring.rs` (`Setup`: the nine steps
a session is assembled in, two of which are not guessable - the subscription has to come before
the wiring, and `introspect::install` hands back a handle the caller has to keep),
`headless.rs` (the other loop: a line of stdin where the
terminal has a key, the session log on stdout and what a person reads on stderr), `help.rs` (the
key listing and the selector listing, which `/help` and the `amend` tool print), `config.rs`
(`Settings`: the JSON `--config-file` takes, one field per argument it stands in for - the merge
itself is `Args::under` in `main.rs`, because only clap can say which arguments were typed, and
the crate's own `kamchatka.json` is a starting point the suite holds to naming every field of it),
`mcp.rs` (feature `mcp`: somebody else's server spawned and its tools installed), `ui/` (drawing only - it decides nothing: `mod.rs` is the frame
and the chrome on it, `tabs.rs` the four bodies, `overlay.rs` the panel that floats over one,
`markdown.rs` and `table.rs` a model's prose turned into styled lines, `text.rs` the measuring and
fitting), `tools/` (the six tools - `files.rs` for the three that open one file, `search.rs` for the two that
walk a directory of them with ripgrep's engine, and `shell.rs` for the one that is a process - with
`policy.rs` for `Careful` and `trim.rs` for the compactor),
`introspect/` (the off-by-default tools an agent inspects and manages its own session with - one
per file, named for the noun each is about: `context` reads the context, `log` the record beside
it, `setup` what the session is running with, `amend` changes any of it - with `mod.rs` holding
`install` and the handful of things they all use),
`provider.rs` (**not a provider**: the four environment variables this program reads, and the two
`connect` functions that turn them into one), `main.rs` (arguments, and the loop that draws). It is
a library plus a binary so the screen can be drawn against a `TestBackend` in tests, and because
the screen is not the program.

**`main.rs` is arguments and a loop, and that is the shape to keep it in.** Everything it used to
assemble is `wiring::Setup`, because it was assembled twice - here and in `examples/recorded.rs` -
and an embedder would have written it a third time out of reading `main.rs`. Its callers now are
the program, that example, and the suites that drive a session with no screen. If something else
needs setting up, it is a field on `Setup` rather than a line in `main`.

**A guard on a session belongs to the session, not to the loop driving it.** The spend ceiling
started on `Headless` - where `--deadline` lives, and where it looked like it belonged - and every
caller that was not the program's own headless loop was unguarded by it, which is the embedder who
most needs one. It is on `App` now: `on_event` is the door all three loops come through, so that
is where the provider's figures are added up, and `start_turn` is where the next turn is refused,
so a caller cannot get round it by not asking. The deadline is the exception that proves the rule
- wall-clock time is a property of a *run*, and a screen session waiting for somebody to type is
not overrunning anything.

**`tui` is a default feature, and the line it draws is load-bearing.** `ui/`, `app/keys.rs`, the
prompt (`App::input`) and the two `ListState`s are behind it; `App` and everything else - the
session, the tools, the policy, the trace, `submit`, `interrupt`, `on_event`, `write_session` - is
not, and neither is `headless.rs`, which is the second caller that proves the first one is not
privileged. The rule for anything new: if it takes a `KeyEvent` or a `ratatui` type it goes behind the
feature, and if a *command* can reach it, it cannot. That is what `help.rs` and the two formatters
in `app/text.rs` are doing where they are: `/prune` prints the selector listing and `context`
formats token counts for a model to read, so neither can live in the module that draws.

`cargo test -p kamchatka --no-default-features` is the check, and every suite it runs is about the
program rather than the screen: `policy`, `sandbox` and `introspect` never draw, and `headless`
drives a whole session - a message, a command, a tool call, a question nobody can answer - through
an `App` that has no screen at all. CI runs it. Adding `--features mcp` adds the `mcp` suite, which
spawns a real server and grants it with `--allow mcp:py`: somebody else's tools with no terminal
anywhere, which is the configuration an embedder is most likely to be in. The binary builds in all
of that too and is headless in it, so `--no-default-features` is a program rather than a library.

`nachalnik-providers/src`: `openai/mod.rs` (`OpenAiCompatible`, where the requests go and what the
endpoint says it serves), `openai/wire.rs` (one request sent and read back, streamed or whole),
`gemini.rs` (Google's own, the one that keeps the order of a turn), `endpoint.rs` (the `Endpoint`
trait both answer), `waiting.rs` (the stall watch and the retry rules, `pub(crate)` because both
dialects use them), `conformance.rs` (the suite, behind its own feature). Each dialect is a
feature; `waiting.rs` is what makes them one crate rather than two.

This crate **reads no environment**. Where the requests go, which key pays for them and what limit
to measure against are arguments, and the two callers in this workspace supply them:
`kamchatka/src/provider.rs` reads `KAMCHATKA_*` and `nachalnik-utils` reads `NACHALNIK_*`. A
library that quietly picked up `OPENAI_API_KEY` would be spending somebody's money on the strength
of a variable they exported for another reason.

Two providers, one trait. `Provider` is the kernel's half - ask, and be answered - and `Endpoint`
is the caller's: where the requests go, what is served there, what the last retry was about.
`kamchatka`'s `App` holds an `Arc<dyn Endpoint>` and never finds out which wire format is behind
it, which is the claim `/seams` makes about every other part of the runtime and had not been true
of this one. `--gemini` picks the second, and turns on `LinearProjector::send_blocks` with it,
because that dialect's turn *is* an order and projecting three slots at it would flatten every
turn on the way out one request after recording the order on the way in.

`introspect/` is the second `nachalnik-mcp`: **written with no change to the runtime at all**, and
worth reading for that reason. Forking a context is `Kernel::snapshot` and `Kernel::resume`;
previewing a request is `preview_request`; pruning is `set_state`; reading the session's own record
is `with_history`, which is the mirror of `with_context` and is named for what it holds rather than
for its argument - so a grep for "session" on the kernel misses it and a design was written calling
for an accessor that had been there all along. What it adds is the part the
runtime has no opinion about - which of those a *model* may do. A pinned item, a system
instruction and the assistant turn carrying the call in flight are refused, `amend` may unpin only
what it pinned itself, and `undo` walks that tool's own journal rather than `Kernel::undo`, whose
stack belongs to the person and whose top during a turn is always the model's own question. There
is a tool per noun rather than one with a mode argument because a `ToolSpec` declares its
capabilities once: reading a context, reading the record beside it, reading what the session is
running with and rewriting the context have to be separately grantable or the grant delivers more
than it implies. It is also what makes each of
them separately *revocable*, which is a session worth recording in its own right.

`nachalnik-eval/src` is the third instance of the same test, and the one that is furthest from
the runtime's own concerns: `subject.rs` (a `Kernel` plus "ask, and wait for the turn to end"),
`probe.rs` (a question whose answer shape is declared, so a reading can parse it without a judge
model), `intervene.rs` and `fork.rs` (a frozen `Snapshot`, a `ContextState` moved on a copy of it,
and the copy run once with no tools), `trial.rs` (an append-only record, the way `Session` is,
plus `Act` - what a subject *did*), `score.rs` (the arithmetic, computed *from* the record),
`experiment.rs` (one trait method, a runner, and `Instrument`), and `suite/` (the eight
experiments, the two dossiers, `script.rs`, and `handles.rs`).

**What the crate is for is a ladder, and it is easy to miss the top of it.** `attribution`,
`recursion`, `lie`, `privilege` and `feedback` measure introspection *by report* - ask a model
what its answer rests on, and score the answer. That is all any harness can do. `instrumented`
and `repair` measure introspection *by experiment*: `suite/handles.rs` installs two tools a
subject can call, one that forks its own context and ablates an item and one that rewrites it, and
the argument is the difference between what a model *says* and what it finds out. Those two
experiments are the reason this crate is on this runtime rather than beside it.

`provenance` is off the ladder rather than on a rung of it, and reading it as an eighth report
experiment is the mistake to avoid. Every other experiment here asks the subject something and
scores what it said; this one asks the subject nothing at all. The harness writes a tool call, its
result and the answer drawn from it into a context, then runs copies with the result left alone,
elided and excluded, and asks each of them whether anything was run and whether that is the whole
of the conversation. Both answers have a ground truth because the harness wrote the record, so
nothing here is scored against a fork, and the `standing` arm is a base rate rather than a
control: a model that suspects tampering in an untouched context has not detected anything. Six
requests, the cheapest thing in the suite. The runbook and the methods document belong to a study
rather than to the instrument, and live in whichever repository ran it.

The one place it departs from the runtime's rules is prompt text, and the departure is contained:
everything above `suite/` does not know what a question is about, and the two tool descriptions
are prompt text too and are hashed like the rest. Four decisions decide whether a figure means
anything at all - the control condition is a *copy* rather than the live session, both arms are
blinded to the exchange in which the subject already answered, every claim is elicited before any
copy is run (`tests/harness.rs` asserts the ordering), and accuracy is never reported without the
majority baseline beside it.

`Instrument` is the part to be careful with. Every `Outcome` carries a stated version and an
FNV-1a digest over every sentence the experiment says, and `tests/machinery.rs` pins all eight. If
that test fails, a question changed and every run recorded before the change measured something
else. Adding a template nothing existing reads is safe and leaves the other digests alone; editing
one is not.

`nachalnik/examples`: `transparency` and `compaction` need no key and run in CI; `compare_models`
and `panel` talk to a real API through `examples/common`. `nachalnik-eval/examples/bench.rs` runs the
introspection suite against any OpenAI-compatible endpoint and writes the whole record out as
JSON; a local ollama works and costs nothing.

`nachalnik-eval`'s other two examples are the analysis half, and neither asks a model anything:
they read the saved `report.json` files back, which is the point of `--json` holding every
question and every answer. `compare` puts runs side by side and groups them by
`Instrument::digest`, so runs whose questions differ by a word are reported apart rather than
averaged together. `pool` computes the figures that are about *models* rather than about items -
the sign test over one run per model, which is honest there and nowhere else in the crate, since
models are independent of each other in a way that items sharing a dossier never are.

`kamchatka/examples/recorded.rs` runs a session headless and writes it out four ways - readable,
as events, as a snapshot, as the raw stream - which is how the first two transcripts under `docs/`
were produced. The third needed a conversation rather than a task, which `recorded.rs` cannot do:
it takes a brief and two tasks. That session ran the same `App`, tools and kernel with its turns
read from a file and `/save` typed at the end, and that harness is not in this tree yet. `PLANT`, `TASK`, `TASK2`, `BRIEF`, `DIALECT` and `OUT` parameterise it, and it
needs `KAMCHATKA_CONTEXT_LIMIT` set rather than setting one itself. It writes into `recorded/`,
which is ignored: a run measuring the repository it sits in must not find previous transcripts
lying in it.

`docs/` is the write-ups, served by GitHub Pages from `master` `/docs`. `index.html` lists them and
each piece is a directory with an `index.html` in it; `style.css` is shared by all of them. No
build step, no scripts. Every number in every one of them is copied out of a recorded event log;
if a claim in there stops being true, the fix is a new recording rather than a new sentence.
