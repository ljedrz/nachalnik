# postponed, on purpose

Known and decided against *for now*, so that nobody spends an afternoon rediscovering them.
Each entry says what it is, why it waits, and what would unblock it - and where an entry has
been wrong before, it says that too rather than being quietly corrected.

Referenced from [AGENTS.md](AGENTS.md).

---

- **`nachalnik-mcp` carrying a picture rather than naming one.** The bridge answers an image
  block with `[an image (image/png), not carried into the context]`, which was the only thing it
  could do and is no longer. Carrying it is a few lines - a `Content::Blob` instead of a sentence
  - and the reason to wait was that a server offering a 4 MB screenshot would put 5.5 MB of
  base64 into a context whose budget could not count it. **The counter is in as of 0.4.0**, so
  the blocker is gone: a budget now says how many pieces it could not price, and `kamchatka`'s
  compactor takes an unpriced tool result first.

  **The blocker was never only the counter, and this entry said it was.** Neither dialect
  accepts a picture in a *tool result* - `tool` content is a string in one and a
  `functionResponse` in the other - so a `Content::Blob` in one is flattened to
  `[image/png, N bytes]` on the way out, deliberately, and `blobs.rs` pins that. Carrying an MCP
  picture would therefore put megabytes of base64 in the context, send the model the same
  sentence it already gets, and - measured - make `Budget::uncounted` report one unpriced piece
  for a request whose actual content is twenty-four characters of text. That is the budget
  naming a hole the request does not have, which is the thing the elided-item rule exists to
  prevent.

  So what would unblock it is not a counter. It is a decision about **where a tool's picture
  reaches the model**, since the one place it cannot is where it currently sits: a picture has
  to be hoisted into a message that accepts one, which is a `Projector`'s business or a
  provider's, and neither has been asked. Nothing about the bridge changes until that does.

  Worth knowing for whoever picks this up: the kernel's projection carries the blob and the
  *provider* flattens it, so the kernel's budget is right about the request it built and wrong
  about the one that goes out. That is the only place in this workspace where those two differ
  in a way a figure can see.

- **A headless deadline that can cut short a command of the operator's own.** `--deadline` and
  `ctrl+c` are branches of the driver's `select!`, and a line read from the input is submitted
  *inside* the branch that read it - so while `/models` fetches a list, or `/model` and
  `/provider` finish a switch, neither branch can be reached. A deadline falling in that window is
  served when the command returns. The model's own turns are interruptible, which is where a run
  spends its time, so the hole is real and narrow.

  What would unblock it is somewhere for a command to run that the loop can outlive: `App::submit`
  takes `&mut App`, so the obvious move - a `timeout_at` around it - would drop the future
  mid-command and leave a `/provider` half applied, which is a worse thing to leave a session than
  a late deadline. The shape that works is the one `/model` already uses for its switch (a task,
  and `App::settling` awaited before the next line is read), applied to the commands that are
  themselves a request; what has to be decided first is what a deadline *means* for one - whether
  it interrupts the request or merely stops what comes after it.

- **Getting the shipped settings file to somebody who installed the binary.** `kamchatka.json`
  ships in the crate and in the release archive, so it reaches whoever clones the repository,
  unpacks the `.crate`, or downloads a build - and `cargo install` copies no files, so it reaches
  nobody who took that road. Two ways to close the rest and they compose: `include_str!` it into
  the binary behind a flag that prints it, or look for `./kamchatka.json` (and an XDG path) when
  `--config-file` was not given. The second is the one with a decision in it - a file that applies
  because of where you are standing is a file that surprises you, and the answer to that is
  usually "say which one you read, on the way in". It is open rather than decided, and
  `--config-file` works meanwhile.

