# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [0.1.0] - 2026-08-29

The first release: MCP servers as `nachalnik` tools.

### added

- `Server`, a connection to one MCP server: over a child process (`spawn`) or any transport you
  have opened (`connect`).
- `Server::tools` hands back `Arc<dyn Tool>`s; `Server::install` puts them in a `Kernel` and
  reports what it displaced, because a kernel holds one tool per name and two servers may both
  offer `read`.
- `Trust`, which decides what a server's tools are allowed to do. It believes nothing by default:
  MCP annotations are hints, and taking the word of the thing being gated is not a permission
  model. Every tool declares `Capability::Custom("mcp:<server>")`, which is a fact rather than a
  claim.
- `Server::resources` reads a server's resources as `ContextItem`s, handed back rather than
  pushed.
- Results map with nothing lost quietly: structured content stays `Content::Json`, `isError`
  becomes an error result the model can read, and content that cannot be text - an image, audio -
  is named rather than dropped.
- Tested against a real `rmcp` server in-process, and against a hand-written MCP server in Python
  over a child process, so that what is under test is the protocol rather than one library's
  round trip.
