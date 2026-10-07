# kamchatka

[![crates.io](https://img.shields.io/crates/v/kamchatka.svg)](https://crates.io/crates/kamchatka)
[![docs.rs](https://docs.rs/kamchatka/badge.svg)](https://docs.rs/kamchatka)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**A terminal agent for Linux that gives you full control of the context.**

Built on [`nachalnik`][nachalnik], and everything that makes it an agent is its own: the tools, the
permission policy, the compactor, the confinement, the served sessions, the drawing, and the three
providers next door in [`nachalnik-providers`][providers]. The runtime supplies the state machine,
the context and the paper trail, and none of this needed a change to it.

It builds for Linux, on x86_64 and aarch64, and nothing else: its shell is worth handing a model
because Landlock confines it and a seccomp filter holds every attempt it makes to reach the network
until it has an answer, and both are Linux's.

```console
$ cargo install kamchatka # or download the released binary
$ export KAMCHATKA_API_KEY=sk-or-...
$ kamchatka -m qwen/qwen3-coder -f src/kernel.rs "what does the kernel do?"
```

OpenRouter is the default, not the only choice: `KAMCHATKA_BASE_URL` points it at anything that
speaks OpenAI's chat completions, a model served on this machine by ollama, vLLM or LM Studio
included (no key needed); `--gemini` and `--anthropic` speak Google's and Anthropic's own APIs, and
`--responses` OpenAI's Responses API. [Running it][running] has the details.

## ❓ who this is for

You're likely to find `kamchatka` compelling if any of these apply to you:
- you hate when the agent forgets an important piece of information, or can't trace its reasoning
  back to earlier points in the discussion
- you're dissatisfied with token accounting and auto-compaction being imprecise and unpredictable
- you worry about the supply-chain attack surface of large codebases
- you distrust generic community tools hosted by `npm`
- you want clear, fine-grained control over all the decisions taken by the agent
- you want the agent's shell sandboxed by the kernel rather than by a list of forbidden commands:
  it writes only where you let it, reads nothing private outside the working directory, and reaches
  the network only when you say so
- you want a minimalistic agent with negligible OS footprint and a transparent configuration
- you like to keep detailed, auditable, and local transcripts of past conversations

## 🖼️ what a session looks like

Asked to pin a budget and a deadline, the model writes the note itself with its `context` tool,
and the result it reads back says what the note did to its next request:

![The chat tab. Asked to pin a budget and a deadline, the model calls `context` to write a note;
the result says the note is pinned, goes into every request from then on, and what the next request
now costs.][shot-chat]

The context tab is the same session from your side: every item, what it puts into the next request
and what it holds back, and one opened on the page that says why it is there — in the model's
words, since the model put it there — and that it is pinned:

![The context tab. Five items with what each sends and holds back; the pinned note is selected and
open, showing why it is there and what it says.][shot-context]

## 🔍 nothing behind your back

**No instructions of its own.** `kamchatka` sends no system prompt. The only system text in a
session is what you give it with `--system` or a settings file, and that is a pinned row on the
context tab like any other item. Besides the context, what reaches the model is the tool
definitions, and `/budget` says what they cost.

**The request before it goes.** <kbd>ctrl+p</kbd> prints the next request as the runtime renders
it, naming every item left out and why; `/payload` prints what the provider will put on the wire,
field for field; `/raw` shows what came back from the last one.

**Compaction you can see and take back.** No model is asked to summarize your history. A tool
result the model has read and finished with becomes a one-line marker that says so, and once the
context fills, the oldest exchanges go whole. Every item taken is still a row on the context tab
with a note saying why, `/restore` brings it back, and a pinned item cannot be taken at all.
`/compact` shows a pass before it happens and waits for your answer.

**A token count that says it is a guess.** Nothing here has the model's tokenizer, so the figure
is written `~2,460`, and the percentage beside it names the limit it is a percentage of. Once a
response arrives it is anchored on what the provider actually charged, and the counter corrects
itself from every response it could price in full. Where something in the context has no price
on it, the figure is a floor and says so.

**No telemetry, no update check.** It talks to the endpoint you point it at, and to nothing else
you did not ask for: an MCP server you name, a session you serve, the advisor you turn on. A
command the shell runs is held at the network gate until it has an answer.

## 📦 installing

Download one of the binaries from [releases](https://github.com/ljedrz/nachalnik/releases), or
install the latest release using `cargo`:

```console
$ cargo install kamchatka                     # from the registry
$ cargo install --git https://github.com/ljedrz/nachalnik kamchatka
$ cargo install --path kamchatka              # from a clone
```

A release carries static x86_64 and aarch64 binaries. Building needs Rust 1.95 or newer and no
system libraries.

A released binary can be checked rather than taken on trust:

- **It reproduces.** The release workflow builds every binary a second time, from a checkout at
  another path with its crates fetched into another `CARGO_HOME`, and compares the two byte for
  byte. The release notes give the `rustc -V` it was built with, and
  [CONTRIBUTING.md][contributing] the command to rebuild it yourself.
- **It is attested.** Each archive carries a build provenance attestation tying it to this
  repository, the workflow and the tag, which a checksum beside it cannot do — whoever could
  replace the one could replace the other. `gh attestation verify ARCHIVE --repo ljedrz/nachalnik`
  checks it.
- **It says what is in it.** It is built with `cargo auditable`, so the binary carries its own
  dependency list, and `cargo audit bin kamchatka` checks the exact versions in it against the
  advisory database.
- **Its dependencies are held to a policy.** CI fails on a known vulnerability, an unmaintained
  crate or a yanked version, with nothing ignored, and on a licence the tree did not already need.
  Every action in the workflows is pinned to a commit.

## 🔧 what it comes with

Six tools — `fs`, `shell`, `context`, `fork`, `log`, `setup` — and a policy that asks about all of
it. Nothing is allowed on your behalf before you have been asked, reading a file included. A tool
is a *domain* and what it does is an *operation* in it, so `fs:read` is the subject and `fs` is
every one of them; answering **always** answers for one of those rather than for a tool's name,
which is what makes it work for tools this program has never heard of:

```console
$ kamchatka --mcp 'files=npx -y @modelcontextprotocol/server-filesystem /srv'
```

Those arrive through [`nachalnik-mcp`][nachalnik-mcp] declaring `mcp:call` and nothing else,
whatever their annotations claim, and where they *came from* is a subject of its own:
`--allow-server files` is one server and not the next one, which is why the `name=` is worth
giving.

`fs`'s `grep` and `glob` are ripgrep's engine linked in rather than shelled out to, and the reason
they exist is the subject they ride: without them, finding a symbol means `exec:run`, which
subsumes every other permission. As `fs:grep` and `fs:glob`, bound by the same path rules as a
read, they let a session be asked *about* a repository without handing over the one permission that
answers for everything — [what they cut and what they skip][guide-find].

Four of them are about the session itself: `context` reads the context and changes it, `log` reads
the record kept beside it, `setup` what the session is running with, and `fork` asks a copy of the
session a question. Every operation in them is a public function the screen was already calling —
[what each does][guide-introspect].

The registry is live: `/tools toggle shell` stops offering a tool from the next request and offers
it again the second time, with no restart. How much of a call's output the model is shown is live
too, per subject, with `/limit`, and a result that was cut keeps its whole beside it to send
instead. When a model has gone down the wrong path entirely, <kbd>d</kbd> at the permission prompt
drops *every* call it is waiting on, and the model is told.

## 🐢 one transition at a time

The loop is a state machine, and `/step` performs exactly one transition of it instead of a whole
turn. That is the only way to stand in `ready` — which the runtime documents as *a resting state
on purpose*, the moment the model has said what it wants and **nothing has happened yet**:

```text
/step how many lines are in ledger.py?
```

Where the model answers with a call — a `shell` running `wc -l`, say — the session stops in
`ready`: the command is decided, permitted, and not running. From here you can read it, prune the
context it would have run against, drop it, or `/step` again to run it. An "approve this command?"
prompt is a checkpoint put in front of the loop; here it is a state the loop itself stands in.

`/step` again for each transition — the tool runs, then the next request goes — or `/continue` for
the rest of the turn. While stepping, answering a permission does *not* quietly resume: you asked
to drive.

## 🎛️ features

Five features, three of them on by default. `--no-default-features --features tui` drops MCP
support and the `--mcp` flag with it. `tui` is another default, and it is the screen and the
keys: without it you get the same program, headless, and none of the crates that draw it. `webui`
is the third, and it is `--web`: the session as a web page, served on loopback or, if you give
it one, a local network address. It adds no dependencies.

`advise` is the fourth and is **off**: the client for a System One model, which answers typed
questions rather than writing text. `shell-advisor` is the fifth, also **off**, and adds
`--advise`, which colours each shell command you are asked about green, yellow or red by the
advisor's reading of it. The rating decides nothing. It is behind a feature and a flag because it
sends command lines off the machine; [running it][advise] says exactly what leaves.

## 📚 the rest of it

- **[Using it][guide]** — the four tabs and what each is for, everything the keys do, the
  permission prompt and what answering *always* commits you to, putting a file in, and the tools
  an agent reads and manages its own context with.
- **[Running it][running]** — headless, a session with a socket in front of it that you can walk
  away from, the same session in a browser, the three dialects and which endpoints work, what the
  number in the status line is a
  guess *at*, what happens when the context fills, a settings file, every option, embedding it in
  something else, and what a toolchain in your home directory needs.
- **[The changelog][changelog]**, and [`nachalnik`][nachalnik] for the runtime under all of it.

## 🎸 the name

`nachalnik` is an homage to KINO's *Nachalnik Kamchatki*. Kamchatka was the boiler room Viktor
Tsoi shovelled coal in; this is the one where the work actually happens.

## 📜 licence

MIT.

<!-- crates.io resolves a relative link against the directory this readme was published from,
     which is not where the repository root is. Links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[providers]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-providers
[nachalnik-mcp]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-mcp

[guide]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/GUIDE.md
[guide-find]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/GUIDE.md#-finding-things-without-a-shell
[running]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/RUNNING.md
[changelog]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/CHANGELOG.md
[contributing]: https://github.com/ljedrz/nachalnik/blob/HEAD/CONTRIBUTING.md
[guide-introspect]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/GUIDE.md#-letting-the-agent-read-and-manage-its-own-context
[advise]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/RUNNING.md#a-colour-on-the-question
[shot-chat]: https://github.com/ljedrz/nachalnik/raw/HEAD/kamchatka/assets/chat.jpg
[shot-context]: https://github.com/ljedrz/nachalnik/raw/HEAD/kamchatka/assets/context.jpg
