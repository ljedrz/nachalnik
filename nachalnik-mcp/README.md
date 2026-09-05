# nachalnik-mcp

[![crates.io](https://img.shields.io/crates/v/nachalnik-mcp.svg)](https://crates.io/crates/nachalnik-mcp)
[![docs.rs](https://docs.rs/nachalnik-mcp/badge.svg)](https://docs.rs/nachalnik-mcp)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**An [MCP](https://modelcontextprotocol.io) bridge for [`nachalnik`][nachalnik]: a tool
somebody else wrote, as a `Tool` like any other.**

```rust
let files = Server::spawn("files", Command::new("mcp-server-filesystem")).await?;
let installed = files.install(&kernel).await?;
```

That is the whole integration. An MCP tool is a `Tool` that forwards to a server, so nothing in
the runtime had to change to make this possible - which was the point of the exercise.

---

### 🔒 what a tool is allowed to do

MCP tools carry *hints* about themselves - `readOnlyHint`, `destructiveHint`, `openWorldHint` -
and the specification says, in as many words, that a client "should never make tool use decisions
based on annotations received from untrusted servers". A permission policy that acted on those
hints would be taking the word of the thing it is meant to be gating. A server that would rather
not be asked about need only claim to be read-only.

So `Trust` defaults to believing none of it. Every tool from a server declares one capability
naming that server - `mcp:files` - which is a fact rather than a claim, and which makes "ask me
once about this server" something a policy can say in a line:

```rust
Server::spawn("files", cmd).await?                        // believes nothing (the default)
Server::spawn("files", cmd).await?.trusting(Trust::Annotations)   // believes the server
Server::spawn("files", cmd).await?.trusting(Trust::Fixed(vec![Capability::Read]))
```

`Trust::Annotations` is reasonable for a server you run yourself, and a mistake for one you do
not. It is spelled out rather than defaulted to for that reason.

---

### 📛 what a tool is called

Two servers may both offer `read`, and a kernel holds one tool per identifier, so names are
prefixed with the server's: `files__read`. Names are also rewritten to what model providers
accept (`[a-zA-Z0-9_-]`, 64 characters), and rewriting can collide - so `Installed::replaced`
says what was displaced instead of assuming nothing was. `without_prefix()` turns the prefixing
off, which is worth it for a single server and is how one server's `read` quietly becomes the
other's when there are two.

Where the two together are over the limit it is the *prefix* that gives way, because the tool's
own name is the half that tells one of a server's tools from another. A server with a
sixty-two-character name would otherwise have every one of its tools arrive under the same
identifier.

---

### 📄 what comes back

| MCP | in the context |
| --- | --- |
| text blocks | joined, as text |
| `structuredContent` | `Content::Json` - a server that returned structure meant it |
| `isError` | `ToolOutput::error`, handed to the model rather than stopping the loop |
| images, audio, resources | *named*, not dropped: `[an image (image/png), not carried into the context]` |

A picture cannot go into a text context. Saying what was there is a better answer than a gap.

Resources are read on request and handed back as `ContextItem`s for you to push or not - a server
offering forty documents is not an argument for putting forty documents in a context. A resource
with no text in it is named the same way, rather than quietly leaving the list one item shorter
than the server's.

---

### 🔌 the SDK

`rmcp` is re-exported. `Server::connect` is generic over its transports and `Server::info` hands
back one of its types, so a caller needs to be able to name them - and to name the *same* version
this crate is holding, which a separate dependency line can only guess at. `Server::spawn` takes a
`tokio::process::Command`, which is not re-exported: anything driving this runtime already has
`tokio`.

---

### 🧪 tests

`cargo test -p nachalnik-mcp` stands up a real MCP server *in the test process* and talks to it
over a pipe: the handshake, the tool listing, the calls and their content blocks all actually
happen. There is no mock of the thing the bridge talks to, because a mock of a protocol is a test
of your understanding of it.

Among them is a server offering a tool called `delete_everything` that claims to be read-only.
Under the default it buys nothing.

---

### 📜 license

MIT ([LICENSE-MIT][license]).

<!-- crates.io resolves a relative link against the directory this readme was published from,
     which is not where the repository root is. Links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT
