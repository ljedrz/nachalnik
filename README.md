# nachalnik

[![crates.io](https://img.shields.io/crates/v/nachalnik.svg)](https://crates.io/crates/nachalnik)
[![docs.rs](https://docs.rs/nachalnik/badge.svg)](https://docs.rs/nachalnik)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**An agent runtime in which the context, the tools, the permissions and the requests are explicit
state — state you can read, change and put back.** Not decisions taken inside a framework and
reported to you afterwards.

> The agent is not the boss. You are.

This is the workspace. The runtime is [`nachalnik`](nachalnik) and has a
[readme of its own](nachalnik/README.md); everything else here is built on top of it, and is here
to show that it can be.

---

### 📦 the crates

| crate | what it is |
| --- | --- |
| **[`nachalnik`](nachalnik)** | the runtime: a loop that is a state machine, a context that is a list of identified values, and an append-only log of everything that happened. Five dependencies, no `unsafe`, no network, no prompt. This is the part that matters, and it is meant to stay boring. |
| **[`kamchatka`](kamchatka)** | a terminal agent built on the runtime - the thing you actually run, and the demonstration that the seams hold up under one. Also where the sandbox lives, because it is the program that spawns processes. |
| **[`nachalnik-mcp`](nachalnik-mcp)** | a bridge to [MCP](https://modelcontextprotocol.io) servers, so that a tool somebody else wrote is a `Tool` like any other. |
| **[`nachalnik-eval`](nachalnik-eval)** | a benchmark for model introspection. A model commits to a claim about its own context, the harness moves the thing the claim was about on a copy, and the two are compared - so *"why do you think that?"* stops being unfalsifiable. |
| `nachalnik-utils` | never published, permanently `0.0.0`. The OpenAI-compatible provider the workspace's examples and live tests talk through, so that scaffolding is written once rather than four times. A *dev*-dependency, which is the whole trick: cargo strips those from a published manifest, so a crate only ever dev-depended on never has to exist on the registry. |

---

### 🖥️ the agent you can run

```console
$ cargo run -p kamchatka -- -f src/lib.rs "what does this crate do?"
```

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│  id  label         kind               sending   held  what it says, or why it is not being sent            │
│  1 ▪ src/kernel.rs reference            1,045         pub struct Kernel;                                    │
│  2 · user          user_message             6         what does the kernel do?                              │
│  3 · assistant     assistant_message        7         asked for read                                        │
│  4 - read          tool_result              0     15  excluded: pruned at the terminal                      │
│  5 · assistant     assistant_message        7         asked for shell                                       │
│  6 … shell         tool_result             11  9,004  compaction: compacted to make room                    │
│  7 · assistant     assistant_message       62         The kernel is a state machine with five states. …     │
│                                                                                                              │
└────────────────────────────────────────────────────────────────────────────── 7 items, 2 not going, 1 elided ┘
┌ you ─────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ask for something, or /help                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 done · gpt-4o-mini @ openrouter.ai · ~1,168 tokens, 0.9% (128k) · 1,102 really · 15 held back · F1 for the keys
```

That second tab is the runtime: every item the context holds, what it costs, whether it is going
into the next request, and - for the ones that are not - why, on their own row, in the projector's
words. `space` cycles how much of an item the model gets, `p` pins it, `e` changes what it says,
`u` undoes. `/step` performs exactly one transition of the state machine, which is the only way to
stand in `Ready`: the model has said what it wants to do, and none of it has run yet.

A couple of thousand lines of ordinary user code on top of the crate - two providers, six tools, a
policy, a compactor and the drawing. See [its readme](kamchatka/README.md) for the sandbox, the
keys, and the rest.

---

### 🧩 do the seams hold?

The obvious question about a runtime this abstract is whether its six replaceable parts are real
seams or a diagram. Three crates in this workspace are the answer, and none of them needed a
change to the runtime to exist.

**[`nachalnik-mcp`](nachalnik-mcp)** is deliberately *not* in the core: speaking MCP means spawning
processes, opening sockets and reading notifications in the background, and the runtime promises to
do none of those. Writing it needed nothing added - an MCP tool is a `Tool` that forwards to a
server, tools arriving and leaving are `add_tool` and `remove_tool`, a structured result is
`Content::Json`. It pushed back on exactly one thing worth knowing: MCP tool annotations are
*hints*, and the specification says a client should never make tool-use decisions on hints from an
untrusted server, so the bridge believes none of them by default. Its tests include a server
offering a tool called `delete_everything` that claims to be read-only.

**[`kamchatka --introspect`](kamchatka)** hands the model the same context controls the keys give
you - `look`, `budget`, `request`, `draft`, `fork`, `prune`, `revise`, `note`, `undo` - and every
one of them is a public function a user interface was already calling. Given a 10,000-token limit
and a mundane question, one model's first move was `introspect({"action":"budget"})`; eight
requests later it elided eight tool results in one call and got two thousand tokens back, with
nothing destroyed.

**[`nachalnik-eval`](nachalnik-eval)** is the furthest from anything the runtime was designed for:
it turns those handles around and uses them to *test* a model rather than to serve one. Forking a
context is `snapshot` and `resume`, previewing a request is `preview_request`, pruning is
`set_state`, reading the budget is `budget`.

---

### 🧪 building and testing

```console
$ cargo test --workspace
```

460 tests, of which 34 are live suites that skip themselves when there is no API key: 188 in
`kamchatka`, 175 in `nachalnik`, 72 in `nachalnik-eval` and 25 in `nachalnik-mcp`. Each crate's
readme says what its own cover.

The live suites are the only way to check the things a mock cannot - that the requests this
workspace builds are accepted by a real API, and that a real model's answers survive the round trip
through a context:

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --workspace -- --test-threads=1
```

Every count and every percentage in this workspace's readmes was measured at the commit it was
written for, against a real API where it says so. They are there because a claim with a number in
it can be checked and a claim without one cannot - but the current answer is always
`cargo test --workspace`, not a page.

---

### 🚧 status

Early, but complete for what the runtime claims to cover: the state machine, the context model,
permissions, the event stream, sessions, and projection. Deliberately **not** included, and not
planned for the core: MCP, subagents, an editor protocol, a daemon, a CLI, or a prompt library.
Those belong on top of it - which is the point, and which is what the rest of the workspace is for.

The crates follow [semver](https://semver.org/), and API breakage is to be expected before `1.0`.

---

### 🎸 the name

*Nachalnik Kamchatki* - "the boss of Kamchatka" - is a 1984 KINO album, named for the boiler room
where Viktor Tsoi shovelled coal while making it. A `nachalnik` is a boss, which is the joke: the
agent is not the boss, you are. `kamchatka` is the boiler room the work actually happens in.

---

### 📜 license

Licensed under the MIT License ([LICENSE-MIT](LICENSE-MIT)).
