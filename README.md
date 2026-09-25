# nachalnik

[![crates.io](https://img.shields.io/crates/v/nachalnik.svg)](https://crates.io/crates/nachalnik)
[![docs.rs](https://docs.rs/nachalnik/badge.svg)](https://docs.rs/nachalnik)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**An agent runtime in which the context, the tools, the permissions and the requests are explicit
state — state you can read, change and put back.** Not decisions taken inside a framework and
reported to you afterwards.

> The agent is not the boss. You are.

If you want an agent to run, that is [`kamchatka`](kamchatka), just below. If you want to build
one, the runtime is [`nachalnik`](nachalnik), with a [readme of its own](nachalnik/README.md);
everything else in this workspace is built on top of it.

---

### 🖥️ the agent you can run

`kamchatka` is a terminal agent that shows you everything it is carrying, and lets you change it.
Its **context** tab lists every item in the context: what it costs, whether it goes into the next
request, and — where it does not — why. <kbd>space</kbd> changes how much of an item the model gets,
<kbd>p</kbd> pins it so compaction cannot take it, <kbd>e</kbd> rewrites what it says, and
<kbd>u</kbd> undoes any of it. Nothing runs before you have been asked, reading a file included.

```console
$ cargo install kamchatka # or download a released binary
$ export KAMCHATKA_API_KEY=sk-or-...
$ kamchatka -m qwen/qwen3-coder "what does this repository do?"
```

![The context tab: five items with what each sends and holds back, and a pinned note opened to show
why it is there.][shot-context]

`/step` performs one transition of the loop at a time, which is the only way to stand in `Ready`:
the model has said which calls it wants to make, and none of them has run yet. The model gets
tools for its own session too, so it can read what it is carrying, drop what it no longer needs
and correct what turned out to be wrong — the five transcripts below are what that looks like.
[Its readme](kamchatka/README.md) has the sandbox, the keys and the rest.

**The screen is optional.** `--headless` drives a session from lines on stdin instead of keys —
the session log to stdout, one JSON record a line, what the model says to stderr — and it is
implied when stdout is not a terminal. Everything a run nobody is watching needs is a flag:
answers given in advance with `--allow`/`--deny`, a `--deadline`, and a `--spend` ceiling in
tokens.

---

### 📝 what it looks like when it runs

Five transcripts, at **<https://ljedrz.github.io/nachalnik/>**, quoted verbatim from the sessions
they describe. They read in order, and the machinery turns around halfway through: in the first
three the model is the one editing its context, and from the fourth on it is not.

1. **[a lie in its own notes](https://ljedrz.github.io/nachalnik/a-lie-in-its-own-notes/)** — two
   notes go in labelled as carried over from an earlier session, one of them false. It lists what
   it is carrying, checks the notes against the repository, and rewrites the wrong one in place.
2. **[retracting a hallucination](https://ljedrz.github.io/nachalnik/retracting-a-hallucination/)** —
   asked about a crate that did not exist when it was trained, it invents one twice. Told so, it
   finds both of its own turns and replaces them. Nothing is planted here, which is the caveat the
   first one carries.
3. **[an experiment on itself](https://ljedrz.github.io/nachalnik/an-experiment-on-itself/)** —
   asked which item its answer rested on, it went and checked, by asking a copy of itself the same
   question with that item taken out. Right about its own reasoning, wrong about where the item was
   filed.
4. **[putting words in its mouth](https://ljedrz.github.io/nachalnik/putting-words-in-its-mouth/)** —
   I replace two of its answers with confident falsehoods. By the third turn it is inventing a
   claim more specific than either of mine, with nobody editing that turn. Both real answers are
   still in the session, which is the only reason you can read them.
5. **[taking away the receipt](https://ljedrz.github.io/nachalnik/taking-away-the-receipt/)** — a
   shell command really runs, and then I hide its output, which takes down the turn that made the
   call as well. Asked how it knew, it answers correctly, and then retracts a true statement when I
   say I do not recall any command.

Every number and every quotation in them is copied out of the event log of the session it
describes, which is what an append-only log of typed events is for.

---

### 📦 the crates

| crate | what it is |
| --- | --- |
| **[`nachalnik`](nachalnik)** | the runtime: a loop that is a state machine, a context that is a list of identified values, and an append-only log of everything that happened. Five dependencies, no `unsafe`, no network, no prompt. Meant to stay boring. |
| **[`kamchatka`](kamchatka)** | a terminal agent built on the runtime — the thing you actually run, and the demonstration that the seams hold up under one. Also where the sandbox lives, because it is the program that spawns processes - and so Linux only; 0.15.1 is the last version that builds elsewhere. |
| **[`nachalnik-mcp`](nachalnik-mcp)** | a bridge to [MCP](https://modelcontextprotocol.io) servers, so that a tool somebody else wrote is a `Tool` like any other. |
| **[`nachalnik-eval`](nachalnik-eval)** | a benchmark for model introspection. A model commits to a claim about its own context, the harness moves the thing the claim was about on a copy, and the two are compared — so *"why do you think that?"* stops being unfalsifiable. |
| **[`nachalnik-providers`](nachalnik-providers)** | the two dialects — OpenAI chat-completions and Google's own — streamed, retried and interruptible, behind one trait. The runtime opens no sockets by design; this is where the sockets are. |
| `nachalnik-utils` | never published, permanently `0.0.0`. One file saying which endpoint the workspace's examples and live tests talk to, which key pays for it and which models to ask — so that scaffolding is written once rather than four times. A *dev*-dependency with no version: cargo strips those from a published manifest, so a crate only ever dev-depended on never has to exist on the registry. |

### 📖 the docs

| file | what it holds |
| --- | --- |
| [AGENTS.md](AGENTS.md) | orientation for whoever — person or model — is about to change this workspace: what is being built, what must not be broken, and which way the arguments have gone. |
| [INVARIANTS.md](INVARIANTS.md) | what must not be broken, each with the reasoning that put it there — break one and something in `tests/` should go red. |
| [MAP.md](MAP.md) | the file-by-file map, and the reasoning behind the shapes that are not obvious from the names. |
| [CONTRIBUTING.md](CONTRIBUTING.md) | the commands, what CI does, the house conventions in full with the mistake each came from, and the two gotchas that cost an afternoon each. |
| [SECURITY.md](SECURITY.md) | what is enforced, what is only reported, and why the core will never grow a sandbox. |
| [POSTPONED.md](POSTPONED.md) | known, decided against *for now*, each entry saying what would unblock it. |

Each crate's own readme says what it is and how to start; the longer material sits beside it —
[`kamchatka`'s guide](kamchatka/GUIDE.md) and [running it](kamchatka/RUNNING.md),
[`nachalnik`'s concepts](nachalnik/CONCEPTS.md), and
[`nachalnik-eval`'s running notes](nachalnik-eval/RUNNING.md).

---

### 🧩 do the seams hold?

The obvious question about a runtime this abstract is whether its six replaceable parts are real
seams or a diagram. Three crates in this workspace are the answer, and none of them needed a
change to the runtime to exist.

**[`nachalnik-mcp`](nachalnik-mcp)** is deliberately *not* in the core: speaking MCP means spawning
processes and reading notifications in the background, which the runtime promises not to do. An
MCP tool is a `Tool` that forwards to a server, and tools arriving and leaving are `add_tool` and
`remove_tool`. It believes none of a server's tool annotations by default, because the
specification calls them hints from an untrusted party; its tests include a tool called
`delete_everything` that claims to be read-only.

**[`kamchatka`](kamchatka)** hands the model four tools about its own session — `context`, `fork`,
`log` and `setup` — and every operation in them is a public function the screen was already
calling. None of them can touch what a person pinned.

**[`nachalnik-eval`](nachalnik-eval)** turns the same handles around and uses them to *test* a model
rather than to serve one: forking a context is `snapshot` and `resume`, previewing a request is
`preview_request`, pruning is `set_state`.

---

### 🧪 building and testing

```console
$ cargo test --workspace
```

Every crate has a suite, and each crate's readme says what its own covers. The live ones skip
themselves when there is no API key. [CONTRIBUTING.md](CONTRIBUTING.md) has the full command set,
what CI runs, and how to measure whether a test is worth keeping.

Among them is the provider conformance suite. What a provider makes of a stream is not tested one
provider at a time, because the questions would be the same each time. Every provider in the
workspace is asked the same ones through a real socket instead: each question is a bug that
actually happened to one of them, and a question added applies to all of them without any being
edited.

The live suites are the only way to check the things a mock cannot — that the requests this
workspace builds are accepted by a real API, and that a real model's answers survive the round trip
through a context:

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --workspace -- --test-threads=1
```

The figures in these readmes are measurements — what a request really cost, what a counter guessed
against what a provider charged, what a session did — taken against a real API where they say so.
There is no tally of the repository itself: `cargo test --workspace` and the tree have one that
stays current.

---

### 🚧 status

Early, but complete for what the runtime claims to cover: the state machine, the context model,
permissions, the event stream, sessions, and projection. Deliberately **not** included, and not
planned for the core: MCP, subagents, an editor protocol, a daemon, a CLI, or a prompt library.
Those belong on top of it, and that is what the rest of the workspace is for.

The crates follow [semver](https://semver.org/), and API breakage is to be expected before `1.0`.

---

### 🎸 the name

*Nachalnik Kamchatki* — "the boss of Kamchatka" — is a 1984 KINO album, named for the boiler room
where Viktor Tsoi shovelled coal while making it. A `nachalnik` is a boss, which is the joke: the
agent is not the boss, you are. `kamchatka` is the boiler room the work actually happens in.

---

### 📜 license

Licensed under the MIT License ([LICENSE-MIT](LICENSE-MIT)).

[shot-context]: https://github.com/ljedrz/nachalnik/raw/HEAD/kamchatka/assets/context.jpg
