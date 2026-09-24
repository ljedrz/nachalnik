# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### fixed

- **A `Jev` answer that arrives as a 200 but is not JSON is an error.** A body cut short or a
  proxy's page came back as `Ok` with every question unanswered, which is also what a model that
  declined all of them looks like; the dialects already refused one.
- **Gemini reports a turn that ran out of room as `Length`, even beside a call.** A turn that asked
  for a tool and hit `MAX_TOKENS` came back as `ToolUse`, which hid the one thing said nowhere
  else; the calls run from the blocks either way. The OpenAI dialect already did this.

- **`system1::Answer::confidence` is `None` for an answer that gave none.** A choice or score
  with no `confidence` was held as `NaN` and handed out as `Some(NaN)`, so a caller reading "no
  confidence" as `None` got a figure it printed as "NaN% sure", and one that serialised it wrote
  `null` where an `f64` was expected.

- **A whole answer with no `choices` is an error, not an empty turn.** `OpenAiCompatible` with
  `streaming(false)` took any JSON body without an `error` in it as a completion, so a proxy's
  `{"status":"ok"}` finished the turn with nothing said and a stop reason of `unreported`. It
  is refused with the start of the body, as a server that ignored `stream: true` already was, and
  so is a `choices` array with nothing in it.

- **`[DONE]` ends a stream.** It was skipped as a line that is not JSON, so a server that sent it
  and kept the connection open had a finished answer wait out the whole stall bound and then
  reported as a stall.

### changed

- **Two conformance checks fail a provider they used to pass.** The cut-off stream wanted one
  call to survive and took any call, so one kept with its arguments emptied passed; it wants the
  call as it arrived. The usage check read only what was sent, so a provider that lost the output
  figure passed; it wants both.
- **The conformance check for a body that is not a stream wants the page in the error.** It passed
  on any error at all, so a provider that never reached the fixture passed it; it now asks for the
  server's own words, as the check for a failure inside a 200 already did.
- **`OpenAiCompatible::set_model` and `set_endpoint` read the model listing once.** The limit and
  the check for the model each fetched it.

## [0.6.0] - 2026-09-23

### breaking

- **`system1::Question`, `system1::Answer`, `system1::Answers` and `openai::Attribution` are
  `#[non_exhaustive]`.** This crate had the attribute nowhere and wants it in four places, which
  is the rule the workspace holds itself to: a public enum the world can add to carries it, and so
  does a struct nothing outside the crate builds. A `match` on `Question` or `Answer` written
  elsewhere now needs a wildcard arm, and a struct literal naming every field of `Answers` or
  `Attribution` is no longer how one is made.

  It is worth a break now because it will not get cheaper. Three question types is what the
  engines answer today rather than what a question can be - the shapes are the upstream's to add,
  and this crate exists to speak whatever it grows - and what a response carries is the other
  end's to widen. Nothing in this workspace had to change: the accessors on `Answers` already
  match with a `_` arm, `kamchatka` builds its questions through `Question::noul`,
  `Question::choice` and `Question::score`, and `Attribution` is assembled by
  `OpenAiCompatible::on_behalf_of` and read by nobody.

### added

- **`system1::render`**, the documented request body for a model, a state and its questions,
  without a `Jev` to render it. `Jev::render` calls it, and so does an engine reached some other
  way - `kamchatka`'s local advisor, over a pipe - so the body is written once.

### changed

- The docs say that `OpenAiCompatible::new` takes the address and the key, with the limit from
  `with_context_limit`; that nothing in the crate reads the environment; that `SystemOne` has three
  methods; and that an endpoint listing only its sampling parameters leaves a set parameter
  unchecked rather than ignored.

### fixed

- **Thinking sent as `reasoning_content` is thinking.** DeepSeek, llama.cpp and vLLM's reasoning
  parser use that name, and it was read under `reasoning` alone, streamed or whole, so it reached
  only `raw` and the turn had none.

