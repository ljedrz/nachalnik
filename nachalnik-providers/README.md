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

The runtime ships no provider and never will — it has no network in it, and a kernel with an
opinion about who you talk to would be a kernel worth distrusting. That left every adopter writing
the same thousand lines of streamed HTTP before they could ask a model anything. This is those
lines, written once.

---

### 🗣 three dialects, one trait

| feature | what it speaks |
| --- | --- |
| `openai` (default) | `POST /chat/completions`, `choices[].delta`, tool calls assembled from fragments. OpenRouter, ollama, vLLM, LM Studio, Together, and most of the rest. With `responses(true)`, OpenAI's Responses API instead: `POST /responses`, ordered items, reasoning sealed in `encrypted_content` and sent back. `api.openai.com` and OpenRouter. |
| `gemini` | Google's `generateContent`: `candidates[].content.parts`, whole calls, ordered `thought` parts. |
| `anthropic` | Anthropic's Messages API: typed content blocks, call arguments streamed as `input_json_delta`, signed thinking blocks, the prompt cached by default. `api.anthropic.com`, and OpenRouter at `/api/v1/messages`. |
| `conformance` | the suite the dialects above are held to, for anyone writing another. Stands up real sockets; off unless asked for. |
| `system1` | not a dialect: a client for System One models - any of the ones OpenRouter serves, or an engine of one's own at the same route - which answer typed questions about a state with numbers rather than driving a turn. Nothing it returns reaches a kernel. |

Every dialect answers `Provider`, which is what the kernel asks through, and `Endpoint`, which is what the
program around it asks: where the requests are going, which model is being asked, what this
endpoint serves, and what the last retry was about. `Dialect` is the two together, so one
`Arc<dyn Dialect>` holds any of them, for the kernel and the program alike, and nothing above it
finds out which it got.

The second dialect is the one worth having for its own sake. Gemini answers with the parts of a
turn *in the order they were produced* — a thought, a sentence, a call, more thinking — and an
OpenAI-compatible shim in front of it has nowhere to put that: it flattens the turn into a
`content` string beside a `tool_calls` array and everything downstream reads a rearrangement. Here
it arrives as `Content::Blocks`, is counted and pruned like anything else, and goes back out the
same way — signatures attached to the parts they belong to, without which that API rejects the
next request.

The third is the same bargain with Anthropic's Messages API, whose turn is a list of typed blocks.
Its thinking is signed, and a turn that thought before calling a tool has to send that thinking
back with the result, signature and all; here each block's own fields ride back out on it, and
only the fields that API defines, so a session that started against another provider does not
send it a `thoughtSignature` it would refuse. Its prompt is cached by default, because that API
caches nothing unasked; `cache_control` among the parameters changes that, and `false` turns it
off.

Some of its models hold an edited history against signed thinking, and refuse a turn sent back
after anything before it was pruned or rewritten - which is what a session here does. [The
module's docs](https://docs.rs/nachalnik-providers/latest/nachalnik_providers/anthropic/) say which,
and what to do about it.

OpenAI's Responses API is the same bargain once more, and a mode of the first dialect rather than a
fourth: the same endpoint, key and listing, asked at `/responses`. A reasoning model's thinking
comes back there as an item whose `encrypted_content` is the thinking itself, sealed; chat
completions has nowhere to keep it, so every turn asked through it starts thinking from nothing.
Here it goes back with the call's result, and is read rather than redone.

---

### ⏳ waiting, and knowing what kind of waiting it is

A stream that has gone quiet, a request refused with a `Retry-After`, and one somebody pressed
escape on are three different answers to *send it again?*, and getting them wrong costs either a
turn or somebody's money. All three are separated here, and shared by every dialect:

- **a stalled stream is interruptible**, so a server that takes a connection and goes away does not
  hold the program.
- **the silence is reported** through `Endpoint::take_notice`, and the attempt is given up on after
  150 seconds. A stream that had said something keeps it, as a turn cut off. A request that reached
  the server is not sent again, since it may be working on it and a second try is a second bill.
- **a busy server is retried, a spent quota is not.** A `Retry-After` longer than a minute is a
  daily limit, not a busy server.
- **an interrupt is an answer, not an error.** It comes back as `StopReason::Other("interrupted")`
  with whatever had arrived.

Every request is sent at most four times and **every attempt is billed**, which is why the retry is
for a server that said *busy*, not for a request that is simply large.

---

### 🧾 what actually went out

Streamed is the default. `streaming(false)` asks for the answer in one piece, which is what a
benchmark or a batch wants; an interrupt then comes back empty, since a whole answer has no middle
to keep.

```rust
let provider = OpenAiCompatible::new(model, base_url, key)
    .streaming(false)
    .recording(true);
// ... a turn later
assert_eq!(provider.requests()[0].params["max_tokens"], json!(1));
```

`recording(true)` keeps a copy of every request the provider was asked to send, and `requests()`
hands them back - off by default, since a long session would hold every request it made. `render`
says what will be sent; this says what was.

---

### 🔇 what it does not do

It reads **no environment**. Where the requests go, which key pays for them and what limit to
measure against are the caller's to decide and its business to say out loud; a library that
quietly picked up `OPENAI_API_KEY` would be spending somebody's money on the strength of a
variable they exported for another reason.

It **prints nothing**. Fragments are reported through `nachalnik::DeltaSink` and the screen
belongs to whoever owns it.

It **invents no parameters**. What the caller set goes out verbatim, beside the conversation and
never in place of it: a parameter named after a field the request is built from, `messages` or
`tools`, is left off.

---

### 🧪 tests

`cargo test -p nachalnik-providers --all-features` talks to a real socket wherever what is tested
lives inside `respond`, and every dialect is held to one conformance suite, so a case added once
applies to all of them. Each case is a shape some endpoint actually sent.

---

### 📜 license

MIT ([LICENSE-MIT][license]).

<!-- crates.io resolves a relative link against the directory this readme was published from,
     which is not where the repository root is. Links into the tree are absolute. -->

[nachalnik]: https://github.com/ljedrz/nachalnik/tree/HEAD/nachalnik
[license]: https://github.com/ljedrz/nachalnik/blob/HEAD/LICENSE-MIT
