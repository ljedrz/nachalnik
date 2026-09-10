# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- Properties over the identifier rewriting, generated and shrunk rather than written out as
  cases. `sanitize` and `tool_id` are the two functions in this crate that a model provider's
  charset and length limit are enforced by, and they have a collision in their history: a server
  whose name was sixty-two characters long once had every one of its tools arrive under the same
  identifier. What holds for every pair of names is that the *tool's* own name arrives whole,
  which is what 0.3.1 fixed and what this now states over generated pairs instead of over the one
  server that found it. Four more go with it: the result is always something a provider will take,
  a character in is a character out up to the cap, rewriting an acceptable name changes nothing,
  and a server that asked for no prefix gets none rather than an empty one.

  They live in `src/tool.rs` rather than in `tests/`, because both functions are `pub(crate)` -
  which is also why coverage-guided fuzzing is the wrong instrument here and a property inside the
  crate is the right one.

  Measured rather than assumed: each of five mutations of the two functions is caught. Filtering
  what it cannot use instead of rewriting it, truncating bytes where it truncates characters,
  dropping the guard that stops an empty name becoming an empty identifier, and - the one that
  matters - cutting the pair from the end so that the tool's own name is what gives way, which is
  the 0.3.1 bug put back. The shrinker earns its place on the third of those: it hands back
  `name = ""`.

  The fifth mutation is why the strategies state their lengths. `(?s).*` was the first version and
  it read exactly like a thorough one, but proptest's `*` tops out around thirty characters, so no
  single name it produced ever reached the limit - and doubling that limit in the implementation
  broke nothing at all. Truncation is half of what `sanitize` does and the strategy could not
  reach it. With `{60,70}` in the mix, weighted highest because the boundary is where this
  function's every bug has been, the same mutation fails every property that is about length.

- A case that a rewrite can still collide, constructed rather than found: two tools on one server
  arriving under one identifier, where the prefix is cut at a boundary the longer of the two names
  reproduces out of the separator. It is not a bug and is not to be fixed - `sanitize` has always
  said a rewrite can collide, and `Installed::replaced` is what the bridge answers with instead of
  assuming it displaced nothing. What it pins is that 0.3.1 did not make identifiers unique; it
  made the tool's name survive, which is a different promise and the only one truncation can keep.
  No generator would have found this one, which is the honest reason it is written out.

### changed

- Requires `nachalnik` 0.5.0. Nothing in this crate's own API moved, and nothing here implements
  the trait that changed - but it names runtime types in its public interface, so a caller cannot
  mix this release with a `nachalnik` from the 0.4 series, and cargo reads the middle number as
  the major. The runtime's 0.5.0 re-signed `PermissionPolicy::why` to take the
  `PermissionRequest` it was asked about rather than a `ToolCallId`.

## [0.4.0] - 2026-09-10

### added

- A live test: a real model, offered a real server's tools, calls one and reads what came back.
  The suites here prove the mapping and the child-process transport and then hand the result to a
  scripted provider that was always going to agree with it. What none of that settles is whether
  the thing on the other side of the bridge is *usable* - whether the identifier survives a
  provider's charset, whether the schema is one a model fills in correctly, and whether the
  description is enough to pick the right tool from three. Measured: `arith__add` offered
  alongside two others, the model picked it, the Python server answered 42, and the model read it.
  It skips without a key, the way the rest of the file skips without `python3`.

### changed

- Requires `nachalnik` 0.4.0. Nothing in this crate's own API moved, but it names runtime types
  in its public interface - so a caller cannot mix this release with a `nachalnik` from the
  0.3 series, and cargo reads the middle number as the major. The runtime's 0.4.0 added
  `Budget::uncounted`, `ContextItem::uncounted` and `Blob::meta`, each a public field on a
  struct that is not `#[non_exhaustive]`, and dropped `Eq` from `Blob`.

## [0.3.1] - 2026-09-08

### added

- `rmcp` is re-exported. `Server::connect` is generic over its transports and `Server::info` hands
  back an `Arc<rmcp::model::ServerPeerInfo>`, so a caller has to be able to name those types - and
  to name the version this crate is actually holding, which a separate dependency line could only
  guess at. Guessing wrong compiles into an error about two types with the same name, and an SDK
  bump here was a silent break for anyone who had guessed right.

### fixed

- A tool's own name is no longer what gets cut when a prefixed identifier is over the limit. Names
  are rewritten to `[a-zA-Z0-9_-]`, sixty-four of them, and the pair was truncated from the end -
  so a server with a sixty-two-character name had every one of its tools arrive under the same
  identifier, each quietly replacing the last, with only `Installed::replaced` to say so. The
  prefix gives way instead, and is dropped rather than shortened to nothing when there is no room
  at all.
- A resource with no text in it is named rather than dropped. `Server::resources` returned one
  item fewer than the server offered and said nothing about it, which left a caller unable to tell
  a missed document from one that was never offered - and disagreed with what this crate does
  everywhere else, since a tool result's image and audio blocks have always been named. It arrives
  as `[a resource with no text (image/png), not carried into the context]`, and pushing it is
  optional like everything else here.

## [0.3.0] - 2026-09-05

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