- **A call's arguments are what was written.** An empty string - a call to a tool that takes
  nothing - failed to parse and came back `_unparsed`, so the model was told its arguments were
  invalid and sent the same call again; it is `{}`. Arguments sent as an object where the dialect
  says a string were read as `{}`, and are taken as they are.

- **Reasoning is inferred from a total only where the prompt and the completion are both
  reported.** A prompt count left out was read as `0`, which made the whole prompt a residual
  billed as output.

- **A web page in place of an answer is read in a moment.** Taking the markup off lowercased the
  rest of the body at every tag, which for a page of megabytes was gigabytes of copying for one
  error message; it reads the first 64 KiB, and compares tag names in place.

- **A question about an endpoint gives up rather than waiting for ever.** A listing of models and
  the probes for a context limit ran on a client with no timeout and outside any turn, so nothing
  watched them and no interrupt reached them: an endpoint that took the connection and never
  answered held a startup, a model switch or a listing for ever. Each is bounded now - fifteen
  seconds, and two minutes for the request that makes ollama load a model - and answers what it
  answers when the endpoint cannot say.

- **A whole answer still being written is not asked for again.** With `streaming(false)`, the
  headers come with the last token, so no answer yet is a model still generating - which was taken
  for a busy server and sent again up to three more times, each one billed, before failing anyway.
  Only a request whose connection was never made is retried there now.

- **A stream that trickles can be stopped, and nothing one response sends is held without limit.**
  The interrupt was asked in the quiet and between lines, and a body arriving a byte at a time with
  no newline is neither, so escape did nothing for as long as it trickled; it is asked after every
  chunk now. A line, a body that was never a stream, and a whole answer are each read to 64 MiB
  and no further: past it a stream that has said something keeps it, as a stream cut off does, and
  one that has not is refused with a sentence.

- **A System One refusal that is a web page says what the page says.** A firewall in front of the
  service answers some requests with an HTML page, and the error quoted its first characters - a
  doctype and a stylesheet. It carries the page's words now, with the markup taken off, the way
  the chat dialects already read one.

- **`system1` builds on its own.** It read the shared retry count from `waiting`, which is only
  built for the chat dialects, so `--no-default-features --features system1` did not compile. The
  count lives beside the three of them now, and CI builds that configuration.

- **`jev` is sent a busy request as often as a dialect is, and no more.** `system1` had a
  `RETRIES` of its own with the dialects' value, counted without the first send, so a question to
  a server that stayed busy went out five times where a turn goes out four. It uses the dialects'
  count now.

- **A prompt Google blocks is a refusal.** It comes back with no candidate and the reason under
  `promptFeedback.blockReason`, which nothing read, so the turn was recorded empty with
  `StopReason::Other("unreported")` and the reason survived only in `raw`.

- **`OpenAiCompatible::set_endpoint` forgets the last address's parameter list.** `set_model` put
  it down and `set_endpoint` did not, and a probe only writes one where the new listing has one of
  its own - so a session moved from an endpoint that publishes its parameters to one that does not
  went on checking `/params` against the first server's list.

- **The key is never put in a URL.** The listing reads for a base ending in `/openai` - Google's
  compatible endpoint, and also any gateway laid out that way - asked the native listing one path
  up with `?key=` on the end, which is a secret in every log a proxy keeps. It goes in the
  `x-goog-api-key` header, which the native API takes and the Gemini dialect already used.

- **A caller's `stream_options` are added to, not replaced.** Streaming needs `include_usage`, and
  it was put in place of whatever options the parameters carried rather than beside them.

- **An empty identifier on a later tool-call fragment does not unname the call.** The lookup read
  one as naming nothing and the write took it anyway, so the call ended up with an empty identifier
  and its argument fragments were filed under two.

- **A request's retries are its own.** The count of how many times a request had backed off was
  one counter on the provider, shared by every request made through it and reset by any one that
  succeeded or gave up. Eight abreast against a busy endpoint - `nachalnik-eval`'s `bench -j 8`
  shares one provider across eight kernels - handed out attempts one to eight, so the fourth to fail
  gave up without a single retry, while a request whose failures kept landing between other
  requests' successes could retry without end. Each request counts its own now, in both dialects.

- **A stop pressed during a backoff ends it, and nothing is sent again.** The wait between
  attempts was one sleep, as long as the server asked - a `Retry-After: 30` was thirty seconds of a
  turn that would not stop - and at the end of it the request went out again anyway, to be answered
  and billed. The wait is in heartbeats that check for the interrupt now.

- **A spent daily quota answering a streamed request is not retried.** The whole-answer path told
  it apart from a momentary limit and the streaming path, which is the default, did not, so a quota
  that would still be spent tomorrow was sent three more times over fourteen seconds first.

- **Both dialects send, wait and read through one copy of the code.** Each had its own retry loop
  and its own stream reader, and they had drifted: `Gemini` read no `Retry-After`, refused nothing
  for asking to be left longer than a minute, and put a refusal's whole body into the error where
  the other dialect gave the server's sentence. They share `waiting` and a new `reading` now, and
  what follows was wrong in both and is fixed once:

  - the last event of a stream is read when no newline follows it, rather than dropped with the
    end of the body;
  - a good status whose body is not a stream is reported by what the whole body says, rather than
    by what followed its last newline - which for a page of HTML was its closing tag;
  - a server that ignored the request for a stream and answered whole has answered: a completion
    in the OpenAI dialect is read as one, and Google's unstreamed list of events as those events;
  - an error object inside a stream, or in a body that was not one, is reported by its sentence and
    read for a refusal over length, as a refused status already was;
  - a whole answer's body is watched as a stream is - interruptible, its silence reported, and
    given up on after `WHOLE_ANSWER` - where it was one `text()` that nothing could stop.

- **`generationConfig` is merged all the way down.** Merged one level deep over the default that
  asks for thoughts, a caller's `{"thinkingConfig": {"thinkingBudget": 1024}}` replaced the whole
  of `thinkingConfig` and turned the thoughts off without saying anything about them.

## [0.5.0] - 2026-09-21

### added

- **`system1::SystemOne`, the trait a caller holds.** One method - put these questions to this
  state - plus a name and a notice for a screen. `Jev` implements it, and so does anything else
  that answers typed questions.

  POSTPONED.md said a trait here would be a seam shaped around the only thing that fits it, and
  that was right until the second implementation turned out not to be a second *service*. The
  open engines ship as libraries, so reaching one means spawning a process, and this crate does
  not spawn processes - the line `nachalnik-mcp` exists on the other side of. So the trait is
  here, the local implementation is in the crate that already spawns things, and a caller holds
  `dyn SystemOne` without learning which it got.

  `Question::to_wire` and `Answers::read` are public with it, and `Jev::ask` now goes through
  the latter rather than parsing inline. One reader and one renderer for a documented format
  that two processes have to agree about - the argument `Jev::render` already carried, now with
  a second caller to make it true.

### breaking

- **The `typesafe` module and feature are `system1`.** `nachalnik_providers::typesafe::Jev` is
  `nachalnik_providers::system1::Jev`, `features = ["typesafe"]` is `features = ["system1"]`, and
  the live suite is `--test system1`. Nothing else moved: `Jev` keeps its name, because that is
  the model's name, and every type, constant and method in the module is where it was.

  A System One model is a category rather than a product - a claim to weigh, a closed set to pick
  from, an ordered rubric to place something on, under those three names - and the open engines
  arriving now have the same three. A module named for the company selling one of them was
  naming the shop rather than the goods, and it would have had to be renamed the first time a
  second client went in beside `Jev`.

  What a third *service* takes, as against a second engine, is still only an address:
  `Service::of` reads anything it does not recognise as keeping TypeSafe's paths, so a proxy or a
  self-hosted one answering `state` and `questions` there works through `Jev` unchanged. A second
  engine is what `SystemOne` above is for.

