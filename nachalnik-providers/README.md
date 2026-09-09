# nachalnik-providers

[![crates.io](https://img.shields.io/crates/v/nachalnik-providers.svg)](https://crates.io/crates/nachalnik-providers)
[![docs.rs](https://docs.rs/nachalnik-providers/badge.svg)](https://docs.rs/nachalnik-providers)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**Providers for [`nachalnik`][nachalnik]: somebody else's HTTP API, read carefully.**

```rust
let provider = Arc::new(OpenAiCompatible::new(model, base_url, api_key));
provider.probe().await;

let kernel = Kernel::new(Config::default());
kernel.set_provider(provider);
```

The runtime ships no provider and never will - it has no network in it, and a kernel with an
opinion about who you talk to would be a kernel worth distrusting. That left every adopter writing
the same thousand lines of streamed HTTP before they could ask a model anything. This is those
lines, written once.

---

### 🗣 two dialects, one trait

| feature | what it speaks |
| --- | --- |
| `openai` (default) | `POST /chat/completions`, `choices[].delta`, tool calls assembled from fragments. OpenRouter, ollama, vLLM, LM Studio, Together, and most of the rest. |
| `gemini` | Google's `generateContent`: `candidates[].content.parts`, whole calls, ordered `thought` parts. |

Both answer `Provider`, which is what the kernel asks through, and `Endpoint`, which is what the
program around it asks: where the requests are going, which model is being asked, what this
endpoint serves, and what the last retry was about. So one `Arc<dyn Endpoint>` holds either, and
nothing above it finds out which it got.

The second dialect is the one worth having for its own sake. Gemini answers with the parts of a
turn *in the order they were produced* - a thought, a sentence, a call, more thinking - and an
OpenAI-compatible shim in front of it has nowhere to put that: it flattens the turn into a
`content` string beside a `tool_calls` array and everything downstream reads a rearrangement. Here
it arrives as `Content::Blocks`, is counted and pruned like anything else, and goes back out the
same way - signatures attached to the parts they belong to, which is what that API rejects the
next request over.

---

### ⏳ waiting, and knowing what kind of waiting it is

A stream that has gone quiet, a request refused with a `Retry-After`, and one somebody pressed
escape on are three different answers to *send it again?*, and getting them wrong costs either a
turn or somebody's money. All three are separated here, and shared by both dialects:

- **a stalled stream is interruptible.** The read wakes every 120ms to check whether the caller
  asked it to stop, so a server that accepts a connection and then goes away does not hold the
  program for eighteen minutes with no way to take it back.
- **the silence is reported.** After ten seconds it says so through `Endpoint::take_notice`, and
  again every thirty; after 150 it gives up.
- **a busy server is retried, a spent quota is not.** A `Retry-After` longer than a minute is a
  daily limit answering with the seconds until midnight, and sitting through four doublings to
  discover that wastes the turn as well as the wait.
- **an interrupt is an answer, not an error.** It comes back as `StopReason::Other("interrupted")`
  with whatever had arrived, because a red line for doing as asked reads as a bug.

Every request is retried at most four times and **every attempt is billed** - a provider that
generated nine thousand tokens and then lost the connection has still generated them - which is
why the retry is for a server that said *busy*, not for a request that is simply large.

---

### 🔇 what it does not do

It reads **no environment**. Where the requests go, which key pays for them and what limit to
measure against are the caller's to decide and its business to say out loud; a library that
quietly picked up `OPENAI_API_KEY` would be spending somebody's money on the strength of a
variable they exported for another reason.

It **prints nothing**. Fragments are reported through `nachalnik::DeltaSink` and the screen
belongs to whoever owns it.

It **invents no parameters**. What the caller set is what goes out, verbatim - which is the
runtime's rule and not a provider's to break. `openai::NOT_A_STREAM` is the one concession: a list
of parameter names that stop a stream being a stream, for a client that would like to warn before
the request rather than be quietly wrong about its own record afterwards.

---

### 🧪 tests

`cargo test -p nachalnik-providers --all-features` talks to a real socket wherever the thing under
test lives inside `respond` - a stream assembled from chunks cannot be checked by a parser called
from outside it, because that is a test of a copy of the code. A server that answers and then says
nothing, one that breaks a stream mid-character, one that returns an `error` object inside a 200:
each body below is a shape some endpoint actually sent.

Both dialects are also held to one conformance suite, so that a case is added once and applies to
both. Every case in it is a bug that really happened, back when this code existed in three copies
and each was fixed one copy at a time.

---

### 📜 license

MIT ([LICENSE-MIT][license]).

<!-- crates.io resolves a relative link against the directory this readme was published from,
     which is not where the repository root is. Links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT
