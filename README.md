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
It sends no system prompt of its own and no telemetry: what reaches the model is the context that
tab lists and the tool definitions, and <kbd>ctrl+p</kbd> prints the whole request before it goes.

```console
$ cargo install kamchatka # or download a released binary
$ export KAMCHATKA_API_KEY=sk-or-...
$ kamchatka -m qwen/qwen3-coder "what does this repository do?"
```

**Any model, hosted or local.** OpenRouter is only the default address: anything that speaks
OpenAI's chat completions is one `KAMCHATKA_BASE_URL` away, and that includes a model running on
your own machine under ollama, vLLM or LM Studio, which needs no key at all. `--gemini` speaks
Google's own API, `--anthropic` Anthropic's own Messages API and `--responses` OpenAI's Responses
API, all of which keep a turn's thinking, text and calls in the order they came and send the
signed or sealed thinking back.

```console
$ KAMCHATKA_BASE_URL=http://localhost:11434/v1 kamchatka -m qwen3-coder "what does this repository do?"
$ ANTHROPIC_API_KEY=sk-ant-... kamchatka --anthropic -m claude-haiku-4-5 "what does this repository do?"
```

**A shell sandboxed by the kernel, not by a list of forbidden commands.** Landlock confines what a
command can touch: it writes only where you let it and reads nothing private outside the working
directory. A seccomp filter catches the moment a command reaches for the network, a DNS lookup
included, and holds it there until it has an answer, which by default is yours: you're asked when
a command actually tries, not based on what it's called. [SECURITY.md](SECURITY.md) says where each of them
stops. A released binary reproduces byte for byte and carries a provenance attestation and its own
dependency list; [its readme](kamchatka/README.md#-installing) says how to check all three.

![The context tab: five items with what each sends and holds back, and a pinned note opened to show
why it is there.][shot-context]

`/step` runs the loop one transition at a time, so you can stop in `Ready`, where the model has
said which calls it wants to make and none of them has run yet. The model gets
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
| **[`nachalnik`](nachalnik)** | the runtime: a loop that is a state machine, a context that is a list of identified values, and an append-only log of everything that happened. A handful of dependencies, no `unsafe`, no network, no prompt. Meant to stay boring. |
| **[`kamchatka`](kamchatka)** | a terminal agent built on the runtime — the thing you actually run, with a confined shell, a permission policy in front of every call, and sessions that can be served and rejoined. Also where the sandbox lives, because it is the program that spawns processes — and so Linux only. |
| **[`nachalnik-mcp`](nachalnik-mcp)** | a bridge to [MCP](https://modelcontextprotocol.io) servers, so that a tool somebody else wrote is a `Tool` like any other. |
| **[`nachalnik-eval`](nachalnik-eval)** | a benchmark for model introspection. A model commits to a claim about its own context, the harness moves the thing the claim was about on a copy, and the two are compared — so *"why do you think that?"* stops being unfalsifiable. |
| **[`nachalnik-providers`](nachalnik-providers)** | the three dialects — OpenAI chat-completions (OpenRouter, and local servers such as ollama, vLLM and LM Studio) with OpenAI's Responses API as a mode of it, Google's own and Anthropic's own — streamed, retried and interruptible, behind one trait. The runtime opens no sockets by design; this is where the sockets are. |
| `nachalnik-utils` | never published: shared code that picks the endpoint, key and models for the workspace's examples and live tests. |

### 📖 the docs

| file | what it holds |
| --- | --- |
| [AGENTS.md](AGENTS.md) | an introduction for anyone, person or model, about to change this workspace: what is being built, what must not break, and the design decisions made so far. |
| [INVARIANTS.md](INVARIANTS.md) | what must not be broken, each with the reasoning that put it there — break one and something in `tests/` should go red. |
| [MAP.md](MAP.md) | the file-by-file map, and the reasoning behind the shapes that are not obvious from the names. |
| [CONTRIBUTING.md](CONTRIBUTING.md) | the commands, what CI does, how to run a sweep, a live run or a soak, the house conventions in full, and the gotchas. |
| [SECURITY.md](SECURITY.md) | what is enforced, what is only reported, and why the core will never grow a sandbox. |
| [POSTPONED.md](POSTPONED.md) | known issues and ideas deliberately left for later, each with what would unblock it. |

Each crate's own readme says what it is and how to start; the longer material sits beside it —
[`kamchatka`'s guide](kamchatka/GUIDE.md) and [running it](kamchatka/RUNNING.md),
[`nachalnik`'s concepts](nachalnik/CONCEPTS.md), and
[`nachalnik-eval`'s running notes](nachalnik-eval/RUNNING.md).

---

### 🧩 do the seams hold?

The obvious question about a runtime this abstract is whether its six replaceable parts really
can be replaced. Three crates in this workspace show they can, and none of them needed a change
to the runtime.

**[`nachalnik-mcp`](nachalnik-mcp)** is deliberately *not* in the core: speaking MCP means spawning
processes and reading notifications in the background, which the runtime promises not to do. An
MCP tool is a `Tool` that forwards to a server, and tools arriving and leaving are `add_tool` and
`remove_tool`. It believes none of a server's tool annotations by default, because the
specification calls them hints from an untrusted party.

**[`kamchatka`](kamchatka)** hands the model four tools about its own session — `context`, `fork`,
`log` and `setup` — and every operation in them is a public function the screen was already
calling. None of them can touch what a person pinned.

**[`nachalnik-eval`](nachalnik-eval)** turns the same handles around and uses them to *test* a model
rather than to serve one: forking a context is `snapshot` and `resume`, what a copy will read is
`project`, pruning is `set_state`.

---

### 🧪 how it is tested

There is more test code in this workspace than code under test, and the test suites are only one
of several checks.

- **Every invariant is a test.** Each one [INVARIANTS.md](INVARIANTS.md) states has something in
  `tests/` that goes red when it breaks, and the ones that have to hold after *any* sequence of
  operations are property suites: random runs of pushes, state changes, replacements, undo and
  redo, with the context and its projection checked against each other after every step, and a
  snapshot that has to resume into the session it was taken from.
- **Tests are checked for whether they catch anything.** `cargo mutants` goes over each crate in
  turn, and every mutant that survives the whole workspace's tests either gets a test that catches
  it or a written reason it can't be caught. A ledger of those verdicts limits the next run to
  what has changed. Hand-written tests get the same check with `scripts/mutate.sh`: break the code
  a test is about, and see whether anything *other* than that test notices.
- **CI** runs the suites on Linux (x86_64 and aarch64), macOS and Windows with every feature on,
  builds with the defaults and with none, and checks the workspace on the MSRV, locked and at the
  lowest version of every direct dependency the manifests allow.
- **The live suites** send what this workspace builds to real APIs in all three dialects, because
  a mock cannot say whether an API accepts it.
- **The program is used as well as tested.** Again and again, and with many different models —
  large and small, free, stealth, a diffusion one — headless `kamchatka` sessions are turned on the
  workspace and on `kamchatka` itself. **Sweeps** audit the code, its performance, its quality, its
  tests and its docs, and measure what the tools cost a model using them. **Live runs** aim many
  short sessions at one corner each — hostile files, the sandbox and the network gate, output
  floods, every slash command with bad arguments, resumes and kills — some of them against a
  deliberately hostile fake endpoint. **Soaks** carry one long session across resumes, a SIGTERM,
  a SIGKILL, a context wall and an undo, and hold the whole chain of records to the invariants
  that span it, with a checker proven against deliberate corruptions before its verdict counts.

A finding from any of these only counts once a test or a session record confirms it. What gets fixed lands
with a test that fails without the fix; what needs a person's decision goes into
[POSTPONED.md](POSTPONED.md). The methods are in [`.claude/skills`](.claude/skills), written for an
agent to run.

```console
$ cargo test --workspace
```

The live suites skip themselves without a key:

```console
$ OPENROUTER_API_KEY=sk-or-... cargo test --workspace -- --test-threads=1
```

[CONTRIBUTING.md](CONTRIBUTING.md) has the full command set, what CI runs, the keys each live
suite reads, and how to run a sweep, a live run or a soak.

---

### 📌 status

Complete for what the runtime claims to cover — the state machine, the context model, permissions,
the event stream, sessions, and projection — with every invariant it states held by a test.
Deliberately **not** included, and not planned for the core: MCP, subagents, an editor protocol, a
daemon, a CLI, or a prompt library. Those belong on top of it, and that is what the rest of the
workspace is for.

The crates follow [semver](https://semver.org/), and API breakage is to be expected before `1.0`.

---

### 🎸 the name

*Nachalnik Kamchatki* — "the boss of Kamchatka" — is a 1984 KINO album, named for the boiler room
where Viktor Tsoi shovelled coal while making it. A `nachalnik` is a boss, which is the joke: the
agent is not the boss, you are. `kamchatka` is the boiler room the work actually happens in.

---

### 📜 licence

Licensed under the MIT License ([LICENSE-MIT](LICENSE-MIT)).

[shot-context]: https://github.com/ljedrz/nachalnik/raw/HEAD/kamchatka/assets/context.jpg
