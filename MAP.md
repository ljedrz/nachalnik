# where things are

A file-by-file map of the workspace, with the reason for a shape where the file name does not give
it. [AGENTS.md](AGENTS.md) has the one-line version.

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
| `event.rs` | `Event` (the whole observability story), `Delta`, `DeltaSink`, `OutputSink`. |
| `session.rs` | `Session`, `Record`, `Snapshot`, and `FORMAT`, the number both are written in. `tests/records/` holds one of each event and a snapshot per format: the schema a reader who is not this crate builds against. |
| `config.rs`, `error.rs` | `Config` (with the reasoning for each default in the docs), `Error`. |
| `selectors.rs` | feature `selectors`: `17`, `tool:grep:latest`, `all:tool_results`, `file:src/foo.rs`. |
| `test.rs` | feature `test`: scripted providers, fake tools, table policies and a compactor. Use these rather than writing another mock. |

`nachalnik/examples`: `transparency`, `compaction` and `pricing_a_picture` need no key and run in
CI; `compare_models` and `panel` talk to a real API through `examples/common`.

---

## kamchatka

**`app/`** is the state. `mod.rs` is what a caller may ask of it, `turn.rs` how it drives the
kernel, `questions.rs` how what stands in the prompt's place is answered, `events.rs` what a kernel
event does to it, and `spend.rs` the ceiling and what is charged against it. `transcript.rs` is the
chat as a person reads it, and `views.rs` what only the screen asks of it. `going.rs` is what the
next request does with each item - its own file because the screen, the wire and the `context`
tool all answer out of it, so the model's account of its budget and the person's cannot drift
apart. `keys.rs` is what the keys do, `command/` the slash commands, `session.rs` a session on disk,
and `text.rs` a runtime value as a line. `search.rs` (the `/` filter) and `when.rs` (the clock a
trace line is stamped with) are here rather than in `ui/` so a pane cannot search one string and
draw another.

**`ui/`** draws and decides nothing: `mod.rs` the frame, `tabs.rs` the bodies, `overlay.rs` the
floating panel, `markdown.rs` and `table.rs` a model's prose as styled lines, and `text.rs` the one
place a width is answered.

**`tools/`**: `fs.rs` the filesystem tool (`files.rs` for one file, `search.rs` for a walk),
`shell.rs` the shell and `joints` - where one stage of a command line ends, which is a fact about
the command, so it lives here rather than behind `tui`. `policy.rs` is `Careful`, `reaching.rs` a
running command waiting on the network gate, `shedder.rs` the compactor, `ops.rs` the table a
multi-operation tool is declared from, and `mod.rs` the `Limits` and the argument readers every
tool shares. `advice.rs` (feature `shell-advisor`) is what an advisor is asked about a command, the
one file where what leaves the machine for it is written down; it rates and never decides.

**`introspect/`** is the tools an agent inspects and manages its own session with, one per noun:
`context/` (reads in `reads.rs`, changes and their journal in `changes.rs`), `log`, `setup` and
`fork`. Written with no change to the runtime; what it adds is which operations a *model* may do.

**`sandbox/`** is the Landlock ruleset the shell runs under (`confinement.rs`) and `Reach`, what
the in-process tools will open (`reach.rs`). Beside it, `gate.rs` is the seccomp filter that holds
a command's internet sockets until the program answers - the one module in the workspace that
writes `unsafe`. Read [SECURITY.md](SECURITY.md) before changing any of them.

**`wiring/`** is `Setup`, how a session is assembled - the subscription comes before the wiring,
and `introspect::install` hands back a handle the caller keeps - plus `Setup::relaunch`
(`/restart`) and, in `record.rs`, where a session goes when it is over. If something new needs
setting up, it is a field on `Setup`, not a line in `main.rs`.

