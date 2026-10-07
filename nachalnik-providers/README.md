# nachalnik-providers

[![crates.io](https://img.shields.io/crates/v/nachalnik-providers.svg)](https://crates.io/crates/nachalnik-providers)
[![docs.rs](https://docs.rs/nachalnik-providers/badge.svg)](https://docs.rs/nachalnik-providers)
[![CI](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml/badge.svg)](https://github.com/ljedrz/nachalnik/actions/workflows/ci.yml)

**Model API clients for [`nachalnik`][nachalnik].**

```rust
let provider = Arc::new(OpenAiCompatible::new(model, base_url, api_key));
provider.probe().await;

let kernel = Kernel::new(Config::default());
kernel.set_provider(provider);
```

The runtime has no providers and never will: it does no networking, and a kernel that preferred
one vendor would be harder to trust. Without this crate, everyone using the runtime would have to
write the same thousand lines of streaming HTTP code before talking to a model.

---

### 🗣 three APIs, one trait

| feature | what it supports |
| --- | --- |
| `openai` (default) | `POST /chat/completions`, `choices[].delta`, tool calls assembled from fragments: OpenRouter, ollama, vLLM, LM Studio, Together, and most others. With `responses(true)`, OpenAI's Responses API instead: `POST /responses`, ordered items, and reasoning returned encrypted in `encrypted_content` and sent back. Served by `api.openai.com` and OpenRouter. |
| `gemini` | Google's `generateContent`: `candidates[].content.parts`, whole tool calls, ordered `thought` parts. |
| `anthropic` | Anthropic's Messages API: typed content blocks, tool call arguments streamed as `input_json_delta`, signed thinking blocks, and prompt caching on by default. Served by `api.anthropic.com`, and by OpenRouter at `/api/v1/messages`. |
| `conformance` | the test suite every client above must pass, for anyone writing another. Opens real sockets; off by default. |
| `system1` | not a chat API: a client for System One models (the ones OpenRouter serves, or your own server at the same route), which answer typed questions about a state with numbers instead of taking turns. Nothing it returns reaches a kernel. |

Every client implements `Provider`, which the kernel uses, and `Endpoint`, which the program around
it uses: where requests go, which model is used, what models the endpoint offers, and why the last
retry happened. `Dialect` combines the two, so one `Arc<dyn Dialect>` can hold any of them, and the
code using it doesn't need to know which.

The Gemini client is the main reason this crate has more than one API. Gemini returns the parts of
a turn *in the order they were produced* (a thought, a sentence, a tool call, more thinking), and an
OpenAI-compatible wrapper in front of it can't represent that: it squashes the turn into a `content`
string next to a `tool_calls` list, losing the order. Here the turn arrives as `Content::Blocks`, is
counted and pruned like anything else, and is sent back the same way, with each signature attached
to its part, which Gemini requires on the next request.

The Anthropic client does the same for Anthropic's Messages API, where a turn is a list of typed
blocks. Its thinking is signed, and a turn that thought before calling a tool must send that
thinking back, signature included, with the tool result. Each block is sent back with its own
fields, and only fields Anthropic defines, so a session that started with another provider doesn't
send a `thoughtSignature` that Anthropic would reject. Prompt caching is on by default, because the
API caches nothing unless asked; the `cache_control` parameter changes that, and `false` turns it
off.

Some Anthropic models reject a signed thinking block if anything before it in the history was
pruned or edited, which sessions here do. [The module's
docs](https://docs.rs/nachalnik-providers/latest/nachalnik_providers/anthropic/) list which models,
and what to do about it.

OpenAI's Responses API is a mode of the OpenAI client rather than a separate one: same endpoint,
key and model list, at `/responses`. A reasoning model's thinking comes back as an item whose
`encrypted_content` holds it in encrypted form. Chat completions has nowhere to keep that, so every
turn there starts reasoning from scratch; here it is sent back with the tool result and reused.

---

### ⏳ telling different kinds of waiting apart

A stream that has gone quiet, a request refused with a `Retry-After`, and one the user interrupted
need different answers to *should we send it again?*, and getting it wrong costs either a turn or
money. All three are handled separately, the same way for every client:

- **A stalled stream can be interrupted**, so a server that accepts a connection and then goes
  silent doesn't hang the program.
- **Silence is reported** through `Endpoint::take_notice`, and the attempt is abandoned after 150
  seconds. If the stream had already sent something, that part is kept as an interrupted turn. A
  request that reached the server isn't sent again, since the server may still be working on it and
  a second attempt would be billed again.
- **A busy server is retried; an exhausted quota is not.** A `Retry-After` longer than a minute
  means a daily limit, not a busy server.
- **An interrupt is a normal result, not an error.** It returns `StopReason::Other("interrupted")`
  with whatever had arrived.

Each request is sent at most four times, and **every attempt is billed**, so retries are only for a
server that said it's busy, not for a request that is just slow because it's large.

---

### 🧾 what was actually sent

Streaming is the default. `streaming(false)` asks for the whole answer at once, which suits
benchmarks and batch jobs; an interrupt then returns nothing, since there's no partial answer to
keep.

```rust
let provider = OpenAiCompatible::new(model, base_url, key)
    .streaming(false)
    .recording(true);
// ... a turn later
assert_eq!(provider.requests()[0].params["max_tokens"], json!(1));
```

`recording(true)` keeps a copy of every request the provider sends, and `requests()` returns them.
It's off by default, since a long session would keep every request it made. `render` shows what
will be sent; this shows what was.

---

### 🔇 what it doesn't do

It reads **no environment variables**. Where requests go, which key pays for them and what limit to
use are for the caller to decide and state explicitly; a library that read `OPENAI_API_KEY` by
itself could spend someone's money with a key they set for a different program.

It **prints nothing**. Streamed fragments are reported through `nachalnik::DeltaSink`, and the
screen belongs to whoever owns it.

It **adds no parameters**. Whatever the caller set is sent unchanged, alongside the conversation and
never instead of it: a parameter with the same name as a field the request is built from
(`messages` or `tools`) is left out.

---

### 🧪 tests

`cargo test -p nachalnik-providers --all-features` uses a real socket wherever the code under test
is inside `respond`, and every client must pass one shared conformance suite, so a case added once
applies to all of them. Each case is a response some real endpoint actually sent.

---

### 📜 licence

MIT ([LICENSE-MIT][license]).

<!-- crates.io resolves relative links against the directory this README was published from, not
     the repository root, so links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT
