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
  `.env`, and asking for `.env` directly goes through the normal question. A rule for a domain or
  operation that no call is ever judged under is rejected when it's given, rather than kept.
- **Confinement happens where processes are spawned.** `kamchatka` sandboxes its `shell` tool with
  Landlock: it re-runs itself in a mode that restricts itself and then `exec`s the command, so
  `network: deny` means a refused TCP `connect`, and nothing outside the allowed directories can be
  touched. Landlock covers TCP from ABI 4 (Linux 6.7). On older kernels the ruleset is reported as
  `Partial`: files are still restricted, TCP is left to the network gate (below) where available,
  and the permissions tab says "partly confined". Below Linux 6.2, truncating a file by name isn't
  restricted either. UDP is only covered by the gate. The `exec` matters: if a helper process sat
  in front of the command, stopping a call would kill the helper instead of the command.
  The `fs` tool runs in-process, so it enforces the same boundary in its own code, which is a
  weaker guarantee: it resolves the path (following symlinks), checks it, and then opens it with
  `openat2` and `RESOLVE_BENEATH` relative to the allowed directory, so if a path component is
  replaced by a symlink in between, the kernel refuses it. On kernels older than 5.6 it is an
  ordinary open, and such a swap isn't caught. A directory swapped during a directory walk is only
  caught for the files opened inside it.
