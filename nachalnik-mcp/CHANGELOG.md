# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [0.7.1] - 2026-09-24

### fixed

- **A resource's non-text parts are named beside its text.** A resource with no text at all was
  named, and one with a caption and a picture came back as the caption, the picture dropped
  without a word - where a tool result names every block it cannot carry.

- **A listing of resources that never ends gives up, as a listing of tools does.** `Server::tools`
  follows the server's cursor for a bounded number of pages, and `Server::resources` went through
  the SDK's `list_all_resources`, which follows it for ever: a server always handing back another
  cursor held the call until somebody killed it.

- **Under `Trust::Annotations`, a tool that does not say it is closed to the world gets
  `net:reach`.** An absent `openWorldHint` was read as `false`, where the specification's default
  for it is `true` - the same reading an absent `readOnlyHint` already got the other way, for the
  reason given there: an absent hint is not a reassurance. A policy refusing the network let
  every unannotated tool through.

### changed

- **An embedded resource with no text names its media type**, as a resource read on its own
  already did: "[an embedded resource (application/pdf), not carried into the context]". The two
  read a resource's parts through one function now, by type rather than through its JSON, which
  copied the whole of a blob to find it had no text.
- **What a failed call says is what happened.** A call that could not be sent - the connection
  gone - said the server "refused" it, which sends a model looking for what it did wrong; it
  says the call could not be sent. An interrupted call said the server "was told to stop" whether
  or not the cancellation went out; it says which. `Error::Connect` covers a refused handshake
  as well as an unreachable server, and says "could not connect" rather than "could not be
  reached".

## [0.7.0] - 2026-09-23

### changed

- **Built against `nachalnik` 0.7.** The runtime's minor moved, and the bridge's tools and servers
  are that runtime's types, so a caller on 0.6 and a bridge on 0.7 are two runtimes in one build.

### fixed

- **A server's standard error is held a line's worth at a time.** The tail kept twenty lines and
  no bound on one, so a server writing without newlines - a progress bar redrawn with `\r`, or one
  that means harm - grew it for as long as it ran. The start of each line is kept, to 1 KiB, and
  the rest read and let go.

- **A call the server never answers can be stopped.** The interrupt was read before a call went
  out and not while it was with the server, so a server that took a call and never answered held
  the turn for as long as it liked: escape, a deadline and a first `ctrl+c` did nothing. The call
  watches the interrupt while it waits now, and on one sends MCP's `notifications/cancelled` and
  tells the model the call was stopped part-way, since what the server did by then is not known.
  `tokio`'s `time` and `macros` are features of every build for it, which `rmcp` builds anyway.

- **A listing that never ends gives up.** `Server::tools` followed `nextCursor` for as long as a
  server handed one back, at startup, where nothing interrupts it; it reads a hundred pages and then
  refuses.

- **A spawned server's standard error is held rather than inherited.** `Server::spawn` handed the
  command to a transport whose default is to inherit it, and which sets all three streams over
  whatever the `Command` said - so a server logging a line per request wrote across the caller's
  terminal, `kamchatka`'s drawn screen included, and no caller could stop it. It is read in the
  background now, and the last lines of it ride on the error when the handshake fails, which is
  the one moment what a server says is the reason.

- The crate docs no longer say a server's progress reaches an `OutputSink` - no progress token is
  sent, so no notification arrives - and `Server::install` says that running it again leaves a
  tool the server stopped offering in place. They say a stop is read before a call goes out and a
  call already with the server is let finish, and that a picture in a result is named because
  neither dialect takes one inside a tool result.

## [0.6.1] - 2026-09-19

### fixed

- The crate-level example and `tests/foreign.rs` are behind the feature they need. Both call
  `Server::spawn`, which is what brings `tokio::process` in, so
  `cargo test -p nachalnik-mcp --no-default-features` did not build - and the configuration that
  could not be run is the one this crate documents as the real case, a bridge to a server somebody
  else opened the transport to. The suite is `required-features = ["child-process"]` like
  `kamchatka`'s three, and the example is under a `cfg_attr` on the same feature and renders where
  it did. CI checks this crate rather than testing it, so neither was visible there.

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
