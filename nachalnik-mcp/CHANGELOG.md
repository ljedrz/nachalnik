# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

Nothing changed in this crate. It moves to track `nachalnik` 0.3.0, whose `ModelInfo` grew a field
and is therefore a minor bump; the bridge builds against it untouched, which is the result that
was wanted, and is the second time in a row it has been.

note: it *has* to move, rather than merely being allowed to. `kamchatka` depends on both, and a
bridge left at a version that is already on the registry would be resolved from there when the
workspace is packaged - bringing the `nachalnik` that version was published against with it, and
putting two incompatible copies of the runtime into one build. `cargo package --workspace` is what
catches that, and it is the reason the version below `nachalnik`'s is never the interesting half
of a release.

## [0.2.0] - 2026-08-30

Nothing changed in this crate. It is released to track `nachalnik` 0.2.0, whose `LinearProjector`
grew a field and is therefore a minor bump; the bridge builds against it untouched, which is the
result that was wanted.

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
