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

That's the whole integration. An MCP tool is a `Tool` that forwards calls to a server, so the
runtime didn't need any changes for this.

---

### 🔒 what a tool is allowed to do

MCP tools carry *hints* about themselves (`readOnlyHint`, `destructiveHint`, `openWorldHint`), and
the specification explicitly says a client "should never make tool use decisions based on
annotations received from untrusted servers". A permission policy that relied on them would be
trusting the very thing it's supposed to check: a server that wants to avoid being asked about would
only have to claim to be read-only.

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
name as a prefix: `files__read`. Names are also rewritten to fit what model providers accept
(`[a-zA-Z0-9_-]`, 64 characters), and rewriting can cause collisions, so `Installed::replaced` says
which tools were displaced. `without_prefix()` turns the prefix off, which is fine with a single
server, but with two, one server's `read` silently replaces the other's.

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
OpenAI's Responses, and as a line naming it for Chat Completions and Gemini, whose tool results are
text. Anything that can't be carried is named, because saying what was there is better than leaving
a gap.

Resources are read on request and returned as `ContextItem`s, which you can add to the context or
not; a server offering forty documents doesn't mean you want all forty in the context. A resource
without text is named the same way, instead of silently leaving the list one item short.

---

### 🔌 the SDK

`rmcp` is re-exported. `Server::connect` is generic over its transports and `Server::info` returns
one of its types, so callers need those types, from the *same* version this crate uses, which a
separate dependency can't guarantee. `Server::spawn` takes a `tokio::process::Command`, which isn't
re-exported, since anything using this runtime already depends on `tokio`.

---

### 🧪 tests

`cargo test -p nachalnik-mcp` talks to real MCP servers rather than mocks: one inside the test
process, and one written in Python and started as a child process, which is how most servers are
run (that test is skipped without `python3`). One test offers those tools to a real model, since
only a real provider can show whether a tool name passes its character rules.

---

### 📜 licence

MIT ([LICENSE-MIT][license]).

<!-- crates.io resolves relative links against the directory this README was published from, not
     the repository root, so links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT
