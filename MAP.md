# where things are

A file-by-file map of the workspace, with reasons wherever the layout isn't obvious from the names.
[AGENTS.md](AGENTS.md) has the short version.

---

## nachalnik

`nachalnik/src`:

| file | what lives there |
| --- | --- |
| `kernel/mod.rs` | `Kernel`, `State`, `StateChange`, the state machine, construction and resuming, and observing a session. |
| `kernel/context.rs` | the public operations on the context: push, set a state, replace, undo and redo, count, project, compact. |
| `kernel/components.rs` | the public operations on what a kernel is assembled from: provider, tools, policy, projector, counter, compactor, full notice, parameters. |
| `kernel/request.rs` | private: building a request, sending it, and repairing the call identifiers it came back with. |
| `kernel/calls.rs` | private: asking the policy about a model's tool calls, running them, recording what they produced. |
| `context/mod.rs` | `Context`: the items in order, identifiers handed out, undo/redo. |
| `context/item.rs` | `ContextItem`, `ContextId`, `ContextKind`, `ContextState`. |
| `model/mod.rs` | `Provider`, `Message`, `ModelRequest`/`Response`, `ToolCall`, `Usage`, `TooLong`/`Overrun`, `Params`. |
| `model/content.rs` | `Content`, `Blob`, `Part`, `Block` - what a message is made of. |
| `projection.rs` | `Projector`, `LinearProjector`, `Projection`, `Skipped` - context to wire messages. |
| `tool.rs` | `Tool`, `ToolSpec`, `ToolOutput`. |
| `permissions.rs` | `PermissionPolicy`, `Capability`, `Verdict`, `Grant`, `AskAlways`. |
| `tokens.rs` | `TokenCounter`, `BytesPerToken`, `Calibrating`. |
| `compaction.rs` | `Compactor`, `Budget`, `CompactionPlan`/`Report`. |
| `event.rs` | `Event` (everything observable about a session), `Delta`, `DeltaSink`, `OutputSink`. |
| `session.rs` | `Session`, `Record`, `Snapshot`, and `FORMAT`, their format version. `tests/records/` holds one of each event and a snapshot per format version: the schema for other programs that read them. |
| `config.rs`, `error.rs` | `Config` (with the reasoning for each default in the docs), `Error`. |
| `selectors.rs` | feature `selectors`: `17`, `tool:grep:latest`, `all:tool_results`, `file:src/foo.rs`. |
| `test.rs` | feature `test`: scripted providers, fake tools, table policies and a compactor. Use these rather than writing another mock. |

`nachalnik/examples`: `transparency`, `compaction` and `pricing_a_picture` need no key and run in
CI; `compare_models` and `panel` talk to a real API through `examples/common`.

---

## kamchatka

**`app/`** is the state. `mod.rs` is its public interface, `turn.rs` drives the kernel,
`questions.rs` handles the questions shown in place of the prompt (permissions and the like),
`events.rs` applies kernel events, and `spend.rs` is the spending limit. `transcript.rs` is the chat
as a person reads it, and `views.rs` is what only the screen needs. `going.rs` works out what the
next request does with each item; it's a separate file because the screen, the `context` tool and
a served session's protocol all use it, so the model and the person always see the same budget. `keys.rs` is
what the keys do, `command/` the slash commands, `session.rs` saving and loading sessions, and
`text.rs` formatting values as lines. `search.rs` (the `/` filter) and `when.rs` (trace
timestamps) are here rather than in `ui/` so a pane searches the same text it draws.

**`ui/`** draws and decides nothing: `mod.rs` the frame, `tabs.rs` the tab contents, `overlay.rs`
the floating panel, `markdown.rs` and `table.rs` render a model's answers as styled lines, and
`text.rs` wraps and fits text by the columns it takes on screen rather than by its characters.

**`tools/`**: `fs.rs` is the filesystem tool (`files.rs` for single files, `search.rs` for
searching), `shell.rs` the shell, including `joints`, which splits a command line into stages
(it's here rather than behind `tui` because it's about the command, not the display). `policy.rs`
is `Careful`, `reaching.rs` handles a running command waiting on the network gate, `shedder.rs` is
the compactor, `ops.rs` the table multi-operation tools are declared from, and `mod.rs` the
`Limits` and argument parsing shared by all tools. `advice.rs` (feature `shell-advisor`) is what
gets sent to the advisor about a command, and the one place that defines what leaves the machine
for it; the advisor only rates, it never decides.

**`introspect/`** is the tools an agent uses to inspect and manage its own session, one per
subject: `context/` (reading in `reads.rs`, changes and their history in `changes.rs`), `log`,
`setup` and `fork`. Written without any change to the runtime; it decides which operations a
*model* is allowed to do.

