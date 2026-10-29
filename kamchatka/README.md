# kamchatka

[![crates.io](https://img.shields.io/crates/v/kamchatka.svg)](https://crates.io/crates/kamchatka)
[![docs.rs](https://docs.rs/kamchatka/badge.svg)](https://docs.rs/kamchatka)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**A terminal agent for Linux that gives you full control of the context.**

Built on [`nachalnik`][nachalnik], which provides the state machine, the context and the session
record. Everything else is `kamchatka`'s own: the tools, the permission policy, the compactor, the
sandbox, served sessions, the interface, and the three API clients in
[`nachalnik-providers`][providers]. None of it needed changes to the runtime.

It only builds for Linux, on x86_64 and aarch64, because its shell sandbox relies on Linux
features: its commands run in [sandlock](https://github.com/multikernel/sandlock) sandboxes, where
Landlock limits the files they reach to the ones they are granted, and a seccomp filter holds every
connection to an internet address until it is answered: by you, unless a rule answers first.

```console
$ cargo install kamchatka # or download the released binary
$ export KAMCHATKA_API_KEY=sk-or-...
$ kamchatka -m qwen/qwen3-coder -f src/kernel.rs "what does the kernel do?"
```

OpenRouter is the default, but `KAMCHATKA_BASE_URL` can point it at anything that supports OpenAI's
chat completions API, including a local model served by ollama, vLLM or LM Studio (no key needed);
`--api gemini` and `--api anthropic` use Google's and Anthropic's own APIs, and `--api responses`
OpenAI's Responses API. [Running it][running] has the details.

## ❓ who this is for

You may want `kamchatka` if any of these apply:
- you hate when the agent forgets important information or can't trace its reasoning back to earlier
  points in the discussion
- you find token accounting and auto-compaction imprecise and unpredictable
- you worry about the supply-chain attack surface of large codebases
- you distrust generic community tools hosted by `npm`
- you want fine-grained control over the agent's decisions
- you want the shell sandboxed by the kernel, not by a list of forbidden commands: it writes only
  where you let it, reads only the working directory, the paths you open up, the system
  directories it needs to run, a temporary directory of its own and a few devices, and reaches
  the internet only when you say so
- you want a minimal agent with a small OS footprint and transparent configuration
- you want detailed, auditable, local transcripts of past conversations

## 🖼️ what a session looks like

Asked to pin a budget and a deadline, the model writes the note itself with its `context` tool. The
result it reads back says what the note did to its next request:

![The chat tab. Asked to pin a budget and a deadline, the model calls `context` to write a note;
the result says the note is pinned, goes into every request from then on, and what the next request
now costs.][shot-chat]

The context tab shows the same session from your side: every item, what it adds to the next request
and what it leaves out. One item is opened to show why it is there (in the model's words, since the
model added it) and that it is pinned:

![The context tab. Five items show what each sends and holds back; the pinned note is selected and
open, showing why it is there and what it says.][shot-context]

## 🔍 nothing behind your back

**No hidden instructions.** `kamchatka` sends no system prompt of its own. The only system text in a
session is what you give it with `--system-prompt` or a settings file, and it appears as a pinned
row on the context tab like any other item. Two things that make a session of their own write an
instruction into it: a `fork` is told it is a copy with no tools, and a session
`kamchatka reconcile` makes says which notes came from which fork, pinned where you can read it.
Apart from the context, the model only receives the tool definitions, and `/budget` shows what they
cost.

**See the request before it's sent.** <kbd>ctrl+p</kbd> prints the next request as the runtime
builds it, listing every item left out and why. `/payload` prints exactly what the provider will
send. `/raw` shows the last response as it arrived.

**Compaction you can see and undo.** No model is asked to summarize your history. A tool result the
model has already used is replaced by a one-line placeholder. Once the context is full, the oldest
exchanges are removed entirely. Every removed item is still listed on the context tab with a note
explaining why, `/restore` brings it back, and pinned items are never removed. `/compact` shows what
it would do and waits for your answer.

**An estimated token count.** `kamchatka` does not have the model's tokenizer, so the figure is
shown as `~2,460`, and the percentage next to it says which limit it is a percentage of. Once a
response arrives, the figure is based on what the provider actually charged, and the counter adjusts
from every response it can fully price. When something in the context cannot be priced, the context
tab marks that item's figure with a `+`, and `/budget` says its totals are a minimum.

**No telemetry, no update check.** It talks only to the endpoint you configure and to things you
explicitly ask for: an MCP server you name, a session you serve, the advisor you turn on. Commands
run by the shell are paused at the network gate until you answer.

## 📦 installing

Download a binary from [releases](https://github.com/ljedrz/nachalnik/releases), or install the
latest release with `cargo`:

```console
$ cargo install kamchatka                     # from the registry
$ cargo install --git https://github.com/ljedrz/nachalnik kamchatka
$ cargo install --path kamchatka              # from a clone
```

A release carries static x86_64 and aarch64 binaries. Building needs Rust 1.95 or newer and no
system libraries.

You can verify a released binary instead of trusting it:

- **It's reproducible.** The release workflow builds every binary twice, from a different path with
  crates in a different `CARGO_HOME`, and checks the two are identical byte for byte. The release
  notes give the `rustc -V` it was built with, and [CONTRIBUTING.md][contributing] has the command
  to rebuild it yourself.
- **It's attested.** Each archive has a build provenance attestation proving it was built by this
  repository, workflow and tag. A checksum cannot prove that, because whoever could replace the
  archive could replace the checksum too. Check it with
  `gh attestation verify ARCHIVE --repo ljedrz/nachalnik`.
- **It lists its contents.** Built with `cargo auditable`, the binary contains its own dependency
  list, and `cargo audit bin kamchatka` checks those exact versions against the advisory database.
- **Its dependencies are checked.** CI fails on known vulnerabilities, unmaintained crates, yanked
  versions and new licences, with no exceptions. Every action in the workflows is pinned to a
  commit.

## 🔧 what it comes with

Six tools (`fs`, `shell`, `context`, `fork`, `log`, `setup`) and a policy that asks about all of
them. Nothing is allowed until you've been asked, including reading a file. Permissions are about
*domains* (like `fs`) and the *operations* in them (like `fs:read`), not tool names, so answering
**always** works the same for tools `kamchatka` has never seen:

```console
$ kamchatka --mcp 'files=npx -y @modelcontextprotocol/server-filesystem /srv'
```

Those tools arrive through [`nachalnik-mcp`][nachalnik-mcp] and only declare `mcp:call`, whatever
their annotations claim. Which server they came from is a separate permission: `--auto-allow-servers
files` covers that one server and no other, so give each server a `name=`. The command is split on
whitespace and nothing is unquoted, so an argument cannot hold a space. A path with a space is two
arguments; a script that starts the server works around that.

`fs`'s `grep` and `glob` use ripgrep's libraries directly instead of running `rg`. Without them,
searching for a symbol would need `exec:run`, which implies every other permission. They need
`fs:read`, as reading a file does, and are limited by the same path rules, so they let the model
explore a repository without that all-powerful permission ([what they cut and what they
skip][guide-find]).

Four tools are about the session itself: `context` reads and changes the context, `log` reads the
session record, `setup` shows the session's configuration, and `fork` asks a copy of the session a
question. Each operation is a public function the screen already used ([what each
does][guide-introspect]).

Tools can be changed during a session: `/tools toggle shell` stops offering a tool from the next
request, and toggling again brings it back without a restart. `/limit` changes how much of a call's
output the model sees, per permission, and the full version of a cut result is kept so it can be
sent instead. When a model goes off track, <kbd>d</kbd> at the permission prompt drops every call it
is waiting on, and the model is told.

## 🐢 one transition at a time

The loop is a state machine. `/step` runs one transition at a time instead of a whole turn, so you
can stop in `ready`: the model has said what it wants and nothing has happened yet:

```text
/step how many lines are in ledger.py?
```

If the model answers with a tool call (say, `shell` running `wc -l`), the session stops in `ready`:
the command has been decided and permitted, but isn't running. From here you can read it, exclude
things from the context, drop it, or `/step` again to run it.

Use `/step` again for each transition (the tool runs, then the next request is sent), or `/continue`
for the rest of the turn. While stepping, answering a permission question doesn't resume the turn
automatically, since you chose to step through it.

## 🎛️ features

- `mcp` (default): `--mcp`, for tools from MCP servers.
- `tui` (default): the screen and the keys. Without it, the program runs headless and none of the
  crates that draw it are built.
- `webui` (default): `--web`, the session as a web page on loopback or a local network address you
  give it.
- `advise`: a client for System One models that answer typed questions with probabilities instead of
  writing text. It only adds `kamchatka::endpoint::advise` to the library; the program uses it
  through `shell-advisor`.
- `shell-advisor`: adds `--rate-shell-commands`, colouring each shell command you are asked about
  green, yellow or red by the advisor's reading. The rating decides nothing. It is off by default
  because it sends command lines off the machine; [running it][advise] says exactly what leaves.

## 📚 the rest of it

- **[Using it][guide]** — the four tabs and what each is for, all the keys, the permission prompt
  and what answering *always* means, adding files, and the tools an agent uses to read and manage
  its own context.
- **[Running it][running]** — headless mode, serving a session so you can leave and come back, the
  same session in a browser, the three APIs and which endpoints work, what the status line's
  numbers estimate, what happens when the context fills up, the settings file, every option,
  embedding it in other programs, and what toolchains in your home directory need.
- **[The changelog][changelog]**, and [`nachalnik`][nachalnik] for the runtime under all of it.

## 🎸 the name

`nachalnik` is an homage to KINO's *Nachalnik Kamchatki*. Kamchatka was the boiler room Viktor
Tsoi shovelled coal in; this is the one where the work actually happens.

## 📜 licence

MIT.

<!-- crates.io resolves relative links against the directory this README was published from, not
     the repository root, so links into the tree are absolute. -->

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