### fixed

- **A streamed fragment naming no call could land on the wrong one, and break two.** A `tool_calls`
  entry carrying neither an index nor an identifier is a continuation, and the accumulator gave it
  to the last call in the list. That is the call most recently *announced*, which stops being the
  call being *written* the moment an endpoint opens a second one - with `"arguments": ""`, as they
  do - while the first is still streaming. The next loose fragment then goes to the new call: the
  old one is short those characters and the new one carries them at the front, so one misfiled
  brace leaves two calls unparseable where there should have been none.

  It continues the call last written to now, falling back to the last announced where nothing has
  been written yet, and an empty `arguments` no longer counts as writing - the rule the content and
  reasoning branches beside it have always followed. Two conformance cases: several calls each
  fragmented, which nothing asked before, and a loose fragment after a second call has opened.

  No endpoint in this workspace is known to send that shape. What makes it worth closing is that it
  is the one way *this* end can produce the truncation a real endpoint produced -
  `inclusionai/ling-3.0-flash-vl:free` through Novita drops the last fragment of every call but the
  last - and telling the two apart should not depend on knowing that our version leaves the
  characters on the next call.

## [0.4.0] - 2026-09-19

### added

- `typesafe`, a feature and a module of its own: TypeSafe's `jev`, a System One model. It takes a
  state and a map of typed questions - a claim to weigh, a closed set to pick from, an ordered
  rubric to place something on - and answers each with probabilities, all of them in one request
  and each evaluated on its own against the same state. No text, no tool calls, nothing to stream,
  so what it is for is the decisions a program makes *around* a conversation rather than the
  conversation: whether to run that command, which of four branches this is, how bad the thing it
  just read is.

  The accessors on `Answers` refuse a type they were not asked for rather than defaulting. A
  `choice` read as a `noul` answers `None`, because handing a program a `0.0` nobody sent is worse
  than making it ask twice. `Answers::model` is the version that actually answered - a request
  naming `jev-latest` comes back saying `jev-1.13.0` - and is the one worth recording.

  Three things the reference does not say, found by asking the endpoint: a refusal arrives under
  `detail` rather than the `error` the rest of this crate reads, carrying an `error_type` beside
  the sentence; an unknown model is a 400 and not the documented 422; and a `score` with one level
  is documented as invalid and is accepted, coming back `score: 0.0` at `confidence: 1.0`. So a
  confidence says how concentrated a distribution is and nothing about whether the question was
  worth asking, which is said on `Answer::Score` where somebody reading one will need it.

  No trait for the System One shape. One vendor speaks it, so it would be a shape written from a
  specification with a single implementor - the reason `waiting.rs` gives for there being no
  `input_audio` - and inherent methods on `Jev` will do until a second turns up. `jev` does not
  touch `waiting` either, which stays gated to the two dialects: `watched` takes a `DeltaSink`, and
  a client that drives no turn has no business holding one.

- `Jev` speaks to the second service serving `jev`. OpenRouter resells it, takes the same
  `{model, state, questions}` body, and puts it behind `/decisions` on an `/api/alpha` path of its
  own - not the `/api/v1` its chat endpoint is on, which a decision sent there answers with a 404.
  Which of the two a client is talking to is read off the base URL it was given, the way the app
  headers are, so nothing has to be passed beside the address and `set_endpoint` moving a live
  client between them moves the path with it. `Jev::through_openrouter` is the pair of constants
  for it, as `Jev::latest` is TypeSafe's.

  A refusal is read out of whichever envelope it arrives in. TypeSafe's is `detail` with an
  `error_type`; OpenRouter's is the `error` the rest of its API uses, with a `code` that is a
  number at one service and a string at the other. Nothing holding one of these knows which
  answered, so both are read in one place.

  No listing is asked of OpenRouter. It publishes none on the path it takes these on, and the one
  it publishes elsewhere does not carry this model at all - it is served out of an alpha route
  `/api/v1/models` does not report. Asking that one would announce a model that is served as
  missing, which is the failure the empty answer already exists to avoid.

  `OPENROUTER_MODEL` names a version rather than a moving name, because there is no moving name to
  use: `typesafe/jev-latest` is not among the identifiers OpenRouter serves. It is a constant
  somebody has to bump, and `Answers::model` is what says which version actually answered.

- `is_openrouter`, which is the one address test the crate had been writing out per caller. Three
  things now turn it into a decision - whether to send the app headers that put a program in a
  public ranking, which of the two services serving `jev` a question is shaped for, and, in a
  caller, whether a session's own key may be spent anywhere else - and each is about somebody's
  credentials or somebody's data. `openai`'s `ranks_apps` keeps its name, since what it answers is
  whether there is a ranking to be listed in, and defers to this for the address.

  It reads the authority out of a whole URL as well as a bare host, which the private version never
  had to. `openrouter.ai.example.com` is the case that decides the shape of it: a `contains` or a
  suffix test over the raw address matches that host, and the callers would then name a program,
  shape a request and spend a key against somebody who merely put another name in front of their
  own.

### changed

- **`Endpoint` no longer requires `Provider`**, and the turn-driving half of the crate is
  `Dialect: Endpoint + Provider`. A model arrived that answers no turns and could not implement the
  old shape: `respond` hands back a `ModelResponse` with content or tool calls in it, and the only
  way to satisfy that from a probability distribution is to manufacture an assistant turn nobody
  said. What `jev` does have is every other half of a provider - an address, a key, a model
  identifier, a listing, a usage report, something to say when the server is busy - which is the
  half `Endpoint` was already about, and said it was about in its own docstring before requiring
  `Provider` anyway.

  Two members move down to `Dialect` with the bound, because both are questions about a *turn* and
  a model that produces none has no answer to either: `projection`, the shape of a message on the
  wire, and `lists_every_parameter`, which is about `ModelInfo::parameters` and arrives through
  `Provider`.

  What it costs a caller is the upcast the supertrait was providing. An `Arc<dyn Endpoint>` on its
  way to `Kernel::set_provider` is an `Arc<dyn Dialect>` now, which is what it always meant - both
  dialects in this crate implement it, so the change is at the holder rather than the
  implementation. A suite bounding on `Provider` directly is untouched.

## [0.3.0] - 2026-09-17

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

- `OpenAiCompatible::client` no longer describes its timeout as longer than reqwest's default.
  reqwest has no default request timeout, so the client `OpenAiCompatible::new` builds for itself
  has none at all - which is usable rather than a trap, because what ends a request that has said
  nothing is the silence watch this crate counts for itself. Documentation only.
- One fewer direct dependency: `async-trait` is the runtime's, and every `#[async_trait]` here is
  already written against `nachalnik`'s re-export. Depending on it twice let the two drift.
- reqwest is built without `charset`, which drops `encoding_rs`, `mime` and the six SIMD crates
  `encoding_rs` pulls - eight in all, for a crate that reads nothing but JSON and SSE, both of
  which are required to be UTF-8. What changes is that `text()` reads lossy UTF-8 instead of
  decoding by the `Content-Type`, so a non-conforming endpoint's *error* body may come back with
  replacement characters; nothing that gets parsed is affected. The feature is additive, so a
  caller who wants the decode enables it from their own manifest.

### fixed

- `Gemini::set_endpoint` says when the new address does not serve the model, which it did only when
  a model was named beside the address. Given none, the old name is kept - and a name that was
  right at the last address is exactly the one worth asking about at this one, which is what the
  other dialect has always done. Without it the first word on the subject was a 404 on the next
  request. `tests/switching.rs` holds both dialects to it, along with the other half of the
  promise: an endpoint that lists nothing has not said the model is absent, and neither dialect may
  read its silence as a denial.
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
