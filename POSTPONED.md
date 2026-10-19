# postponed, on purpose

Known issues and ideas deliberately left for later, written down so nobody rediscovers them. Each
entry says what it is, why it waits, and what would unblock it.

Only open items belong here. Do not record finished work, what was tested or checked, or how an
entry reached its current state; those belong in the commit message. A fix that closes an entry
removes it. A fix that closes part of an entry leaves only the remaining part.

---

- **A sandbox root inside a writable root can be swapped for a link to anywhere.** The roots from
  `--sandbox-write` and `--sandbox-read` are kept as written and resolved again each time they are
  used: by the ruleset of every confined command, which opens each by its path and follows links,
  and by `Reach` on every `fs` call. When a root lies inside one a command may write - beneath a
  writable working directory, say - one command can rename it and leave a symlink in its place, and
  every later command, and `fs`, is granted wherever the link points, read-only or read-write as the
  root was. The scratch directory is not affected, being granted from a descriptor held since it was
  made. Resolving each root once at start-up, and opening it in the child without following links
  (`openat2` with `RESOLVE_NO_SYMLINKS`, dropping a root that no longer opens that way), closes it.
  That changes what a root that does not exist yet means, since one made later would never be
  granted, and refusing nested roots outright is the other choice. Unblocked by deciding which.

- **A hard link to a protected file is read under its other name without a question.** Path rules
  match the name a tool is given, and a hard link has no target to follow, so `alias` sharing
  `.env`'s inode reads as an ordinary file. Catching it means finding a file's other names, which
  only a walk of what the session reaches can do, on every read of a file with more than one link;
  a build directory is full of them. It waits on choosing between that walk, refusing every file
  with more than one link under a path rule, or leaving it as SECURITY.md describes.

- **A turn recorded by another dialect goes to Gemini with its thinking unsigned.** A turn kept as
  `content` and `reasoning` rather than as blocks, which is how the OpenAI dialects record one, is
  sent to Gemini as a `thought: true` part with no `thoughtSignature`, and its calls without one
  either. Whether Gemini takes that, ignores it, or refuses the request as it refuses a signed
  call sent back unsigned is not known here. Unblocked by a live Gemini request continuing a
  session recorded through another dialect, with thinking and a call in it.

- **Chat Completions names a tool's image instead of sending it.** A tool message there is a string,
  so an image in a tool result goes out as a line naming it. The dialect could move the image to a
  user message after the tool messages, where the API takes one, without changing the context.
  Unblocked by a model that needs it and a live test showing it reads the image once moved.

- **Anthropic's cache loses everything after an edit that isn't near the end.** The provider marks
  two cache points, the system prompt and the end of the request. Anthropic looks back only 20
  blocks from a mark for an earlier entry, so a session that edits its own context, as a sweep's
  does, falls back to the system prompt after an earlier edit and writes the rest again at more than
  the price of reading it. The API takes four marks, so two are unused. Placement is a trade-off:
  marks that follow the turns or a fixed grid of block positions catch different edits, each mark is
  a cache write, and blocks are counted after thinking and elision change them. Unblocked by a live
  comparison of placements on a session that edits its context, measuring what is read and written.

- **Serving clients older than the session, which `protocol::VERSION` promises.** A session should
  serve the older protocol versions it knows. `watermark` checks a client's version without storing
  it, so once `VERSION` is `2`, nothing knows a connection is a version-1 client and nothing stops
  version-2 messages being sent to it.

Nothing is needed while the version is `1`. When it changes, each connection must remember its
version. Every write in `remote::server::connection::attend` must check it, so each message type
must declare the version it was added in. `Message::Unknown` lets clients survive messages they
don't know, and `Attached::version` tells them what the server speaks.

- **OpenRouter's `reasoning_details` is ignored.** A model whose reasoning appears only there loses
  it, and one that signs its reasoning across tool-call turns gets its turns back without it. No
  test so far has needed it. A model that does, with a live test showing the difference, would show
  whether the OpenAI client should support it.

- **The `/dev/shm` test has not been seen to run.** `a_confined_command_cannot_write_shared_memory`
  skips wherever `/dev/shm` cannot be written outside the sandbox, including a session run under
  `kamchatka`'s own confinement. Its assertions, and whether it fails once `/dev/shm` is added to
  `sandbox::DEVICES`, are unchecked. Unblocked by a run on a machine where `/dev/shm` is writable.

- **`/params` showing a parameter's type and range.** It shows the default and maximum where the
  model listing publishes them, and nothing else, because no endpoint publishes more. A table kept
  in this workspace would be only someone's documentation as of the day it was copied. Unblocked by
  an endpoint that publishes them, read the way `Published` reads the rest.

- **The Anthropic client and signed thinking after an edited history.** The
  `nachalnik_providers::anthropic` docs describe it, with the models it affects; neither the code
  nor the tests handle it. Some models reject a signed thinking block sent back after anything
  before it was edited, which pruning, rewriting and eliding all do, on accounts created on or after
  2026-08-31. Possible fixes are retrying on that one 400 error message, or a beta header no
  parameter can set. Neither OpenRouter, which rewrites the message and is not what checks the
  signature, nor an older account shows the refusal. Unblocked by an Anthropic account created on or
  after 2026-08-31, to see it rejected by the real API and then fixed.

- **What a session costs in money, and what is left of the balance.** OpenRouter reports each
  request's cost in credits in `usage.cost` on every response, in the last chunk of a stream;
  nachalnik ignores it. No response includes the balance. `GET /api/v1/key` returns what is left
  under the key's own spending limit, null for a key without one. The account balance is only at
  `/api/v1/credits`, which needs a management key. Anthropic, OpenAI and Gemini offer neither to an
  ordinary key, so supporting OpenRouter alone would show a cost on some endpoints and nothing on
  the rest. Unblocked by providers settling on a common way to report a request's price and a key's
  balance, or by deciding an OpenRouter-only figure is worth having.

- **`system1::Client::url` keeps a Workers AI address that names a different model.** On Workers AI
  an address is used as it is when it already ends in the model's name, whole or short. Every
  address the docs describe that ends in the short name also ends in the whole one, so the short
  name matters only for an address like `.../ai/run/@cf/meta/clef` used with the model
  `@cf/cloudflare/clef`; the request then goes to meta's model. The short-name check could go, so
  the model is added to such an address, or it could stay and be tested. Unblocked by deciding
  which.

- **Two match arms in `Setup::check` can never be taken.** When a rule in `auto-allow` or
  `auto-deny` names no tool that exists here, the refusal names the setting the rule came from, and
  two match arms name `auto-allow-server` and `auto-deny-server` for a server's rule. `unreached`,
  which finds those rules, never objects to a server's rule, so the two arms are never taken and
  nothing can test them. Either the arms and their comment go, or a server rule that names no
  configured server is also refused and the arms are tested with it. Unblocked by deciding whether
  such a rule should be refused.

- **An MCP call whose future is dropped is never cancelled at the server.** `McpTool::invoke` sends
  MCP's `notifications/cancelled` when the call is interrupted or times out. A caller that drops the
  step future while the call is with the server, rather than interrupting it, sends nothing, and the
  server goes on with work whose answer is thrown away. kamchatka stops a call by interrupting it,
  so it is not affected. A guard that spawns the cancellation when dropped would close it, given a
  runtime to spawn on. Unblocked by a caller that drops step futures, and a test server that can say
  whether it was told to stop.
