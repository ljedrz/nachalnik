# the security position

Stated once and in one place, because it is the thing most likely to be quietly assumed
otherwise. The `README.md` files say the same in longer form; this is what a change in this
workspace has to keep true.

Referenced from [AGENTS.md](AGENTS.md).

---

- **There is no sandbox, and the core will not grow one.** The kernel executes nothing - no
  filesystem, no network, no process spawning - so it has nothing to contain. Containment belongs
  inside a `Tool` or around the whole process.
- **What is enforced is one thing:** a refused call is never handed to `Tool::invoke`, and the
  refusal is an event and a tool result. A decision point with a paper trail, not a boundary. The
  rules a policy decides by are the policy's own and the kernel cannot see them, so the trail
  holds them as `policy.ruled` only where the policy's owner records them - `kamchatka` does for
  every rule it sets, the answer that made one, and the network opened for one call.
- **A `Capability` is a declaration, not a verified property**, and `Shell` subsumes every other
  one. A client that shows `shell: allow` beside `network: deny` without saying so is reporting a
  restriction that does not exist - which is why `kamchatka`'s permissions tab says so.
- **A capability is not always fine enough to answer with.** `Careful::judges` is the one place a
  call's *arguments* become subjects, and two kinds of rule come from there: a path rule
  (`fs:read: allow` is reasonable, `fs:read .env: allow` is not) and an operation rule
  (`context: allow` is reasonable for a `note` and not for an `exclude`). Both only ever tighten -
  the strictest of everything consulted wins - so neither can reopen what a capability refused,
  and that is the property that makes adding one safe. An operation rule is consulted only where one
  exists, so a tool nobody has written a rule about is judged exactly as before. A path rule
  compares names the way the filesystem does - case-blind on macOS and Windows, and on Windows
  without a trailing dot or a `:stream` - since `.ENV` opens `.env` there; and a rule about a
  domain or an operation no call is judged under is refused where it is given rather than kept.
- **Confinement lives where the process is spawned.** `kamchatka` puts its `shell` tool under
  Landlock by re-executing itself in a mode that restricts itself and then `exec`s the command, so
  `network: deny` is a refused TCP `connect` and the working directory is the edge of the world.
  The `landlock` crate has no UDP right to hand a ruleset, ABI 10 and the kernel's own
  `BIND_UDP`/`CONNECT_SEND_UDP` notwithstanding, and the readmes say so rather than rounding it up.
  The `exec` is load-bearing rather than tidy: a helper standing in front of the command is what a
  stopped call would kill instead of the command. `#![deny(unsafe_code)]` is why it is a re-exec
  rather than `CommandExt::pre_exec` - and why the UDP rights stay out of reach until the crate
  exposes them. The `fs` tool, which is not a process - it opens a file, or walks a directory of
  them - is held to the same boundary by its own code, which is weaker in kind and said to be: a
  path is resolved, links followed, and checked, and then opened beneath the directory it was
  allowed under - and a write makes its new file and renames it in a directory opened the same
  way. On Linux that open is `openat2` with `RESOLVE_BENEATH`, so a component swapped for a link
  between the check and the open is refused by the kernel; elsewhere, and on a kernel older than
  5.6, it is an ordinary open and the swap is not caught. A directory swapped in the middle of
  a walk is only caught at the files opened under it: `glob` lists names, and a name is not
  refused.
- **A command the model runs is not handed this program's keys.** Every variable `kamchatka` reads
  a key from - `endpoint::KEYS` - is taken out of the `shell` tool's environment, confined or not.
  The confinement holds a command to its directory and says nothing about what the command was
  handed when it started, so a key left in the environment is the one secret a confined command
  could always print, and what it prints goes into the context, the request and the record. The
  keys are still in this program's own environment, and on Linux `/proc/PID/environ` is how a
  process reads another's: confined, a command is refused its parent's, because Landlock denies a
  process in a domain any look into one outside it; with `--no-sandbox` it is not, and stripping
  the variables keeps them out of the command's environment and no further. MCP
  servers and a local advisor keep them: those are programs the person chose, running unconfined
  with everything the person can read, and stripping their environment would be a nuisance rather
  than a boundary.
- **A boundary that stops at `open` stops short.** A command that can reach a unix socket can have
  the process behind it act for it, and that process is not in the domain: `systemd-run --user`
  over the session bus read and wrote a home directory the same command was refused directly, and
  the compositor and a container daemon are the same door. Landlock got an access right for it in
  ABI 9, which is Linux 7.1, and `kamchatka` handles that right where the kernel has it - so a
  command may connect to a socket it could have written to, and to no other. Below that kernel
  nothing governs a `connect` at all. `sandbox::confines_unix_sockets` is what says which of the
  two a machine is, and it is asked rather than assumed, because handling a right that is not
  there would cost the ruleset its `Full` status and quietly stop the suite that tests it.
- **A sandbox that might not be there has to say so.** `Confinement` has a variant for every way it
  can fail and the permissions tab draws it. Never let it degrade silently.
- **A boundary the refused party cannot see is a boundary it will walk into repeatedly.** Landlock
  refuses an `open` with `EACCES`, which is exactly what the kernel says about a file that belongs
  to somebody else - so a confined command is handed a permission error indistinguishable from an
  ordinary one, and a model that cannot tell the two apart spends its turns on `sudo`. Saying it
  in the tool description is not enough: a live session ignored one and went hunting for a `cargo`
  that was never missing. Say it at the point of failure, name the path, and say nothing where the
  refusal was not yours - a hedge on `cat /etc/shadow` sends a model looking for a boundary that
  had nothing to do with it. `Sandbox::note_for` is the shape.
- **And a refusal names the whole of what the session *does* reach.** The other half of the same
  rule: a refusal that names the working directory and calls it as far as the session goes stops
  being true the moment anybody passes `--sandbox-allow` or `--sandbox-read`. Under-reporting a
  boundary costs more than over-reporting it, because a model reads a refusal as the whole of the
  rule and never goes near the path somebody opened for exactly this - and it is the one thing in a
  refusal the model cannot work out for itself. `Reach::range` is the shape, and it is
  deliberately spelled the way `Sandbox`'s `Display` is: two things saying the same thing about one
  session should not read as two rules.
- **A refusal closes the retry, and names no path but the one it refused.** A refusal that does
  not say the same call will fail again is read as a reason it failed *this time*: one model sent
  an identical path back again and again in a single turn. And every concrete path in a refusal is
  read as a path to try, because a refusal is read under pressure to try something else - a
  parenthesis offering `./~` for the rare file genuinely called that had two models reading `./~`,
  a file neither of them wanted. Rare spellings belong in the argument's description, which is
  read while choosing; the refusal gets the one instruction that applies. Two suites test it:
  `tests/boundary.rs` pins the sentence, and only the last section of `tests/live.rs` can watch a
  real model read it, because a scripted provider agrees with every refusal it is handed.
- **Nothing expands `~` for the file tools, and that is deliberate.** They run in process with no
  shell, so `read ~/.gitconfig` would join a directory literally called `~` onto the working
  directory and come back `No such file or directory` - the same trap as the one below, since a
  model believes an absent file and concludes the home directory is empty. Expanding it is the
  wrong fix: under `--no-sandbox` `Reach::allows` returns the path untouched, so `~/.ssh/id_rsa`
  would resolve for real on a path the model wrote. It is refused with a sentence instead, before
  the unconfined early return, and the argument's own description says the rule so the refusal is
  not a surprise. `shell` is the other way round - `sh -c` does expand it, and the confinement
  refuses what it expands to.
- **`access(2)` does not know about Landlock.** It answers from the file's own permissions, so a
  program that probes before it opens is told yes and then refused - and lands in whichever branch
  it keeps for a *corrupt* file rather than a *missing* one. Git does exactly this with
  `~/.gitconfig` and dies with `fatal: unknown error occurred while reading the configuration
  files`. Anything the confinement puts out of reach may need to be told it is not there rather
  than left to find out.
