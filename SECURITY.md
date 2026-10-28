# the security position

What changes to this workspace must keep true about what is and isn't enforced.

---

- **There is no sandbox in the core, and there won't be.** The kernel executes nothing (no
  filesystem, no network, no processes), so it has nothing to contain. Containment belongs inside a
  `Tool` or around the whole process.
- **The kernel enforces one thing:** a refused call is never passed to `Tool::invoke`, and the
  refusal is recorded as an event and a tool result. It's a checkpoint with a record, not a
  security boundary. The kernel can't see the rules a policy uses, so they only appear in the
  record (as `policy.ruled`) when the policy's owner records them. `kamchatka` records every rule it
  sets, the answer that created it, and the network access granted for a single call.
- **A `Capability` is a declaration, not a verified fact**, and `Shell` implies all the others. A
  client that shows `shell: allow` next to `network: deny` without pointing this out is reporting a
  restriction that doesn't exist, which is why `kamchatka`'s permissions tab points it out.
- **Capabilities aren't always detailed enough.** `Careful::judges` is the one place where a call's
  *arguments* become something rules can match, and two kinds of rules come from it: path rules
  (`fs:read: allow` is reasonable, `fs:read .env: allow` is not) and operation rules
  (`context: allow` is reasonable for `note` but not for `exclude`). Both can only make things
  stricter (the strictest matching rule wins), so neither can reopen something a capability
  refused, which is what makes them safe to add. Operation rules only apply where one exists, so a
  tool without any is judged as before. Path rules compare names exactly, as the filesystem does
  (`.ENV` and `.env` are different files), and `fs` refuses to follow a symlink to a protected path,
  since a link is just another name for it: reading `alias -> .env` is refused and reported as
  `.env`, and asking for `.env` directly goes through the normal question. A hard link is not
  caught this way: it is the same file under a second name, and a rule that matches names does
  not know the two names are one file. `fs` makes no links, and a command that makes one could read the file itself,
  since path rules cover tools given a path and `shell` is given a command. A rule for a domain or
  operation that no call is ever judged under is rejected when it's given, rather than kept.