**`sandbox/`** is the Landlock ruleset the shell runs under (`confinement.rs`) and `Reach`, what
the in-process tools may open (`reach.rs`). Next to it, `gate.rs` is the seccomp filter that pauses
a command's internet sockets until the program answers; it's the only module in the workspace with
`unsafe` code. Read [SECURITY.md](SECURITY.md) before changing any of them.

**`wiring/`** is `Setup`, which assembles a session (subscribing to events comes before wiring, and
`introspect::install` returns a handle the caller keeps), plus `Setup::relaunch` (`/restart`) and,
in `record.rs`, saving a finished session. New setup goes in a field on `Setup`, not in
`main.rs`.

**The loops.** `main.rs` picks one and draws the screen; `headless.rs` reads stdin and writes the
log;
`remote/` serves a session over a socket (`protocol/` the wire, `server/` the session's side,
`client.rs` `--connect`), and `server::Serving` is the part the drawn loop shares. `web/` (feature
`webui`) is `--web`: the embedded page, `browser.html`, and the HTTP that relays browsers to a
served session as protocol clients. Limits on a
session, like the spending limit, live on `App`, which every loop goes through; `--deadline` lives
on `Headless`, because a time limit applies to a run, not a session.

The rest: `args.rs` the command line and `config.rs` the settings file it merges with,
`endpoint.rs` the environment variables and the `connect` functions, `check.rs` `--check`,
`reconcile.rs` `kamchatka reconcile`, `help.rs` the key and selector listings, `attach.rs` adds a
file to the context, `clipboard.rs` OSC 52, `stopping.rs` <kbd>ctrl+c</kbd>, and `mcp.rs` (feature
`mcp`) MCP servers.

**Features.** `tui` (default) covers `ui/`, `app/keys.rs` and the prompt: anything using a
`KeyEvent` or a `ratatui` type goes behind it, and nothing a *command* can reach may.
`cargo test -p kamchatka --no-default-features` is the check, and builds a headless binary.
`remote/` has no feature, since it only needs one more `tokio` feature. `webui` (default) is
`web/`.

`kamchatka/examples/`: `attached.rs` is a client written against `remote::protocol` and the three
`app` types it names; `gateway.rs` (feature `webui`) serves `web/`'s page for a session in another
process; `system1_assisted_compaction.rs` (feature `advise`) asks a System One model what to drop
from a full context; `recorded.rs` runs a headless session and saves it in four formats into the
git-ignored `recorded/`.

---

## nachalnik-providers

`openai/` is `OpenAiCompatible` (`mod.rs`), one request sent and read back (`wire.rs`), and the
Responses mode (`responses.rs`). `gemini.rs` and `anthropic.rs` are the other two dialects,
`endpoint.rs` the `Endpoint` and `Dialect` traits, and `conformance.rs` (its own feature) the suite
every dialect must pass. `waiting.rs`, `reading.rs` and `markup.rs` are private shared helpers:
sending and retrying, reading a stream one event at a time, and extracting the text from an error
body that isn't JSON. `system1.rs` (feature `system1`) is the client for System One models; it
isn't a `Dialect`, and its docs list the servers that support it and how they differ.
`attribution.rs` is the app headers the chat-completions, Anthropic and System One clients send to
OpenRouter.

The crate **reads no environment variables**: `kamchatka/src/endpoint.rs` reads `KAMCHATKA_*` and
`nachalnik-utils` reads `NACHALNIK_*`. A library that read `OPENAI_API_KEY` by itself could spend
someone's money with a key they set for a different program.

---

## nachalnik-mcp

`server.rs` is a connection to one MCP server, open as long as the `Server` exists, whose
`install` hands back which tools it installed for the caller to keep; `tool.rs` wraps one of its tools as a `Tool`, and defines `Trust`, how
much of what the server says is believed.

---

## nachalnik-eval

`subject.rs` is a `Kernel` that can be asked questions and awaited, `probe.rs` a question with a
declared answer format, `intervene.rs` and `fork.rs` change a copy of a `Snapshot` and run it once,
`trial.rs` is the append-only record, `score/` computes scores from it, `experiment.rs` has
`Experiment` and `Instrument`, `abreast.rs` limits concurrency without a new dependency, and
`suite/` holds the experiments, their descriptions, and the tools `instrumented` and `repair` give
the model being tested.
What the experiments measure is in its [README](nachalnik-eval/README.md).

**Be careful with `Instrument`.** Every outcome includes a hash of the text the experiment sends -
its dossiers, its templates and the fork's preamble, though not the answer-format sentence after
each question (see POSTPONED.md) - and `tests/machinery/` checks those hashes: if it fails, a
question changed, and earlier results are no longer comparable.

`examples/`: `bench` runs the suite against an OpenAI-compatible endpoint; `compare` and `pool`
read saved reports and send no requests.

---

## docs

`docs/` is the write-ups, served by GitHub Pages from `master` `/docs`, with no build step. Every
number in them comes from a recorded event log; if a claim stops being true, fix it with a new
recording, not by editing the text.
