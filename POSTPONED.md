# postponed, on purpose

Known issues and ideas deliberately left for later, written down so nobody wastes time rediscovering
them. Each entry says what it is, why it waits, and what would unblock it.

Only what is still open belongs here. No finished work, no record of what was tested or checked,
no history of how an entry got to where it is: those go in the commit message. The fix that closes
an entry removes it; one that closes part of it leaves only the part that is left.

---

- **Chat Completions names a tool's image instead of sending it.** A tool message there is a
  string, so an image in a tool result goes out as a line naming it. The dialect could move the
  picture to where its API takes one - a user message after the tool messages - without changing
  the context. Unblocked by a model that needs it, and a live test showing it reads the picture
  once moved.

- **Anthropic's cache loses everything after an edit that isn't near the end.** The provider marks
  two cache points, the system prompt and the end of the request, and Anthropic looks back only
  20 blocks from a mark for an earlier entry. A session that edits its own context, as a sweep's
  does, falls back to the system prompt after an edit anywhere earlier and writes the rest again,
  at more than the price of reading it. The API takes four marks, so two are unused, but where to
  put them is a trade-off: marks that follow the turns or a fixed grid of block positions catch
  different edits, each mark is a cache write of its own, and blocks are counted after thinking
  and elision change them. Unblocked by a live comparison of placements on a session that edits
  its context, measuring what is read and written.

- **Serving clients older than the session, which `protocol::VERSION` promises.** A session should
  serve the older protocol versions it knows, but `watermark` checks a client's version without
  storing it, so once `VERSION` is `2`, nothing knows a connection is a version-1 client, and
  nothing stops it being sent version-2 messages.

  Nothing is needed while the version is `1`. When it changes, each connection has to remember its
  version, and every write in `remote::server::connection::attend` has to check it, which means each
  message type must declare the version it was added in. Meanwhile, `Message::Unknown` lets clients
  survive messages they don't know, and `Attached::version` tells them what the server speaks.

- **OpenRouter's `reasoning_details` is ignored.** A model whose reasoning only appears there loses
  it, and one that signs its reasoning across tool-call turns gets its turns back without it.
  Nothing tested so far needed it; a model that does, with a live test showing the difference,
  would show whether the OpenAI client should support it.

- **The `/dev/shm` test has not been seen to run.** `a_confined_command_cannot_write_shared_memory`
  skips wherever `/dev/shm` can't be written outside the sandbox, which includes a session run
  under `kamchatka`'s own confinement, so its assertions, and whether it fails once `/dev/shm` is
  added to `sandbox::DEVICES`, are unchecked. Unblocked by a run on a machine where `/dev/shm` is
  writable.

- **`/params` showing a parameter's type and range.** It shows the default and maximum where the
  model listing publishes them, and nothing else, because no endpoint publishes more. A table kept
  in this workspace would just be someone's documentation as of the day it was copied. Unblocked by
  an endpoint that publishes them, read the way `Published` reads the rest.

- **The Anthropic client and signed thinking after an edited history.** Described in the
  `nachalnik_providers::anthropic` docs, with the models it affects, and handled by neither the
  code nor the tests: some models reject a signed thinking block sent back after anything before
  it was edited (which pruning, rewriting and eliding all do), on accounts created on or after
  2026-08-31. The possible fixes are retrying on that one 400 error message, or a beta header no
  parameter can set. Neither OpenRouter, which rewrites the message and isn't what checks the
  signature, nor an older account shows the refusal. Unblocked by an Anthropic account created on
  or after 2026-08-31, to see it rejected by the real API and then fixed.