- **Network access is asked about when a command tries to use it, not based on the command's
  name.** The sandboxed child also installs a seccomp filter (`kamchatka::gate`) that pauses every
  `socket()` call for `AF_INET` or `AF_INET6` (the first step of any network use, DNS lookups
  included), and for `AF_SMC` and `AF_RDS`, which any process can open and which reach the internet
  over TCP as well, and hands the decision to the parent process. The parent answers from
  `net:reach`: `allow` lets it through, `deny` refuses it with `EACCES`, and `ask` asks the person,
  once per command, while the call waits. The filter only reads the call's integer arguments (the low 32
  bits of the address family, which is all the kernel reads), so there's no address the command
  could change after the answer; decisions aren't made per destination. Where the gate works,
  `Careful` stops looking for program names in commands, and an allowed `exec:run` runs without
  asking until the command tries to use the network. The gate also covers 32-bit processes and x32
  calls, and refuses `io_uring_setup` entirely, since an io_uring can open sockets without calling
  `socket()`. It doesn't cover a socket inherited from, or passed over a unix socket by, a process
  outside the sandbox (the unix socket rules below deal with that), or other address families such
  as `AF_PACKET`, which need privileges a sandboxed command doesn't have. A network attempt by a
  process a command left running, after the call has finished and with nothing decided, is refused
  without asking, since there's no command left to ask about. Where the gate can't be installed
  (under `--no-sandbox`, or on a kernel that can't pause calls), the question is based on the
  command's name, and the permissions tab says so. `gate` is the only module in the workspace with
  `unsafe` code, each block with a comment explaining why it's sound.
- **Commands the model runs don't get this program's API keys.** Every variable `kamchatka` reads a
  key from (`endpoint::KEYS`) is removed from the `shell` tool's environment, sandboxed or not. The
  sandbox restricts files and the network but not the environment a command starts with, so a key
  left there could always be printed, and whatever a command prints goes into the context, the
  request and the record. The keys are still in `kamchatka`'s own environment, and on Linux a
  process can read another's through `/proc/PID/environ`. A sandboxed command can't read its
  parent's, because Landlock prevents processes inside a sandbox from inspecting ones outside it;
  with `--no-sandbox` it can, so removing the variables only keeps them out of the command's own
  environment. MCP servers keep the keys: they're programs the person chose, running unconfined with
  access to everything the person can read, so stripping their environment would be an annoyance,
  not protection.
- **A sandboxed command has no terminal, and can open only five devices.** The child starts its
  own session before running anything, so it has no controlling terminal: `/dev/tty` won't open,
  and `TIOCSTI` is refused on every other terminal. With terminal access, a command could inject a
  `y` into the input `kamchatka` reads keys from and answer its own permission question (on kernels
  that still allow `TIOCSTI`), or draw over the question on any kernel. If the child can't detach
  from the terminal, it runs nothing. In `/dev`, the ruleset allows `null`, `zero`, `full`, `random`
  and `urandom` and nothing else (`sandbox-device` replaces the list), since other devices reach
  beyond the command: the person's other terminals, shared memory, cameras and microphones. Under
  `--no-sandbox`, a command keeps the terminal and `/dev`, along with everything else the person
  has access to.
- **Stopping a call stops the command's session; anything that escapes it keeps running.**
  Stopping a call kills the command's process group, which is the session it starts in, so
  everything it started is stopped too, and the same happens when a call is dropped. A process that
  starts its own session (with `setsid`, or a daemon detaching itself) leaves that group and keeps
  running after the call reports that it stopped. It stays sandboxed and gated, and its network
  attempts after the call are refused without asking unless the command was already let through,
  so it can't gain access it wasn't given; a served session can't tell it apart from a client,
  though. Tracking the whole process tree would need a cgroup, a
  PID namespace or a subreaper per command, and each of those fails on some systems this program
  runs on, so stopping covers the process group, and says so.
  A call that *finishes* leaves its process group alone, so a background job it started
  (`sleep 300 &`, a server) keeps running while the session lasts. When the session ends, every
  such group still running is sent `SIGTERM`, then `SIGKILL`, and listed. `--leave-running` leaves
  them running, and also lists them. As above, processes that left their group aren't on the list.
- **Restricting `open` isn't enough.** A command that can reach a unix socket can ask the process
  behind it to act on its behalf, outside the sandbox: the session bus, the compositor, a container
  daemon. Landlock can restrict this from Linux 7.1, and `kamchatka` uses that: a command may only
  connect to sockets it could have written to. On older kernels, nothing restricts `connect`.
  `sandbox::confines_unix_sockets` reports which is the case. Abstract sockets have no path for that
  rule to check, and the X server listens on one and accepts any of the user's processes, which
  means a connection that can type into the person's terminal. Landlock can restrict abstract
  sockets from ABI 6 (Linux 6.12), and `kamchatka` uses that where available: a command may only
  connect to abstract sockets created inside its own sandbox. On older kernels, all of the user's
  abstract sockets are reachable. `sandbox::confines_abstract_sockets` reports which is the case.
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
  (`Probed::why`), since a kernel without Landlock and a kernel that refused this ruleset are
  different problems. What the kernel is too old to confine - TCP below Linux 6.7, abstract sockets
  and signals below 6.12 - is said at startup and at the top of each session
  (`sandbox::weaker_here`), since the tab reads "confined" either way. Never let it fail silently.
- **Tell the model when the sandbox refused something.** Landlock refuses an `open` with `EACCES`,
  the same error as for a file owned by someone else, so a sandboxed command gets a permission error
  that looks ordinary, and a model that can't tell the difference wastes its turns trying `sudo`. A
  tool description isn't enough: say it where the failure happens, name the path, and say nothing
  when the refusal wasn't the sandbox's. Where the error names no path, there is nothing to judge,
  so the note says only that the sandbox may be the cause. See `Sandbox::note_for`.
- **And name everything the session *can* reach.** A refusal that names the working directory as
  the whole of what's allowed becomes false as soon as someone passes `--sandbox-allow` or
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
  better than letting a command run and fail, and because it's only used where the gate isn't
  available. It isn't what keeps the model off the network. Anything similar needs the same care.

---

## who `kamchatka` is defending against

The points above are each about one mechanism. This section covers the same ground by who could
cause harm, what stops them, and what doesn't.

- **The model, and anyone who wrote something it read.** A file, a command's output or a tool's
  answer can contain instructions, and the model acts on what it reads, so the model is treated as
  something that may be manipulated. It can only act through tools the policy allows. The `shell`
  tool is sandboxed by Landlock on Linux (files outside the allowed paths, TCP `connect`, and from
  Linux 7.1 unix sockets it couldn't write to), its internet sockets are paused by the gate where
  available, and it doesn't get this program's keys. The `fs` tool enforces the same paths in its
  own code, and on Linux opens files relative to the directory they were allowed under. What the
  model can still do: send UDP where there's no gate, and use the network however a person allowed;
  read anything inside the allowed paths and put it into the context, which is sent to the
  provider; and spend the session's budget, including on `fork` drafts. Under `--no-sandbox` the
  shell isn't sandboxed at all, and the permission question is the only safeguard.
- **Anyone who reaches a served session.** The protocol can use the `shell` tool, so reaching it
  means running commands as the person who started it. `--serve` binds to loopback only and makes
  its socket `0600`, and there is no other authentication. `--web` serves a page for the session,
  also without authentication; whoever reaches the page controls the session like the person at
  the keyboard. It listens on loopback, or on an IP address in a private range, in which case every
  device on that network can control and read the session, and `kamchatka` warns about this at
  startup and in each session. A host name is taken only where it resolves to this machine, as
  `localhost` does; one that resolves elsewhere, wildcards and public addresses are refused. From
  further away, use a tunnel that authenticates. Other web pages open in a browser on the same
  machine are refused: the page only accepts same-origin JSON requests addressed to an IP address
  or `localhost`. Clients can answer permission questions, so the session's own commands are kept
  out too: ports the session or its page are served on are closed to every command sandboxed while
  they're open. That takes Landlock's TCP rules, from Linux 6.7: on an older kernel the ruleset is
  `Partial` and a port is closed only where the network is shut or asked about, which the gate
  holds, so with TCP allowed or refused by Landlock alone, a command can reach a session served
  over TCP and answer its questions. That is not worked around, and the startup warning says so. A
  socket file can be reached by every sandboxed command below Linux 7.1, and from 7.1 by any that
  may write where it is, so connections are refused when the other end is in the
  session of a command this process sandboxed (each runs in its own session), which covers anything
  it left running. What gets through: a process a command started with `setsid`, the `gateway`
  example's page (a separate process, so it can't close its port to the session's commands), and
  any *other* served session on the machine. A session only closes its own ports, and a TCP
  connection doesn't identify the process behind it, so one process's sandboxed command is an
  ordinary client to a session in another, and that session's addresses are up to whoever started
  it. A command allowed on the network can reach all three.
- **An MCP server.** It's a program the person chose, running unconfined with the person's
  environment and access to everything the person can read. What `kamchatka` controls is what its
  answers can do: its tools are judged under the server's name, and what it returns goes into the
  context like any other tool result, where the model reads it, which brings us back to the first
  point.
- **Whoever wrote the directory `kamchatka` is run in.** Without `--config-file`, `kamchatka`
  reads `./kamchatka.json`, and that file can set every setting the command line can: `mcp`, which
  starts unconfined programs before the first message, `no-sandbox`, `allow`, `allow-server`,
  `on-ask` and the sandbox paths. So running `kamchatka` in someone else's repository would run it
  with their settings: their MCP servers would start as you, and a file that turns off the sandbox
  and allows `exec` would leave nothing between that repository's instructions and your machine.
  So the file isn't read until someone at a terminal approves it: the question lists which of
  those settings it changes, and anything but yes runs without it. A run without a terminal to ask
  at is refused, with a message saying to pass the file with `--config-file`. The file in your own
  config directory is read without asking, since nobody else writes there.
- **The provider.** Everything in a request is sent to it, and a request is the context: the
  conversation, tool results, and any file the model was allowed to read. Nothing here prevents
  that, and nothing can; that's what using a model means.
- **Other users on the machine.** The automatic session record is written under a `0700` directory
  in the temporary directory, and not at all if whatever has that name isn't a directory only you
  can read. A served unix socket is made `0600` right after the bind, and putting it in a directory
  of your own covers the moment before that. A served loopback port isn't protected: every account
  on the machine can connect to it, and a connection can run the `shell` tool as the person
  serving. So on shared machines, use `--serve unix:PATH`.
- **Size.** What a tool keeps from one call is capped at `tools::KEPT`, for each of a command's two
  output streams, so a command that never stops writing, or a huge file, can't exhaust memory, the
  archive or a saved session.
  Output from MCP server tools is up to the server to limit.