- **Confinement happens where processes are spawned.** `kamchatka` runs each `shell` command in a
  [sandlock](https://github.com/multikernel/sandlock) sandbox: a Landlock ruleset, a seccomp filter,
  and a supervisor in `kamchatka`'s own process that answers the calls the filter holds. sandlock
  forks a child, confines it and executes the command in it, so the command is the process that is
  confined, and nothing outside the allowed directories can be touched. A command also reads the
  `shell-read` paths, which `fs` doesn't: `/usr`, `/etc`, `/bin`, `/sbin`, `/lib`, `/lib64`,
  `/opt`, `/proc`, `/sys` and `/run/systemd/resolve` unless the list is replaced. They're the
  shell's alone because `/proc/self` is a different file to each tool: to a command it's the
  command, and to `fs`, which runs inside `kamchatka`, it's `kamchatka`, with the API keys in its
  environment. The rest of `/run` isn't on the list, since it holds the person's running session:
  the session bus, the compositor, the container daemon and any mounted drive.
  `/run/systemd/resolve` holds only the resolver's configuration and its own sockets, and
  `/etc/resolv.conf` is a link into it on Ubuntu, Fedora and Debian. One more directory is
  writable: a command's own temporary directory, handed to it as `TMPDIR`, even where the working
  directory is read-only, and removed when the command ends. An older kernel's Landlock lacks some
  rights, and the sandbox goes without them: below Linux 5.19 a command can't move or link a file
  into another directory at all, truncating a file by name isn't restricted below 6.2, and device
  `ioctl`s aren't below 6.10. Such a sandbox is `Partial`, and the permissions tab says "partly
  confined". Where the sandbox doesn't start at all (no Landlock, or seccomp refused), the shell
  runs unconfined and says so; it is never left half-built.
  The `fs` tool runs in-process, so it enforces the same boundary in its own code, which is a
  weaker guarantee: it resolves the path (following symlinks), checks it, and then opens it with
  `openat2` and `RESOLVE_BENEATH` relative to the allowed directory, so if a path component is
  replaced by a symlink in between, the kernel refuses it. The directories a `write` makes for a
  new file are made the same way, one at a time from the one before, refusing a symlink. On
  kernels older than 5.6 it is an ordinary open, and such a swap isn't caught. A directory swapped
  during a directory walk is only caught for the files opened inside it.
  The allowed directories themselves (the working directory and the `--sandbox-write` and
  `--sandbox-read` paths) are resolved once, when the session starts, and are used only while no
  symlink stands on the way to them: `fs` checks that on every call, and before each command
  `kamchatka` opens each with `openat2` and `RESOLVE_NO_SYMLINKS` and leaves out one a symlink now
  stands on, telling the command so. sandlock takes paths rather than descriptors, though, and opens
  each again in its child, so a command left running that renames an allowed directory it can write
  beside, and leaves a symlink at its name between the two opens, gets that symlink's target
  granted. Granting the descriptor would close that (see POSTPONED.md). On kernels older than 5.6
  `kamchatka`'s own open follows symlinks too.
- **Network access is asked about when a command tries to use it, not based on the command's name.**
  This is the network gate. sandlock's supervisor holds every `connect`, `bind`, `sendto`, `sendmsg` and `sendmmsg` a
  sandboxed command makes, with its destination, and `kamchatka` answers for an internet address
  (TCP, UDP, and ICMP through ping sockets; DNS lookups included) from `net:reach`: `allow` lets it
  through, `deny` refuses it with `EACCES`, and `ask` asks the person, once per command, while the
  call waits. Decisions aren't made per destination. The supervisor makes an allowed connection
  itself, from its own copy of the address, so the command can't change the address after the
  answer. Raw sockets are refused, and so is every socket family but unix, inet, inet6 and netlink
  (which sandlock answers itself, from this kernel only), with `EAFNOSUPPORT`; `AF_SMC`, `AF_RDS`,
  `AF_VSOCK` and `AF_BLUETOOTH`, which reach beyond the machine, are among them. `io_uring` is
  refused entirely, since it can open sockets without these calls. Where the sandbox holds, `Careful`
  stops looking for program names in commands, and an allowed `exec:run` runs without asking until
  the command tries to use the network. It doesn't cover a socket passed over a unix socket by a
  process outside the sandbox (the unix socket rules below deal with that). A network attempt by a
  process a command left running, after the call has finished and with nothing decided, is refused
  without asking, since there's no command left to ask about. Under `--no-sandbox`, or where the
  sandbox doesn't start, the question is based on the command's name, and the permissions tab says
  so. `kamchatka` has no `unsafe` code; the system calls this takes are sandlock's.
- **Commands the model runs don't get this program's API keys.** Every variable `kamchatka` reads a
  key from (`endpoint::KEYS`) is removed from the `shell` tool's environment, sandboxed or not, and
  so is an address variable (`endpoint::ADDRESSES`) that carries a `user:password@`. The sandbox
  restricts files and the network but not the environment a command starts with, so a key left
  there could always be printed, and whatever a command prints goes into the context, the request
  and the record. The keys are still in `kamchatka`'s own environment, and on Linux a
  process can read another's through `/proc/PID/environ`. A sandboxed command can't read
  `kamchatka`'s, or any other process's outside its sandbox: sandlock serves `/proc` to a command
  itself and refuses those, and Landlock refuses inspecting a process outside the sandbox anyway;
  with `--no-sandbox` it can, so removing the variables only keeps them out of the command's own
  environment. MCP servers keep the keys: they're programs the person chose, running unconfined with
  access to everything the person can read, so stripping their environment would be an annoyance,
  not protection.
- **A sandboxed command can't use a terminal, and can open only five devices.** Its standard input
  is `/dev/null` and its output goes to pipes, `/dev/tty` won't open, and sandlock refuses the
  `TIOCSTI` and `TIOCLINUX` requests on any terminal. With terminal access, a command could inject a
  `y` into the input `kamchatka` reads keys from and answer its own permission question (on kernels
  that still allow `TIOCSTI`), or draw over the question on any kernel. In `/dev`, the ruleset
  allows `null`, `zero`, `full`, `random` and `urandom` and nothing else (`shell-devices` replaces
  the list), since other devices reach beyond the command: the person's other terminals, shared
  memory, cameras and microphones. Under `--no-sandbox`, a command keeps `/dev`, along with
  everything else the person has access to.
- **Stopping a call stops everything the command started.** sandlock follows every process group a
  sandboxed process makes or joins, so stopping a call kills all of them, including a process that
  started its own session with `setsid` or a daemon that detached itself; the same happens when a
  call is dropped. A call that *finishes* leaves them alone, so a background job it started
  (`sleep 300 &`, a server) keeps running while the session lasts, sandboxed, with its network
  attempts after the call refused without asking unless the command was already let through. When
  the session ends, every such process still running is sent `SIGTERM`, then `SIGKILL`, and listed.
  They are found by a mark every command carries in its environment, so one that cleared its own
  environment (`env -i`) isn't listed; it is still ended, since sandlock ends what is left in a
  command's sandbox when `kamchatka` exits. Under `--no-sandbox`, stopping a call kills the
  command's process group, and a process that left it, or cleared its environment, keeps running
  after the session.
- **Restricting `open` isn't enough.** A command that can reach a unix socket can ask the process
  behind it to act on its behalf, outside the sandbox: the session bus, the compositor, a container
  daemon. A `connect` or `send` to a named unix socket goes through only where the command may
  write, on any kernel. From Landlock ABI 9 (Linux 7.1), Landlock checks a `connect` and the
  command makes the connection itself, so the process at the other end sees the command's
  credentials. Below 7.1, sandlock's supervisor checks each `connect`; it checks each `send` to a
  named socket on every kernel, because the network gate holds every send. The supervisor resolves
  the socket file as the command would see it, holds it, and connects or sends from `kamchatka`,
  so a symlink swapped in after the check leads nowhere. The process at the other end then sees
  `kamchatka`'s credentials rather than the command's (see the served session below), and a socket
  named by a relative path is refused, since the supervisor can't resolve it from the command's
  directory. Abstract sockets have no path for that rule to check, and the X server listens on one
  and accepts any of the user's processes, which means a connection that can type into the person's
  terminal. Landlock can restrict abstract sockets from ABI 6 (Linux 6.12), and `kamchatka` uses
  that where available: a command may only connect to abstract sockets created inside its own
  sandbox. On older kernels, all of the user's abstract sockets are reachable.
  `sandbox::confines_abstract_sockets` reports which is the case.
- **A sandboxed command can only signal processes in the session.** Landlock's signal restriction
  arrived together with the abstract socket one, but it isn't part of each command's own ruleset:
  each command gets a separate sandbox, so it would stop a command from stopping a server that an
  earlier call left running. Instead, `kamchatka` applies it to its own process before starting
  anything, so every command inherits it: a command can signal what the session started and
  `kamchatka` itself, and `kill -9 -1` reaches nothing else of the person's. The cost is
  `no_new_privs` on everything `kamchatka` starts, MCP servers included, so set-user-ID programs
  like `sudo` gain no privileges in them; under `--no-sandbox` it isn't applied. Below Linux 6.12
  there's no such restriction, and every process of the user's can be signalled.
  `sandbox::confines_signals` reports which is the case.
- **If the sandbox might be missing, say so.** `Confinement` has a variant for every way it can
  fail, and the permissions tab shows it, with the kernel's own reason when a ruleset was refused
  (`Probed::why`), since a kernel without Landlock and a kernel that refused this sandbox are
  different problems. What the kernel is too old to confine that matters most - abstract sockets
  and signals below Linux 6.12 - is said at startup and at the top of each session
  (`sandbox::weaker_here`), since the tab's "partly confined" doesn't say which. Never let it fail
  silently.
- **Tell the model when the sandbox refused something.** Landlock refuses an `open` with `EACCES`,
  the same error as for a file owned by someone else, so a sandboxed command gets a permission error
  that looks ordinary, and a model that can't tell the difference wastes its turns trying `sudo`. A
  tool description isn't enough: say it where the failure happens, name the path, and say nothing
  when the refusal wasn't the sandbox's. Where the error names no path, there is nothing to judge,
  so the note says only that the sandbox may be the cause. See `Sandbox::note_for`.
- **And name everything the session *can* reach.** A refusal that names the working directory as
  the whole of what's allowed becomes false as soon as someone passes `--sandbox-write` or
  `--sandbox-read`. Under-reporting is worse than over-reporting here: a model takes a refusal as
  the complete rule, and won't try a path someone opened for exactly this purpose, and it can't
  find that out by itself. See `Reach::range`, which is worded the same way as `Sandbox`'s
  `Display`, so the two don't read like different rules.
- **A refusal says that retrying won't help, and names no path except the refused one.** If it
  doesn't say the same call will fail again, the model takes it as a temporary failure, and any
  other path mentioned in it gets tried next. Rarely used options belong in the tool description;
  the refusal gets only the instruction that applies. `tests/boundary.rs` checks the wording.
- **The file tools don't expand `~`, deliberately.** They run in-process without a shell, so
  `read ~/.gitconfig` looks for a directory literally named `~` inside the working directory and
  returns `No such file or directory`, and a model would believe the file doesn't exist. Expanding
  it would be the wrong fix, though: under `--no-sandbox` paths aren't checked, so `~/.ssh/id_rsa`
  would really resolve. Instead it's refused with an explanation, and `fs`'s description mentions
  it. `shell` is the opposite: `sh -c` expands `~`, and the sandbox refuses what it expands to.
- **`access(2)` doesn't know about Landlock.** It answers from the file's own permissions, so a
  program that checks before opening is told yes and then refused, and ends up in its handling for a
  *corrupt* file rather than a *missing* one, as git does with `~/.gitconfig`. Files the sandbox
  hides may need to be reported as not there.
- **Anything that reaches the session protocol can use the `shell` tool, so where it listens is the
  security boundary.** `--serve` refuses to bind anywhere but loopback, rather than documenting it
  as something not to do, and the socket file is made `0600` right after the bind creates it.
  During the one syscall in between, the directory it's in is what keeps others out, which only
  matters with a umask that makes new files group- or world-writable. The protocol deliberately has
  no authentication: a token in every message would be another mechanism to maintain, guarding a
  channel whose real boundary is the socket. To reach it over a network, use a tunnel that
  authenticates. Anything added to `remote/` must follow this: no credentials, and no deciding that
  some addresses are safe enough. (`--web` is the one exception, and only for its page; see below.)
- **Answering a permission question takes four steps, and two are easy to forget**: telling
  `Careful` as well as the kernel (or an allowed command runs with the network cut off), applying
  `always` to what the policy actually *checked* rather than what the tool declared, clearing the
  questions queued behind it, and continuing the turn afterwards. `App::decide` is the single place
  all three loops answer through. The network gate's question has its own, `App::decide_reach`, for
  the same reason: the kernel never asked it, so the `policy.ruled` it writes is the only record
  that a command was allowed out.
- **Don't add checks that promise more than they deliver.** `reaches_the_network` is acceptable
  because its documentation says exactly what it misses, because refusing early with a reason is
  better than letting a command run and fail, and because it's only used where the sandbox isn't
  there to hold the network. It isn't what keeps the model off the network. Anything similar needs the same care.

---

## who `kamchatka` is defending against

- **The model, and anyone who wrote something it read.** A file, a command's output or a tool's
  answer can contain instructions, and the model acts on what it reads, so it is treated as
  manipulable. It can only act through tools the policy allows. The `shell` tool is sandboxed by
  sandlock: files outside the allowed paths and unix sockets it could not write to are blocked, and
  every attempt to reach the network is held until the policy or a person answers; it does not get
  this program's keys. The `fs` tool enforces the same paths in its own code, and on Linux opens
  files relative to the directory they were allowed under. The model can still use the network
  however a person allowed; read anything inside the allowed
  paths and put it into the context, which is sent to the provider; and spend the session's budget,
  including on `fork` drafts. Under `--no-sandbox` the shell is not sandboxed at all, and the
  permission question is the only safeguard.
- **Anyone who reaches a served session.** The protocol can use the `shell` tool, so reaching it
  means running commands as the person who started it. `--serve` binds to loopback only and makes
  its socket `0600`; there is no other authentication. `--web` serves a page for the session whose
  only credential is a random key in its address, printed at startup; whoever has the address
  controls the session like the person at the keyboard. The key is 32 bits so it can be typed on a
  phone; after 100 wrong keys the page refuses everything for a minute, which puts guessing it at
  about 40 years for even odds. A connection has 10 seconds to send its request, and at most 64 may
  be waiting at once, so a device that can reach the port without the key can keep the page from
  loading for as long as it keeps trying, but cannot use up the file descriptors the session needs
  for its record and its commands. It listens on loopback or on an IP address in a private range;
  then every device on that network can read the session's traffic, and any device that learns the
  address can control it. `kamchatka` warns about this at startup and in each session. A host name
  is taken only where it resolves to this machine, as `localhost` does; one that resolves elsewhere,
  wildcards and public addresses are refused. From further away, use a tunnel that authenticates.
  Other web pages open in a browser on the same machine are refused: the page only accepts
  same-origin JSON requests addressed to an IP address or `localhost`. That rests on the browser
  saying where a request came from, with `Origin` or `Sec-Fetch-Site`, and where one is too old to
  send either, on the key: every route is beneath it, and a page that was never shown the address
  cannot guess it in time. Clients can answer permission questions, so the session's own commands
  are kept out too. Ports the session or its page are served on are closed to every command
  sandboxed while they are open, on every address and whatever the network stance or a person's
  answer. A socket file is reachable by any command that may write where it is, so a connection to a
  served unix socket is refused when the process at the other end is in one of this session's
  sandboxes, or is `kamchatka` itself while any command it sandboxed is still running, since that is
  how a command's connection arrives below Linux 7.1 (see unix sockets above); the web page's
  relay, which is `kamchatka` too, reaches the session without the socket. What gets through: the
  `gateway` example's page (a separate process, so it cannot close its port to the session's
  commands), and any *other* served session on the machine. A session only closes its own ports,
  and a TCP connection does not identify the process behind it, so one process's sandboxed command
  is an ordinary client to a session in another, and that session's addresses are up to whoever
  started it. A command allowed on the network can reach both.