- **A protocol that carries the `shell` tool is the machine, so where it listens is the boundary.**
  `--serve` refuses a non-loopback bind rather than documenting it as a thing not to do, and the
  socket file is `0600` from the moment it exists. There is deliberately no authentication *in* the
  protocol: a token in every message is a scheme to keep in step, and it would be guarding a channel
  whose real boundary is somewhere else. Across a network, tunnel something that does authenticate.
  Anything added to `remote/` is held to this: it does not grow a credential, and it does not start
  deciding that some addresses are safe enough.
- **An answer to a permission question is four things, and two of them are easy to leave out.**
  `App::decide` is the one place all three loops answer through, and it exists because they did not:
  a headless run granted a `curl` and then ran it with the network cut, because telling `Careful`
  about a granted command is a separate act from telling the kernel. The other three are honouring
  `always` over what the policy actually *consulted* rather than over what the tool declared,
  sweeping the questions already queued behind this one, and driving the turn on afterwards.
- **Do not add a check that implies more than it delivers.** `reaches_the_network` is allowed to
  exist because its documentation is exact about what it misses, and because refusing up front with
  a reason is kinder than letting a command run and fail. It is no longer what stands between the
  model and the network. Anything of that shape needs the same treatment.

---

## who `kamchatka` is defending against

The positions above are each about one mechanism. This is the same ground by who could do harm,
what stands in the way, and what does not.

- **The model, and whoever wrote something it read.** A file, a command's output or a tool's
  answer can carry instructions, and the model acts on what it reads, so the model is treated as
  a party that may be steered. It acts only through tools the policy lets run. The `shell` tool is
  confined by Landlock on Linux - files outside the reach, TCP `connect`, and on 7.1 and later a
  unix socket it could not write - and is not handed this program's keys. The `fs` tool is held to
  the same reach by its own code, and on Linux opens beneath the directory a path was allowed
  under. What it can still do: send UDP; read anything the reach includes and put it in the
  context, which goes to the provider; spend the session's budget, including on `fork` drafts,
  which the spend ceiling does not count yet (POSTPONED.md). Off Linux, and under `--no-sandbox`,
  the shell is not confined at all and the permission question is the only thing in the way.
- **Whoever reaches a served session.** The protocol carries the `shell` tool, so reaching it is
  reaching the machine as the person who started it. `--serve` binds loopback only and makes its
  socket `0600`, and there is no authentication beyond that. The `gateway` and `phone` examples
  put a page in front of a session and will listen wherever they are told: whoever reaches that
  page drives the session, the same as the person at the keyboard. A web page open in a browser on
  the same machine is refused - the relay takes only requests whose host is an address or
  `localhost`, that name no origin but its own, and that post JSON - so a site cannot drive a
  session through the visitor's browser.
- **An MCP server or a local advisor.** These are programs the person chose, and they run
  unconfined with the person's environment and everything the person can read. What `kamchatka`
  controls is what their answers do: a server's tools are judged under the server's name, and what
  it returns reaches the context like any other tool result, where the model reads it - which is
  the first actor above again.
- **The provider.** Everything in a request is sent, and a request is the context: the
  conversation, what the tools returned, and any file the reach let the model read. Nothing here
  stops that, and nothing can - it is what asking a model is.
- **Other people on the machine.** The automatic record is written under a `0700` directory in the
  temporary directory, and not at all if what holds that name is anything but a directory nobody
  else can read; a served unix socket is `0600` from the moment it exists. A served loopback port
  is not: every account on the machine can connect to one, and a connection is the `shell` tool
  running as the person serving - so where other people share the machine, `--serve unix:PATH` is
  the one that keeps them out.
- **Size.** What a tool keeps of one call stops at `tools::KEPT`, so a command that writes without
  end, or a file larger than anybody meant to read, cannot fill the process, the archive or a save.
  What a tool from an MCP server returns is that server's to bound.
