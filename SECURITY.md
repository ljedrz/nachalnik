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
  refusal is an event and a tool result. A decision point with a paper trail, not a boundary.
- **A `Capability` is a declaration, not a verified property**, and `Shell` subsumes every other
  one. A client that shows `shell: allow` beside `network: deny` without saying so is reporting a
  restriction that does not exist - which is why `kamchatka`'s permissions tab says so.
- **A capability is not always fine enough to answer with.** `Careful::judges` is the one place a
  call's *arguments* become subjects, and there are two kinds that come from there: a path rule
  (`fs:read: allow` is reasonable, `fs:read .env: allow` is not) and an operation rule
  (`context: allow` is
  reasonable for a `note` and not for an `exclude`). Both only ever tighten - the strictest of
  everything consulted wins - so neither can reopen what a capability refused, and that is the
  property that makes adding one safe. An action rule is consulted only where one exists, so a
  tool nobody has written a rule about is judged exactly as before.
- **Confinement lives where the process is spawned.** `kamchatka` puts its `shell` tool under
  Landlock by re-executing itself in a mode that restricts itself and then `exec`s the command, so
  `network: deny` is a refused TCP `connect` - the `landlock` crate has no UDP right to hand a
  ruleset, ABI 10 and the kernel's own `BIND_UDP`/`CONNECT_SEND_UDP` notwithstanding, and the
  readmes say so rather than rounding it up - and the working directory is the edge of the world.
  The `exec` is load-bearing rather than tidy: a helper standing in front of the command is what a
  stopped call would kill instead of the command. The `fs` tool, which is not a process - it opens a
  file, or walks a directory of them - is held to the same boundary by its own code, which is
  weaker in kind and said to be: a path is resolved, links followed, and checked, and then opened
  beneath the directory it was allowed under. On Linux that open is `openat2` with
  `RESOLVE_BENEATH`, so a component swapped for a link between the check and the open is refused
  by the kernel; elsewhere, and on a kernel older than 5.6, it is an ordinary open and the swap is
  not caught. A directory swapped in the middle of a walk is only caught at the files opened under
  it: `glob` lists names, and a name is not refused. `#![deny(unsafe_code)]` is why it is a re-exec rather than `CommandExt::pre_exec` - and why the UDP
  rights stay out of reach until the crate exposes them.
- **A command the model runs is not handed this program's keys.** Every variable `kamchatka` reads
  a key from - `endpoint::KEYS` - is taken out of the `shell` tool's environment, confined or not.
  The confinement holds a command to its directory and says nothing about what the command was
  handed when it started, so a key left in the environment is the one secret a confined command
  could always print, and what it prints goes into the context, the request and the record. MCP
  servers and a local advisor keep them: those are programs the person chose, running unconfined
  with everything the person can read, and stripping their environment would be a nuisance rather
  than a boundary.
- **A boundary that stops at `open` stops short.** A command that can reach a unix socket can have
  the process behind it act for it, and that process is not in the domain: `systemd-run --user`
  over the session bus read and wrote a home directory the same command was refused directly, and
  the compositor and a container daemon are the same door. Landlock got an access right for it in
  ABI 9, which is Linux 7.1, and `kamchatka` handles that right where the kernel has it - so a
  command may connect to a socket it could have written to, and to no other. Below that kernel
  nothing governs a `connect` at all; `sandbox::confines_unix_sockets` is what says which of the
  two a machine is, and it is asked rather than assumed, because handling a right that is not
  there would cost the ruleset its `Full` status and quietly stop the suite that tests it.
- **A sandbox that might not be there has to say so.** `Confinement` has a variant for every way it
  can fail and the permissions tab draws it. Never let it degrade silently.
- **A boundary the refused party cannot see is a boundary it will walk into repeatedly.** Landlock
  refuses an `open` with `EACCES`, which is exactly what the kernel says about a file that belongs
  to somebody else - so a confined command is handed a permission error indistinguishable from an
  ordinary one, and a model that cannot tell the two apart spends its turns on `sudo`. Saying it
  in the tool description is not enough; a live session ignored one and spent six calls hunting for
  a `cargo` that was never missing. Say it at the point of failure, name the path, and say nothing
  where the refusal was not yours - a hedge on `cat /etc/shadow` sends a model looking for a
  boundary that had nothing to do with it. `Sandbox::note_for` is the shape.
- **And a refusal names the whole of what the session *does* reach.** The other half of the same
  rule, and the one that was quietly wrong for longer: `Reach`'s refusal named the working
  directory and called it as far as the session went, which stopped being true the moment anybody
  passed `--sandbox-allow` or `--sandbox-read`. Under-reporting a boundary costs more than
  over-reporting it, because a model reads a refusal as the whole of the rule and never goes near
  the path somebody opened for exactly this - and it is the one thing in a refusal the model cannot
  work out for itself. `Reach::range` is the shape, and it is deliberately spelled the way
  `Sandbox`'s `Display` is: two things saying the same thing about one session should not read as
  two rules.
- **A refusal closes the retry, and names no path but the one it refused.** Two rules about the
  wording, both bought by watching models read one. A refusal that does not say the same call will
  fail again is read as a reason it failed *this time*: one model sent an identical path back six
  times in a single turn, and after the sentence was added it asked once and got it right. And
  every concrete path in a refusal is read as a path to try, because a refusal is read under
  pressure to try something else - a parenthesis offering `./~` for the rare file genuinely called
  that had two models reading `./~`, a file neither of them wanted. Rare spellings belong in the
  argument's description, which is read while choosing; the refusal gets the one instruction that
  applies. Two suites test it, and only one can - `tests/boundary.rs` pins the sentence, and the last
  section of `tests/live.rs` watches a real model read it, because a scripted provider agrees with
  every refusal it is handed.
- **Nothing expands `~` for the file tools, and that is deliberate.** They run in process with no
  shell, so `read ~/.gitconfig` used to join a directory literally called `~` onto the working
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
  Measured: breaking the first failed nothing in the whole crate before the test that now covers it.
- **Do not add a check that implies more than it delivers.** `reaches_the_network` is allowed to
  exist because its documentation is exact about what it misses, and because refusing up front with
  a reason is kinder than letting a command run and fail. It is no longer what stands between the
  model and the network. Anything of that shape needs the same treatment.
