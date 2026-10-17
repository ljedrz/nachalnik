# nachalnik-mcp

[![crates.io](https://img.shields.io/crates/v/nachalnik-mcp.svg)](https://crates.io/crates/nachalnik-mcp)
[![docs.rs](https://docs.rs/nachalnik-mcp/badge.svg)](https://docs.rs/nachalnik-mcp)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**An [MCP](https://modelcontextprotocol.io) bridge for [`nachalnik`][nachalnik]: tools from MCP
servers, usable as ordinary `Tool`s.**

```rust
let files = Server::spawn("files", Command::new("mcp-server-filesystem")).await?;
let installed = files.install(&kernel).await?;
```

An MCP tool is a `Tool` that forwards calls to a server. The runtime needed no changes.

---

### 🔒 what a tool is allowed to do

MCP tools carry hints about themselves (`readOnlyHint`, `destructiveHint`, `openWorldHint`). The
specification says a client "should never make tool use decisions based on annotations received from
untrusted servers". A policy that relied on them would trust a server's claim instead of checking
it; a server could claim to be read-only to avoid being asked.

So by default, `Trust` believes none of them. Every tool from a server declares `mcp:call` and
nothing else, whatever its annotations say. Which server a tool *came from* is a fact, not a claim,
and it's recorded by whoever started the server rather than by the tool, so a policy can say "ask me
once about this server" without trusting anything the server says:

```rust
Server::spawn("files", cmd).await?                        // believes nothing (the default)
Server::spawn("files", cmd).await?.trusting(Trust::Annotations)   // believes the server
Server::spawn("files", cmd).await?.trusting(Trust::Fixed(vec![Capability::fs("read")]))
```

`Trust::Annotations` is reasonable for a server you run yourself, and a mistake for one you don't,
which is why it has to be chosen explicitly.

---

### 📛 what a tool is called

Two servers may both offer `read`, and a kernel holds one tool per name, so names get the server's
name as a prefix: `files__read`. Names are rewritten to fit what model providers accept
(`[a-zA-Z0-9_-]`, 64 characters, starting with a letter or `_`; `7zip__list` is sent as
`_7zip__list`). Rewriting can cause collisions, so `Installed::replaced` says which tools were
displaced. `without_prefix()` turns the prefix off. With one server that is fine; with two, one
server's `read` replaces the other's, and only `Installed::replaced` says so.

When the prefix and name together are too long, the *prefix* is shortened, because the tool's own
name is what tells a server's tools apart.

---

### 📄 what comes back

| MCP | in the context |
| --- | --- |
| text blocks | joined, as text |
| `structuredContent` | `Content::Json` in place of any blocks — a server that returned structure meant it |
| `isError` | `ToolOutput::error`, handed to the model rather than stopping the loop |
| images | carried, as a `Content::Blob` between the text around them, unless larger than 5MB of base64 |
| audio, resources, larger images | *named*, not dropped: `[audio (audio/wav), not carried into the context]` |

An image goes to the model as each provider's API allows: inside the tool result for Anthropic's and
OpenAI's Responses, in the `functionResponse`'s own parts for Gemini, and as a line naming it for
Chat Completions, whose tool results are text. Anything that can't be carried is named, because saying what was there is better than leaving
a gap.

Resources are read on request and returned as `ContextItem`s, which you can add to the context or
not; a server offering forty documents doesn't mean you want all forty in the context. A resource
without text is named the same way, instead of silently leaving the list one item short.

---

### 🔌 the SDK

`rmcp` is re-exported. `Server::connect` is generic over its transports and `Server::info` returns
one of its types, so callers need those types from the same version this crate uses; a separate
dependency cannot guarantee that. `Server::spawn` takes a `tokio::process::Command`, which is not
re-exported because anything using this runtime already depends on `tokio`.

---

### 🧪 tests

`cargo test -p nachalnik-mcp` talks to real MCP servers, not mocks: one inside the test process, and
one Python child process (most servers run this way; that test is skipped without `python3`). One
test offers those tools to a real model, because only a real provider can show whether a tool name
passes its character rules.

---

### 📜 licence

MIT ([LICENSE-MIT][license]).

<!-- crates.io resolves relative links against the directory this README was published from, not
     the repository root, so links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT
