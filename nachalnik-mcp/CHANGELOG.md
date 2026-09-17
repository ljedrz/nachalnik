# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [0.6.0] - 2026-09-17

### changed

- One fewer direct dependency: `async-trait` is the runtime's, and every `#[async_trait]` here is
  already written against `nachalnik`'s re-export. Depending on it twice let the two drift.
- `tokio` belongs to the `child-process` feature rather than to the crate, and asks for `process`
  rather than for `rt` and `sync`. `Server::spawn` names a `tokio::process::Command` and nothing
  else here touches the runtime, so the two features it did ask for were never used and the one it
  needs was arriving through `rmcp/transport-child-process` - true today and not this crate's to
  rely on. A `--no-default-features` build now has no `tokio` of its own at all.
- The `rmcp` dev-dependency asks for 3.4, which is where `model::ServerConfig` arrived; the bench
  server in `tests/bridge.rs` has named it that since the 3.4 bump, so `3.1` was a requirement the
  tests could not be built under. The library's own floor stays at 3.1, because every name in
  `src/` is there.
- `foreign.rs`'s live test defaults to a model the default endpoint serves. It was `mercury-2.5`,
  which is Inception's spelling, and against OpenRouter that is a 404 - swallowed by the same skip
  that exists for a rate-limited free tier, so the suite passed and the one claim the test makes
  went untested with nothing saying so. The variable itself is `nachalnik_utils::test_model` now,
  in one place rather than five.
- The readme's tests section described one of the two suites. The second stands up a server written
  in Python, over the child-process transport most servers actually arrive on, and hands its tools
  to a real model.

## [0.5.0] - 2026-09-11

### added

- Property tests for the identifier rewriting (`sanitize`, `tool_id`), in `src/tool.rs` because both
  are `pub(crate)`: a tool's own name survives truncation whole, the result is always something a
  provider will accept, a character in is a character out up to the cap, rewriting an acceptable
  name changes nothing, and a server asking for no prefix gets none. The generators state their
  lengths - proptest's `.*` tops out near thirty characters, short of the sixty-four-character limit
  where every bug here has been.
- A written-out case showing a rewrite can still collide: two tools on one server under one
  identifier. Not a bug - 0.3.1 made the tool's name survive, not identifiers unique, and
  `Installed::replaced` reports it.

### changed

- Requires `nachalnik` 0.5.0, whose `PermissionPolicy::why` takes a `PermissionRequest` rather than
  a `ToolCallId`. Nothing in this crate's API moved, but it names runtime types in its public
  interface, so this release cannot be mixed with a 0.4-series runtime.

## [0.4.0] - 2026-09-10

### added

- A live test: a real model, offered a real server's tools, calls one and reads the result. Skips
  without an API key, as the rest of the file skips without `python3`.

### changed

- Requires `nachalnik` 0.4.0, which added `Budget::uncounted`, `ContextItem::uncounted` and
  `Blob::meta` - public fields on structs that are not `#[non_exhaustive]` - and dropped `Eq` from
  `Blob`. Nothing in this crate's API moved.

## [0.3.1] - 2026-09-08

### added

- `rmcp` is re-exported. `Server::connect` is generic over its transports and `Server::info` returns
  an `Arc<rmcp::model::ServerPeerInfo>`, so a caller has to be able to name those types at the
  version this crate holds.

### fixed

- A tool's own name is no longer what gets cut when a prefixed identifier exceeds sixty-four
  characters. A server with a sixty-two-character name had every tool arrive under one identifier,
  each quietly replacing the last. The prefix gives way instead, and is dropped rather than
  shortened to nothing when there is no room.
- A resource with no text is named rather than dropped from `Server::resources`, as
  `[a resource with no text (image/png), not carried into the context]`.

## [0.3.0] - 2026-09-05

Nothing changed in this crate. It tracks `nachalnik` 0.3.0, whose `ModelInfo` grew a field.

## [0.2.0] - 2026-08-30

Nothing changed in this crate. It tracks `nachalnik` 0.2.0, whose `LinearProjector` grew a field.

## [0.1.0] - 2026-08-29

The first release: MCP servers as `nachalnik` tools.

### added

- `Server`, a connection to one MCP server: over a child process (`spawn`) or any transport you have
  opened (`connect`).
- `Server::tools` returns `Arc<dyn Tool>`s; `Server::install` puts them in a `Kernel` and reports
  what it displaced, since a kernel holds one tool per name and two servers may both offer `read`.
- `Trust`, which decides what a server's tools may do. It believes nothing by default: MCP
  annotations are hints, and taking the word of the thing being gated is not a permission model.
  Every tool declares `Capability::Custom("mcp:<server>")`.
- `Server::resources` reads a server's resources as `ContextItem`s, handed back rather than pushed.
- Results map with nothing lost quietly: structured content stays `Content::Json`, `isError` becomes
  an error result the model can read, and content that cannot be text is named rather than dropped.