- **`--reconcile`: one context out of several hard forks of one session.** Two or more past
  snapshots that share an ancestor, folded into one session to carry on from. Nothing about it is
  blocked; it is not built. The design was worked out and is written down here rather than in
  nobody's head, because most of it is decisions rather than code.

  It needs nothing in the runtime. `Snapshot` and `Kernel::resume` already are this, which is
  what `context: fork` is built out of and what its own note says - so it is a
  `Vec<Snapshot> -> Snapshot` in this crate and everything downstream is unchanged.

  **It is a common prefix, not a common subset.** Identifiers come from one `next_item` that never
  reuses a number, so two forks of one ancestor share ids 1..K with the same contents and then both
  allocate K+1 for different things. The longest agreeing prefix is a linear scan; if item 1
  already disagrees these are not forks of each other and it should refuse rather than concatenate.
  The one real conflict is an ancestor item a fork `revise`d - same id, different content - and
  `meta.revised` is what makes that detectable rather than a truncated prefix.

  **Identifiers are the easy half.** Renumber the tails, keep an old-to-new map per source, stamp
  `meta` with where each came from. Tool call identifiers need nothing: a result names its call by
  `ToolCallId` rather than by context id, so the pairing survives renumbering. `used_calls` is a
  union that dedupes the shared prefix on its own - and a genuine collision between two tails has
  to be a hard error, never a silent renumber, because a result paired with the wrong call is a
  lie about who asked what.

  **Do not merge the logs.** A snapshot says where things ended up and a log says what happened;
  two append-only logs interleaved are a record of something no observer saw. The reconcile is one
  event at the head of a fresh log, naming what was taken from where, and the source logs stay on
  disk.

  **Carry findings, not transcripts.** Keep the shared prefix whole and take from each tail only
  what the agent wrote down for itself - `Reference` items whose source is `agent`. Merging
  findings is coherent and merging transcripts is not: prefix plus tail A plus tail B is a
  conversation in which the work was done twice in an order that never happened. It also sidesteps
  signed reasoning, which some APIs require echoed back attached to exactly the turn it came from
  and which is invalid across models.

  **Two notes that contradict each other are both carried, labelled by fork.** Decided: a
  contradiction the model can see is what `conflict` in `nachalnik-eval` measures models on, and
  hiding it picks a side on the model's behalf.

  **Where that label goes is the part with a trap in it.** `note` is replaced whenever an item's
  state changes, so provenance cannot live there - the first `pin` would take it.
  `included_because` is already carrying the model's own reason for writing the note, and
  overwriting it would be rewriting the model's words. And `meta`
  never reaches the request at all: a `Reference` projects as `{label}:\n{text}`, so the label is
  the only field of an item a model reads without calling `context: look`. Prefixing the label
  (`a/plan`) buys visibility and breaks addressing - `label:plan` then names neither. So leave the
  label alone, stamp `meta` for the pane and for `look`, and push one **manifest** item at the
  divergence point: which ids came from which fork, and which labels are now carried by more than
  one. Said out loud because the resumed session cannot work it out, the way `fork`'s system
  message is - and it is the read-time counterpart of what `note` says at write time when a name
  is already taken.

  Calibration merges by summing `estimated` and `reported` and recomputing, which is right for two
  forks of one model and meaningless across two models, where it should be dropped - `Option` with
  `serde(default)` already makes "nothing learned" a valid state.

  Build the file-producing form first: `kamchatka reconcile a.json b.json -o merged.json`, then
  `--resume merged.json`. The operation is lossy and opinionated, so the constructed context wants
  reading before anything is sent to a model - the same argument `/request` is there for. A flag
  that does both is a convenience on top of it, not the primitive.

- **Reading a picture back out of a blob, anywhere.** `kamchatka` can now send one and still
  draws none, and that split is deliberate rather than unfinished: a terminal cell is not a
  pixel. It looks like a gap: an attached image is the one item in the context whose *content*
  nobody at this end can inspect. The context tab names it, `enter` on it names it, and the
  person's own knowledge of the file is the only account of what was sent. A client that renders
  is a different client, and the runtime already supports it; see `Blob::meta` and
  `pricing_a_picture.rs` for the half that is not rendering.

- **Confining the shell on macOS, and the Mac binary that would go with it.** Away from Linux
  `confine` answers `Unsupported` and the shell runs unconfined, which the readme says and
  `Confinement` says on the status line. It waits on two things: nobody here can test it, so CI
  would be the only thing that ever checked the boundary, and the one interface that could carry
  it is deprecated.

  macOS has one mechanism, Seatbelt - Apple's TrustedBSD MAC layer - and it is the right shape:
  kernel-level, unprivileged, path-scoped file rules and network rules. Two of its three doors are
  shut. `sandbox_init(3)` is the self-apply call and the true Landlock analogue: deprecated,
  private in the form that takes arbitrary SBPL, and `unsafe` FFI. App Sandbox entitlements are
  what the deprecation notice points at, need code signing, and sandbox the app rather than a
  child command. `sandbox-exec(1)` is deprecated in its own man page and still shipped and
  working; `apple/containerization#737` asks for a removal timeline and a replacement and is
  unanswered.

  A backend goes in `confine`'s `cfg(not(target_os = "linux"))` arm. The child this program
  re-executes would not confine itself; it would `exec sandbox-exec -p <profile> -- sh -c <cmd>`,
  which is no FFI, no `unsafe` and no dependency. Landlock's paths-with-rights map onto SBPL
  directly: `(deny default)`, `(allow file-read* (subpath ...))`,
  `(allow file-write* (subpath ...))`, `(deny network*)`.

  What differs. `(deny network*)` covers UDP, so the module's "`no network` here means no TCP" is
  a Linux-only sentence. There is no `Partial` - a profile applies or it does not - so macOS
  answers `Full` or `Unavailable`, and `Unavailable` becomes a runtime check for
  `/usr/bin/sandbox-exec`, which doubles as the warning if Apple pulls it. `SYSTEM` needs a macOS
  twin: `/System` for the dyld cache, `/private/var`, `/Library`. And the profile is generated
  text, so a working directory holding a `"` is an injection surface that wants escaping and a
  test before the rest is worth having.

  `birdcage` covers both platforms and is the wrong fit: it confines the calling process, so the
  restriction leaks past the spawn, which is the opposite of the re-execution this program does
  deliberately.

  `tests/sandbox.rs` is `#![cfg(target_os = "linux")]` at the file level, so the `macos-latest`
  column in CI is green while checking none of this. Splitting it is the first step: the claims
  about the program - a command cannot write outside the working directory, a `curl` is refused -
  run on both, and only the ones naming a mechanism stay gated.

  **The Mac binary is separable and is not blocked by any of it.** It is one matrix entry on
  `macos-latest` for `aarch64-apple-darwin` in the `upload-rust-binary-action` the Linux job
  already uses. Gatekeeper gates it rather than the build: an unsigned download is quarantined
  until `xattr -d com.apple.quarantine`, and signing and notarising needs a paid Apple Developer
  account and two secrets in CI. A Homebrew tap avoids quarantine.