**The loops.** `main.rs` picks one and draws; `headless.rs` is stdin in and the log out;
`remote/` serves a session over a socket (`protocol/` the wire, `server/` the session's side,
`client.rs` `--connect`), and `server::Serving` is the part the drawn loop shares. `web/` (feature
`webui`) is `--web`: the embedded page, `browser.html`, and the HTTP that relays browsers to a
served session as protocol clients. Guards on a
session - the spend ceiling - live on `App`, which every loop comes through; `--deadline` lives on
`Headless`, because wall-clock time is a property of a run.

The rest: `args.rs` the command line and `config.rs` the settings file it merges with,
`endpoint.rs` the environment variables and the `connect` functions, `check.rs` `--check`,
`reconcile.rs` `kamchatka reconcile`, `help.rs` the key and selector listings, `attach.rs` one file
into the context, `clipboard.rs` OSC 52, `stopping.rs` <kbd>ctrl+c</kbd>, and `mcp.rs` (feature
`mcp`) somebody else's server.

**Features.** `tui` (default) puts `ui/`, `app/keys.rs` and the prompt behind it: anything taking a
`KeyEvent` or a `ratatui` type goes behind it, and anything a *command* can reach cannot.
`cargo test -p kamchatka --no-default-features` is the check, and builds a headless binary.
`remote/` has no feature: it costs one `tokio` feature and nothing else. `webui` (default) is
`web/`, which adds no dependencies either.

`kamchatka/examples/`: `attached.rs` is a client written against `remote::protocol` alone;
`gateway.rs` (feature `webui`) serves `web/`'s page for a session in another process;
`system1_assisted_compaction.rs` (feature `advise`) asks a System One model what a full context can
lose; `recorded.rs` runs a session headless and writes it out four ways, into the ignored
`recorded/`.

---

## nachalnik-providers

`openai/` is `OpenAiCompatible` (`mod.rs`), one request sent and read back (`wire.rs`), and the
Responses mode (`responses.rs`). `gemini.rs` and `anthropic.rs` are the other two dialects,
`endpoint.rs` the `Endpoint` and `Dialect` traits, and `conformance.rs` (its own feature) the suite
every dialect is held to. `waiting.rs`, `reading.rs` and `markup.rs` are crate-private and shared:
the send loop and retries, a stream read an event at a time, and the words out of a body that is
not JSON. `system1.rs` (feature `system1`) is the client for System One models - not a `Dialect`,
and its docs list the engines that answer it and where they differ. `attribution.rs` is the app
headers both clients send to OpenRouter.

The crate **reads no environment**: `kamchatka/src/endpoint.rs` reads `KAMCHATKA_*` and
`nachalnik-utils` reads `NACHALNIK_*`, and a library that picked up `OPENAI_API_KEY` on its own
would be spending somebody's money on a variable they exported for another reason.

---

## nachalnik-mcp

`server.rs` is a connection to one MCP server, living as long as the `Server` does, and what
installing its tools did; `tool.rs` is one of its tools as a `Tool`, and `Trust`, how much of what
the server says is believed.

---

## nachalnik-eval

`subject.rs` a `Kernel` that can be asked and waited on, `probe.rs` a question with a declared
answer shape, `intervene.rs` and `fork.rs` a frozen `Snapshot` changed on a copy and run once,
`trial.rs` the append-only record, `score/` the arithmetic read out of it, `experiment.rs`
`Experiment` and `Instrument`, `abreast.rs` bounded concurrency without a new dependency, and
`suite/` the experiments, their dossiers and the tools `instrumented` and `repair` hand a subject.
What the experiments measure is in its [README](nachalnik-eval/README.md).

**`Instrument` is the part to be careful with.** Every outcome carries a digest over every sentence
the experiment says, and `tests/machinery/` pins them: if it fails, a question changed and every
earlier run measured something else.

`examples/`: `bench` runs the suite against an OpenAI-compatible endpoint; `compare` and `pool`
read saved reports back and ask nothing.

---

## docs

`docs/` is the write-ups, served by GitHub Pages from `master` `/docs`, with no build step. Every
number in them is copied out of a recorded event log; if a claim stops being true, the fix is a new
recording rather than a new sentence.
