# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- `too_long`, and both dialects return a `nachalnik::TooLong` where a refusal is one. The wordings
  differ per vendor and the arithmetic does not, so it reads one number out of the prose - the
  request, which is the largest token count such a complaint can name - and takes the limit from
  what the provider already knows the model to hold. Reading that out of the sentence too is what
  the second number in `you requested about 92674 tokens (92174 of text input, 500 in the output)`
  would have cost: an overshoot of 500 reported where it was 27,138, which is the figure somebody
  prunes against. The known limit is also what says the reading is sane, since a refusal of this
  kind names a request larger than the model; without one, a message naming a single number is
  left as prose, because reading it as the wrong one of the two calibrates a counter down on its
  way to a refusal.

  The sentence also has to *name* that limit, which is the one thing that says it is counting in
  the same units. The same model id at the same address refuses in two voices: the aggregator's
  own, which quoted a 65,536-token window against a request it put at 71,311 where the counter had
  said 71,231 - and the model behind it, which quoted a 131,072-token window in its native
  tokenizer against bytes the aggregator had counted as fitting. Both are true and only one is in
  the units this session is held to. Reading the second would name tens of thousands of tokens
  that were never there and teach a counter a scale belonging to somebody else's tokenizer; it is
  left as the sentence it arrived as, which says the problem in words.

### fixed

- `Gemini::set_endpoint` says when the new address does not serve the model, which it did only when
  a model was named beside the address. Given none, the old name is kept - and a name that was
  right at the last address is exactly the one worth asking about at this one, which is what the
  other dialect has always done. Without it the first word on the subject was a 404 on the next
  request. `tests/switching.rs` holds both dialects to it, along with the other half of the
  promise: an endpoint that lists nothing has not said the model is absent, and neither dialect may
  read its silence as a denial.

### changed

- `OpenAiCompatible::client` no longer describes its timeout as longer than reqwest's default.
  reqwest has no default request timeout, so the client `OpenAiCompatible::new` builds for itself
  has none at all - which is usable rather than a trap, because what ends a request that has said
  nothing is the silence watch this crate counts for itself. Documentation only.

### added

- `OpenAiCompatible::thinking_in_content`, on by default: takes thinking a model wrote into its own
  content back out of it. A model whose chat template ends the prompt inside a thinking block never
  writes the opening `<think>`, and an endpoint with no reasoning parser passes the lot through as
  content - so the answer arrives with its thinking on the front, a bare `</think>` in the middle,
  and `ModelResponse::reasoning` as `None`. The thinking before the first closing tag becomes the
  reasoning and what follows is the answer.

  Guarded twice, since the alternative is emptying the answer of every model that has no thinking at
  all: only the **first** closing tag is a delimiter, and only where the endpoint reported no
  reasoning of its own. `thinking_in_content(false)` turns it off, worth doing where a model may
  write the characters and mean them. The whole response stays on `ModelResponse::raw`.

  One reader, both wire paths. The *live* fragments are not split, since nothing watching them
  arrive knows a `</think>` is coming until it does, and holding them back would leave a model that
  never writes one silent to the end of its turn.
