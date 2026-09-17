# kamchatka

[![crates.io](https://img.shields.io/crates/v/kamchatka.svg)](https://crates.io/crates/kamchatka)
[![docs.rs](https://docs.rs/kamchatka/badge.svg)](https://docs.rs/kamchatka)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**A terminal agent that shows you its context.**

Built on [`nachalnik`][nachalnik], and built to demonstrate it. Everything in here is
ordinary user code — the tools, the permission policy, the compactor, the drawing, and the two
providers next door in [`nachalnik-providers`][providers]. The runtime supplies the state machine,
the context and the paper trail.

```console
$ cargo install kamchatka
$ export KAMCHATKA_API_KEY=sk-or-...
$ kamchatka -m qwen/qwen3-coder -f src/kernel.rs "what does the kernel do?"
```

```text
┌ chat │ context │ trace │ permissions ────────────────────────────────────────────────────────────────────────┐
│> what does the kernel do?                                                                                    │
│                                                                                                              │
│⟩ read({"path":"src/kernel.rs"})                                                                              │
│                                                                                                              │
││ pub struct Kernel(Arc<InnerKernel>);                                                                        │
│  // ... 900 more lines                                                                                       │
│                                                                                                              │
│· read: 15 tokens                                                                                             │
│                                                                                                              │
│The kernel is a state machine with five states. `step` performs one transition and returns the state it       │
│produced; `turn` repeats it until the model stops asking for tools. Nothing in it decides what the model is   │
│told - that is the projector's job.                                                                           │
│                                                                                                              │
└──────────────────────────────────────────────── alt+1 chat · alt+2 context · alt+3 trace · alt+4 permissions ┘
┌ you ─────────────────────────────────────────────────────────────────────────────────────────────────────────┐
│ ask for something, or /help                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────────────────────────────────┘
 done · gpt-4o-mini @ openrouter.ai · ~1,168 tokens, 0.9% (128k) · 1,102 really · 15 held back
```

> **The permissions are enforced, and it is still a demonstration rather than a hardened agent.**
> The `shell` tool runs under [Landlock](https://landlock.io), so `network: deny` is a TCP
> `connect()` refused by the kernel rather than a policy reading the word `curl` — TCP being the
> whole of what the `landlock` crate can refuse, so a UDP datagram still goes out; the kernel grew
> UDP rights in ABI 10 (Linux 7.2) and the crate has not caught up. `write: deny` makes the working
> directory read-only, and nothing outside that directory is readable or writable, with one
> deliberate exception: the system directories, because a command that cannot read `/usr/bin`
> cannot be a command. So `cat /etc/passwd` works and `cat ~/.ssh/id_rsa` does not.
> `fs`, which is not a process, is held to the same boundary by its own code and to a
> tighter one: it refuses `/etc/passwd` too, and it never expands `~` — there is no shell in front
> of it, so a path is taken at its word and it says so rather than reporting the file as missing.
> `--sandbox-allow PATH` opens up more, `--sandbox-read PATH` opens it for reading only, and
> `--no-sandbox` turns it off. It is one LSM, not a container; see [what it does and does not
> protect you from][protection], and [the permissions tab][guide-permissions] for what the screen
> says about which of it your kernel actually took.

## 🔧 what it comes with

Six tools — `fs`, `shell`, `context`, `fork`, `log`, `setup` — and a policy that asks about all of
it. Nothing is allowed on your behalf before you have been asked, reading a file included. A tool
is a *domain* and what it does is an *operation* in it, so `fs:read` is the subject and `fs` is
every one of them; answering **always** answers for one of those rather than for a tool's name,
which is what makes it work for tools this program has never heard of:

```console
$ kamchatka --mcp 'files=npx -y @modelcontextprotocol/server-filesystem /srv'
```

Those arrive through [`nachalnik-mcp`][nachalnik-mcp] declaring what their annotations claim and
nothing else, and where they *came from* is a subject of its own: `--allow-server files` is one
server and not the next one. The `name=` is worth giving, because it is what that grant names —
without it the name comes from the program, which for most of the servers people actually run is
`npx`.

`fs`'s `grep` and `glob` are ripgrep's engine linked in rather than shelled out to, and the reason
they exist is the subject they ride. Finding a symbol used to mean `exec:run`, which subsumes every
other permission — so a session that only wanted to be asked *about* a repository had to hand over
the one that answers for everything. These are `fs:grep` and `fs:glob`, walk a directory without a
shell in front of them, honour a `.gitignore`, and cut at a number of matches rather than at bytes,
saying so where they cut. The path rules bind them too: a walk cannot ask about `.env`, so it does
not open it and says how many it left alone.

Four of them are about the session itself: `context` reads the context, `log` the record kept
beside it, `setup` what the session is running with, and `fork` asks a copy of the session a
question. Every
action in them is a public function the screen was already calling, which is the argument for the
whole workspace rather than a feature of this program — [what each does][guide-introspect].

The registry is live rather than fixed at startup: `/tools toggle shell` stops offering it from the
next request onward and `/tools toggle shell` again offers it, which is one call on the kernel each way
and no restart. The `tools` key in a settings file says which of them a session starts
with. When a model has gone down
the wrong path entirely, <kbd>d</kbd> at the permission prompt drops *every* call it is waiting on
with one reason — and the model is told, rather than left waiting on calls that silently vanished.

So is how much of a call's output the model is shown, keyed by the same subject its permission is —
one row for `fs:read` and another for `fs:grep`, because a file and a repository-wide search are not
the same size. It starts at 32,000 bytes, and at 8,000 for the seven whose answer is a report of a
fixed shape rather than a piece of the session: measured against a session of ten items and one of a
thousand, those seven do not move and everything else does. `/limit` lists them — numbered, and the
number is one the command takes, so `/limit fs:read 64000` and `/limit 6 64000` are the same
instruction — and either changes one from its next call onward. The result that has *already* been
cut is recovered a different way: its whole is archived beside the copy the model was given, and
<kbd>space</kbd> on it sends that instead — the projector answers one call with one result, so the
whole takes the call and the short copy drops out.

## 🐢 one transition at a time

The loop is a state machine, and `/step` performs exactly one transition of it instead of a whole
turn. That is the only way to stand in `ready` — which the runtime documents as *a resting state
on purpose*, the moment the model has said what it wants and **nothing has happened yet**:

```text
> /step how many lines are in ledger.py?

⟩ shell({"cmd":"wc -l ledger.py"})

· step → ready: 1 call(s) decided, none of them run yet
      shell {"cmd":"wc -l ledger.py"}
```

The command is decided, permitted, and not running. From here you can read it, prune the context
it would have run against, drop it, or `/step` again to run it. A whole turn walks through this
state without ever drawing it, which is why every other agent's "approve this command?" is the
only checkpoint it has. Here the checkpoint is the state machine's own.

`/step` again for each transition — the tool runs, then the next request goes — or `/continue` for
the rest of the turn. While stepping, answering a permission does *not* quietly resume: you asked
to drive.

## 📦 installing

```console
$ cargo install kamchatka                     # from the registry
$ cargo install --git https://github.com/ljedrz/nachalnik kamchatka
$ cargo install --path kamchatka              # from a clone
```

Rust 1.88 or newer, and that is the whole list: no system libraries, no `pkg-config`, nothing
to install first. The TLS is `rustls` over `ring`, which builds its own cryptography rather than
looking for yours.

Two features, both on by default. `--no-default-features --features tui` drops MCP support and
the `--mcp` flag with it. `tui` is the other one, and it is the screen and the keys: without it
you get the same program, headless, 88 crates lighter.

**The sandbox is Linux-only.** The `shell` tool is confined with [Landlock](https://landlock.io),
which is a Linux LSM. Everywhere else the program builds and runs, but the shell is unconfined:
the permissions tab says `shell: a command can do any of these` rather than `shell: confined`, and
the stances are answers you were asked for rather than a boundary anything enforces.

On Linux it also wants a kernel new enough to have Landlock — 5.13 for the filesystem rules, 6.2
for the one that refuses `truncate()`, and 6.7 for `network: deny`. The tab says how much of it the
kernel took: `confined`, or `partly confined` where some of it is older than the machine.

## 📚 the rest of it

- **[Using it][guide]** — the four tabs and what each is for, everything the keys do, the
  permission prompt and what answering *always* commits you to, putting a file in, and the
  off-by-default tools an agent reads and manages its own context with.
- **[Running it][running]** — headless, the two dialects and which endpoints work, what the
  number in the status line is a guess *at*, a settings file, every option, embedding it in
  something else, and what a toolchain in your home directory needs.
- **[The changelog][changelog]**, and [`nachalnik`][nachalnik] for the runtime under all of
  it.

## 🎸 the name

`nachalnik` is an homage to KINO's *Nachalnik Kamchatki*. Kamchatka was the boiler room Viktor
Tsoi shovelled coal in; this is the one where the work actually happens.

## licence

MIT.

<!-- crates.io resolves a relative link against the directory this readme was published from,
     which is not where the repository root is. Links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[providers]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-providers
[nachalnik-mcp]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik-mcp
[protection]: https://github.com/ljedrz/nachalnik/blob/HEAD/nachalnik/README.md#-what-it-does-and-does-not-protect-you-from

[guide]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/GUIDE.md
[running]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/RUNNING.md
[guide-permissions]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/GUIDE.md#-the-permissions-tab
[changelog]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/CHANGELOG.md
[guide-introspect]: https://github.com/ljedrz/nachalnik/blob/HEAD/kamchatka/GUIDE.md#-letting-the-agent-read-and-manage-its-own-context