- **An MCP server.** It is a program the person chose, running unconfined with the person's
  environment and access to everything the person can read. `kamchatka` controls what its answers
  can do: its tools are judged under the server's name, and what it returns goes into the context
  like any other tool result, where the model reads it.
- **Whoever wrote the directory `kamchatka` is run in.** Without `--config-file`, `kamchatka` reads
  `./kamchatka.json`, which can set every setting the command line can: `mcp` (starts unconfined
  programs before the first message), `no-sandbox`, `auto-allow`, `auto-allow-servers`, `on-ask` and
  the sandbox paths. Running `kamchatka` in someone else's repository would run it with their
  settings: their MCP servers would start as you, and a file that turns off the sandbox and allows
  `exec` would leave nothing between that repository's instructions and your machine. So the file is
  not read until someone at a terminal approves it; the question lists which settings it changes,
  and anything but yes runs without it. A run without a terminal to ask at is refused, with a
  message saying to pass the file with `--config-file`. The file in your own config directory is
  read without asking, because nobody else writes there.
- **The provider.** Everything in a request is sent to it, and a request is the context: the
  conversation, tool results, and any file the model was allowed to read. Nothing here prevents
  that, and nothing can; that's what using a model means.
- **Other users on the machine.** The automatic session record is written under a `0700` directory
  in the temporary directory, and not at all if whatever has that name is not a directory only you
  can read, or if a directory above it lets others rename what is in it (writable by them and not
  sticky, as `/tmp` is), since they could move it once it is checked. A served unix socket is made
  `0600` right after the bind; putting it in a directory of your own covers the moment before that.
  A served loopback port is not protected: every account on the machine can connect to it, and a
  connection can run the `shell` tool as the person serving. On shared machines, use
  `--serve unix:PATH`.
- **Size.** What a tool keeps from one call is capped at `tools::KEPT` for each of a command's two
  output streams, so a command that never stops writing, or a huge file, cannot exhaust memory, the
  archive or a saved session. Output from MCP server tools is up to the server to limit.