- `OpenAiCompatible::filed_under`: `X-OpenRouter-Categories`, a comma-separated list beside the
  referer, which is what puts an app in the [marketplace](https://openrouter.ai/apps) rather than
  only in the rankings. An unrecognised category is dropped silently rather than refused, so nothing
  here checks the spelling and there is no list of category names in this crate. OpenRouter
  documents two per request and ten in total, merged across requests. Sent only where `on_behalf_of`
  named an app, and only to the endpoint that reads it, since the page is built against the URL.
- `OpenAiCompatible::unlisted`: `X-OpenRouter-App-Visibility: hidden`, for attribution that is
  telemetry rather than a listing. It reaches only the request that *creates* the page, so a URL
  that already has one keeps its visibility - which is what makes it safe to expose, since a caller
  of somebody else's app cannot hide it. Public is the default and sends nothing.

### changed

- One fewer direct dependency: `async-trait` is the runtime's, and every `#[async_trait]` here is
  already written against `nachalnik`'s re-export. Depending on it twice let the two drift.
- reqwest is built without `charset`, which drops `encoding_rs`, `mime` and the six SIMD crates
  `encoding_rs` pulls - eight in all, for a crate that reads nothing but JSON and SSE, both of
  which are required to be UTF-8. What changes is that `text()` reads lossy UTF-8 instead of
  decoding by the `Content-Type`, so a non-conforming endpoint's *error* body may come back with
  replacement characters; nothing that gets parsed is affected. The feature is additive, so a
  caller who wants the decode enables it from their own manifest.

### fixed

- `with_client` documents that a caller's own client needs `install_crypto` first. reqwest is built
  here with `rustls-no-provider`, so `ClientBuilder::build` panics until something has installed a
  process default - recommending `aws_lc_rs`, which is reqwest's suggestion and not the provider
  this crate uses. Found in this crate's own attribution tests, where three of four failed depending
  on which thread installed the provider first.

## [0.2.1] - 2026-09-15

### fixed

- Two text items merged into one Gemini turn no longer run into each other. The API concatenates the
  parts of a turn with nothing between them, and the provider merges consecutive same-role messages
  because the dialect alternates - so a note ending `...the codename is kotelnaya` followed by
  `what is the codename?` reached the model as `kotelnayawhat is the codename?`. Text against text
  only; the boundary is normalised, so the same request renders to the same bytes twice.

## [0.2.0] - 2026-09-11

### changed

- A stall says an interrupt gives up on it, where it used to say `esc` does, in all three places.
  This crate is handed a `DeltaSink` and asks whether it has been interrupted; how a caller sets it
  is none of its business, and a headless run was printing `esc gives up on it` down a pipe. The
  sentence is written once, in `waiting`, which keeps `not_answered` apart from `gone_quiet`.
- Requires `nachalnik` 0.5.0, whose `PermissionPolicy::why` takes a `PermissionRequest` rather than
  a `ToolCallId`. Nothing in this crate's API moved, but it names runtime types throughout its
  public interface, so this release cannot be mixed with a 0.4-series runtime.

## [0.1.0] - 2026-09-10

### added

- The crate: the two providers this workspace had, taken out of `kamchatka` and published on their
  own. `OpenAiCompatible` speaks the OpenAI chat-completions dialect and `Gemini` speaks Google's,
  both streamed, retried and interruptible, and both answering `Endpoint` as well as
  `nachalnik::Provider`. The runtime ships no provider by design, and until now the only two
  complete implementations were locked inside a terminal program.
- The second OpenAI-compatible implementation folds in - the one `nachalnik`'s examples and live
  suite talk through. Six things it had that the published one did not:
  - `streaming(false)` and the whole-answer path behind it, the only way to reach some endpoints'
    non-streaming code.
  - `recording(true)` and `requests()`: every request sent, in order, for a caller that wants to
    assert on what actually went out. Off by default.
  - `client()`, `client_with()` and `with_client()`: several models on one host share a connection
    pool, and the timeout is the caller's. Ten minutes by default, because reqwest's covers the
    whole request and a shorter one fires *during generation*, surfacing as `error decoding response
    body`.
  - `attempts()`, separate from the backoff counter every success resets.
  - `labelled()`, which is what `nachalnik::ModelInfo::provider` reports.
  - `out_of_quota`, for telling a daily limit from a momentary one - same status code, and the first
    is not worth waiting out.

  One thing came the other way: the reasoning figure is inferred from a `total_tokens` residual
  where the endpoint publishes no `reasoning_tokens` by name. Both paths read it through one
  function now, and one `stop_reason`.
- A blob that is not a picture goes out as a `file` part. The media type picks between the OpenAI
  dialect's two payload shapes, and a PDF sent as `image_url` is a 400. `filename` comes from
  `Blob::meta["name"]`, derived from the media type when nobody supplied one. `input_audio` is
  deliberately absent: nothing here produces a recording, so it would be a shape written from
  documentation and pinned by no test.
- Both dialects carry `nachalnik::Content::Blob`, each where its own API takes one: a `data:` URI in
  a typed content part, an `inline_data` part. A list of content parts only where there is a blob,
  since a plain string is what every endpoint accepts and some smaller ones accept nothing else. A
  turn that is a sentence *and* a picture goes out as both, in order. Neither dialect accepts one in
  a *tool result*, so a tool returning a picture sends the sentence naming it.
- The conformance suite, as `conformance`, off by default: a list of shapes some server really sent,
  asked through a real socket. For whoever writes a third provider.
- `OpenAiCompatible::with_context_limit` and `Gemini::with_context_limit`, for the two cases `probe`
  cannot settle: an endpoint publishing no context length, and one whose published length is not
  what the model is really served with.

### fixed

- A refusal whose body is a web page is reported as its words. A base URL pointing at a site rather
  than an API is a common typo, and the first three hundred characters of the resulting 405 are a
  doctype and the opening of a stylesheet. Tags come off, `<style>` and `<script>` go whole, and a
  short page keeps its sentence.
- `stream_options` follows whatever the request's parameters settled `stream` on, rather than how
  the provider was built. Wrong in both directions otherwise, and one is silent: sent to an endpoint
  asked for a whole answer it is a 400, and left off a streaming request the endpoint reports no
  usage and the turn is recorded with its cost unknown.
- A rate limit arriving as an `error` object inside a 200 is waited out like any other. A spent
  daily quota is still told apart and not waited out.

### changed

- Neither provider reads the environment. `KAMCHATKA_API_KEY`, `KAMCHATKA_BASE_URL` and
  `KAMCHATKA_CONTEXT_LIMIT` were read inside the constructors; all three are arguments now.
- The two notices a provider writes for its caller no longer name a client's commands.
