# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- **`system1::Client` reaches Clef and Clef-flash on Cloudflare's Workers AI.** An address on
  `api.cloudflare.com` under an account's `/ai/run` - `system1::is_workers_ai` - is posted to under
  the model's name, `@cf/cloudflare/clef`, rather than at `/systemone`, with the short name the
  schema there takes (`clef`) in the body; the model's whole URL off its page is taken as the
  address too. The answer is read out of the `result` that API wraps every response in, a refusal
  out of its `errors` list with the code, and no listing is asked for, since there is none at that
  address. Every other address is asked exactly as before.
- **`Attribution`, at the crate's top level, and `attributed_to` on both clients that send it.**
  `OpenAiCompatible::attributed_to` and `system1::Client::attributed_to` take the same value, so a
  program asking OpenRouter for a conversation and for advice is one app to it, and one switch
  stops both. It goes only where `is_openrouter` says, as before; `on_behalf_of`, `filed_under`
  and `unlisted` on `OpenAiCompatible` still work, and edit the one it holds.

- **`Anthropic`, behind the `anthropic` feature: Anthropic's Messages API as a `Dialect`.** A turn
  comes back as `Content::Blocks` in the order its blocks were opened - thinking, text, `tool_use` -
  and goes back out the same way, each thinking block with its `signature`, which a turn that
  thought before calling a tool has to carry or the next request is refused. Only the fields this
  API defines go back on a block, so a session that started against Gemini sends it no
  `thoughtSignature`, and unsigned thinking from another provider is left out rather than refused.
  Call arguments are assembled from `input_json_delta` fragments; cached prompt tokens are added
  into `Usage::input_tokens`, which this API reports apart. It speaks to `api.anthropic.com` and to
  OpenRouter at `/api/v1/messages`, whose additions to the format - `provider`, `cost`, a `[DONE]`
  after `message_stop` - are read past. The prompt is cached by default, since this API caches
  nothing unasked: a breakpoint at the end of the conversation and one at the end of the
  instructions, both replaced by a `cache_control` parameter and dropped by a `false` or `null`
  one.
- **`Conformance::anthropic` and `conformance::Dialect::Anthropic`**, so the suite holds the third
  dialect to every case it can express - from streams recorded off the real API.
- **`OpenAiCompatible::responses`: OpenAI's Responses API, as a mode of the OpenAI dialect.** The
  same address, key, listing and attribution, asked at `/responses`: a turn comes back as
  `Content::Blocks` in the order of its items - reasoning, a message, calls - and goes back out the
  same way, each reasoning item with the `encrypted_content` that is its thinking sealed. Asked with
  `store: false` and with `reasoning.encrypted_content` in `include`, so nothing is kept on the
  server and the thinking reaches the next request anyway - measured on `gpt-5-nano`, the request
  carrying a call's result spent no reasoning tokens with it and 64 without. Instructions stay where
  they were put rather than being gathered at the top, so the prefix this API caches by holds. It
  speaks to `api.openai.com` and to OpenRouter's `/api/v1/responses`; `asks_responses` says which
  mode a provider is in, and `Dialect::projection` follows it.
- **`Conformance::responses` and `conformance::Dialect::Responses`**, which the mode passes in every
  case it can express, numbered-from-one and summaries in parts among them.

### fixed

- **A failure reported as an event is found in the two places the Responses API and Anthropic's put
  it, and a busy one is waited out there too.** The Responses API's `error` event carries its
  `message` at the top and `response.failed` nests it under `response`, and neither was read as a
  failure; its codes and Anthropic's are names - `rate_limit_exceeded`, `overloaded_error` - which
  read as `0` were never retried. They are the statuses they name now, and "the first event" is
  the first that carries anything, past the two a Responses stream opens with.

### changed

- Requires `nachalnik` 0.9. Nothing here uses what the runtime added in it, but every provider
  names runtime types throughout its public interface, so this release cannot be mixed with a
  0.8-series runtime.
- **`openai::Attribution` is `Attribution`**, re-exported under the old path, with `categories`
  and `unlisted` on it beside `url` and `title`; its `Default` names no app and sends nothing.
- **The dependency requirements are raised to what the whole workspace resolves to:** `tokio` 1.47.
  Each was lower than a version another dependency already asks for, so no build could have had
  it; CI now builds every crate here with each direct dependency at its floor at once, on the MSRV.
- **`tokio` is asked for `time` alone**, without `macros` and `rt`, which nothing outside the tests
  used; `conformance` still asks for `rt` itself. A crate that was getting either from this one
  without asking `tokio` for it has to ask now.
- **The MSRV is 1.95**, from 1.88. Edition 2024 already needed 1.85, and the resolver an
  edition-2024 project gets picks, for somebody on an older toolchain, the last version that still
  builds on it.

## [0.8.1] - 2026-10-05

### changed

- **The dependency requirements name the lowest versions this crate builds against**, rather than
  the first of each major, which it did not: `reqwest` 0.13.2, `rustls` 0.23.27, `serde_json`
  1.0.127 and `tokio` 1.40. The versions it is built and released with are unchanged, and cargo
  still picks the newest compatible ones; what changes is that a lockfile holding an older one is
  updated rather than failing to compile.
- **An empty key is no key.** Every client leaves the `Authorization` or `x-goog-api-key` header out
  rather than sending it with nothing in it, which is what a model served locally - ollama,
  llama.cpp, vLLM - wants, and what a proxy in front of one may refuse.

### fixed

- **A reasoning count of zero the OpenAI dialect is given is kept as zero.** A reported
  `"reasoning_tokens": 0` came back as `None`, which `Usage::reasoning_tokens` documents as the
  provider not having said; Gemini's dialect already kept its reported zero. A zero inferred from
  the total is still `None`.
- **A `Retry-After` given as a date is waited out until that date.** It was read as a number of
  seconds or not at all, so a date fell through to the doubling and the server was asked again,
  three times, before the time it had named. A date further off than the minute a refusal is
  waited out for is refused at once, as a number of seconds that long is.
- **The conformance suite's error inside a 200 no longer costs fourteen seconds.** It carried a
  `502`, which a provider that waits out a passing failure - as this crate's do - asked about three
  more times, two, four and eight seconds apart, before the case could pass. It carries a `400`.
- **A provider with no model yet is not told the address does not serve one.** Moved to an address
  with an empty name - which is what `/restart` does to a session started without a model - every
  dialect said that ` ` is not one of the models the address lists.

## [0.8.0] - 2026-10-04

### added

- **`Dialect::published` says what an endpoint publishes about a parameter beyond its name** - a
  default, and the largest value it takes - as a `Published`, and nothing where it publishes
  nothing, which is the default. `OpenAiCompatible` reads both from the entry the context limit
  comes from: OpenRouter's `default_parameters`, without the ones it gives as `null`, and
  `top_provider.max_completion_tokens` as the most `max_tokens` and `max_completion_tokens` take.
  The same cap is now its `ModelInfo::max_output_tokens`. A switch of model or address forgets all
  of it, as it forgets the list of names.

### changed

- **`system1` speaks to any System One model, and favours none.** `system1::Jev` is
  `system1::Client`, and the crate root no longer re-exports it. OpenRouter serves a family of
  these models, so the client has no model of its own: `Jev::latest`, `Jev::through_openrouter`,
  `DEFAULT_MODEL`, `OPENROUTER_BASE_URL` and `OPENROUTER_MODEL` are gone, and `DEFAULT_BASE_URL` is
  OpenRouter's `https://openrouter.ai/api/v1`. A provider OpenRouter does not list is reached
  through its bring-your-own-key.
- **Every question goes to `{base}/systemone`**, which is OpenRouter's route under its ordinary
  `/api/v1` and the one a self-hosted engine such as `laya-serve` keeps. The alpha `/decisions`
  route and the rule that chose between two services by the address are gone, so a session and its
  advisor can share one address and one key.
- **The listing is asked for the decision models** - `models?output_modalities=decisions`, read as
  OpenRouter's `data[].id` or an engine's `models[].name` - so a model OpenRouter does not serve is
  now reported at the probe, where before nothing was asked of OpenRouter at all.
- The live suite reads `OPENROUTER_API_KEY` and `NACHALNIK_SYSTEM1_MODEL` in place of
  `TYPESAFE_API_KEY`.

### fixed

- **The conformance suite asks why a turn with calls ended.** `two_calls` read the calls and not
  `stop`, so a provider that reported `EndTurn` for a turn the server ended with `tool_calls` passed
  every case. It now has to say `ToolUse`, in both dialects.
- **`Gemini`'s notice about a model the address does not list names three of those it does**, and
  counts the rest, as `OpenAiCompatible`'s does. It gave only the count, which left a mistyped
  name nothing to be corrected by.

## [0.7.0] - 2026-10-02

### changed

- Requires `nachalnik` 0.8, whose `ModelInfo` is `#[non_exhaustive]` and says where a provider
  sends its requests. Both dialects build their `ModelInfo` with its `with_` methods and report
  their base URL in `ModelInfo::endpoint`, and every provider here names runtime types throughout
  its public interface, so this release cannot be mixed with a 0.7-series runtime.
- **Both dialects say where their requests go.** `OpenAiCompatible` and `Gemini` report their base
  URL as `ModelInfo::endpoint`, without any `user:password@` or query string in it, since that
  goes into every record of a session. A `set_endpoint` that keeps the model's name is therefore
  a `model.changed` once the kernel is told, where it used to be nothing at all.
- **`system1` asks once more after a 502 or a 503.** It retried only the two statuses the service
  documents, 429 and 529, so a proxy's blip in front of it failed the question. A 502 or a 503
  now gets one more try and no more; any other 5xx still fails at once, because what is waiting
  on the answer is a person at a permission prompt.

### fixed

- **A stream silent before its headers is asked for once.** A streamed request the server took
  and said nothing to for `PATIENCE` was sent again, up to four times, on the reading that a
  stream's headers come before its first token. An endpoint that holds them until the first
  token - ollama while it loads a model - was then asked, and billed, four times. It now follows
  the whole answer's rule: only a request that never made its connection is sent again, and one
  that was taken and went quiet is given up on after one try's wait.
- **`Endpoint::host` leaves out a `user:password@`.** It left out a query string so that a key
  passed in one was not drawn on a status line, and drew a key passed before the host.

## [0.6.3] - 2026-09-29

### fixed

- **An advisor's notice about a model it does not list names three and counts the rest.** It named
  every model the endpoint lists, so pointed at OpenRouter it was one line of hundreds of names.
  It now says how many there are and names three, as the OpenAI dialect's notice does.

- **An advisor that could not be reached says why.** A question `system1` could not send failed
  with the transport's own line, which names the URL and nothing else, so a refused connection
  or a name that did not resolve read as `error sending request for url (...)`. It now carries the
  causes under that line, as a turn in either dialect already does.

- **`Endpoint::host` stops at a query or a fragment, not only at a path.** A base URL with no path
  and a query, such as `https://example.com?key=...`, came back with the whole query, so a key
  passed that way was drawn wherever the host is shown, and the host read as another one where it
  decides something - whether an endpoint is OpenRouter, which service `system1` is talking to.

- **A refusal quoting a page keeps words that the markup reading used to drop.** A tag whose
  name only started with `style` or `script`, such as `<stylesheet-error>`, had everything up to
  a matching close skipped as if it were a stylesheet, and a `<` that was not a tag, as in
  `retry in <60s`, took the rest of the body with it. Both now leave the words where they are.

- **A streamed summary of the thinking is kept when its words were already said.** A summary was
  passed over when its text appeared anywhere in the reasoning held so far, so one repeating a
  phrase of the streamed thinking, or of a longer summary before it, was dropped from the turn. A
  summary is now passed over only when the same summary has already been appended.

- **A switch to Google's compatible endpoint asks for its native listing once.** Where the
  conventional listing says nothing, the context limit and the names both fall back to the native
  one a path up, and each fetched it for itself, so every `set_model` and `set_endpoint` there
  sent the same request twice. It is read once per switch now.

## [0.6.2] - 2026-09-28

### fixed

- **Only an address that answers like an ollama is asked for one.** Every `/v1` base whose
  listing named no context length was sent `GET /api/ps` and then an unauthenticated
  `POST /api/generate` with an empty prompt, to make ollama load the model and say what `num_ctx`
  it is serving it with; against anything else those two were given fifteen seconds and two
  minutes to refuse a request it does not have, and a startup waited out both of them. A base is
  now asked for its version first, and neither probe goes out unless it answers `{"version":…}`.
- **A base URL ending in `/` is used without it.** Every path is appended to the base, so
  `…/v1/` asked for `…/v1//chat/completions` and a server routing on the path answered with a bare
  404. All three clients trim it, at construction and on `set_endpoint`, and `endpoint()` reports
  the trimmed address.
- **A 404 names the address it was asked at.** It is the answer to a wrong base URL, and what
  such a server sends back is usually its own "Not Found" and nothing else.
- **A request that never reached a server says why.** The transport's own line is its category
  and the URL - `error sending request for url (…)`, `builder error` - and the reason, such as
  `Connection refused` or `relative URL without a base`, was left in its chain, where a recorded
  error is never read. Both dialects now put the causes in the error's message.
- **`501 Not Implemented` and `505 HTTP Version Not Supported` are not retried.** Every 5xx was
  taken for a busy server and asked again with doublings, so an address serving something that
  does not take a `POST` - a plain file server - sat through three waits before saying so.
- **A switch that finds its model takes down the notice the last switch put up.** Two switches
  with nobody reading between them - a mistyped model, then the right one - left the first
  switch's "is not one of the models this address lists" waiting, to be read later as though it
  were about the model now in use. All three clients clear it.
- **A recording goes out as audio and a film as a video.** Every blob that was not a picture went
  out as a `file` part, which a `file` part does not refuse and does not carry: an mp4 came back
  answered `please upload the video` and a wav came back with nothing said at all, so a model was
  asked about bytes it had never received. `audio/wav`, `audio/x-wav`, `audio/mpeg` and
  `audio/mp3` now go out as `input_audio` with the format the field takes in place of the media
  type, and anything under `video/` as `video_url` with a data URL. A document, and an audio type
  this dialect has no word for, is still a `file`.
- **`info` and `respond` no longer deadlock when they run at once.** Both read the model and the
  context limit, and each held both locks together - `info` taking the limit first and `respond`
  the model first - so a caller reading `info` on one thread while a request began on another
  could leave both waiting for ever. A screen drawing the model's name every frame met it now and
  then. Each lock is now taken and let go in a statement of its own, in both dialects.
- **A rate limit that arrives inside a stream, before anything else in it, is waited out.**
  OpenRouter sends its `200` before the model has produced anything, so a `429` it meets after
  that - once its own failover has run out - arrives as the stream's first event, and the turn
  ended on it as a provider failure where the same `429` as a status was waited out. A refusal as
  the first event, or as the whole of a body that was not a stream, is now retried on the same
  terms as one sent as a status. One after the answer has started is not retried, since what
  arrived before it has already been handed on.
- **A parameter no longer replaces what a request is built from.** One named `messages`, `tools`
  or `model` in the OpenAI dialect, or `contents`, `systemInstruction` or `tools` in Gemini's, was
  written over the field the request had built, so the conversation that went out was not the one
  `model.requested` named - and a `null` one failed every request after it. Those parameters are
  now left off the wire, in both dialects.
- **A stream that goes quiet keeps what it said.** A server that sent the finish and the usage and
  then neither `[DONE]` nor a close sat out the whole 150s stall bound and ended the turn as a
  failure, with the answer and its usage thrown away; one that stalled part-way through an answer
  lost what had streamed. A finished answer now ends once the quiet after it is worth mentioning,
  and a stall mid-answer keeps what arrived as a turn cut off, as a stream broken off already did.
- **A failure reported inside a stream after the answer has started keeps what arrived.** The
  turn ended as a provider failure and the kernel recorded nothing, so the part of the answer that
  had streamed - generated and billed - was on the screen and nowhere else. It is now a turn cut
  off, as a stream broken off is, with the server's sentence as the notice.
- **A stream that closes cleanly before saying the turn is over is cut off.** A clean close was
  taken for the server's own end, so a stream that stopped after half an answer - and half a line
  of the next event - was recorded as finished for a reason `unreported`, with nothing said. With
  neither a finish nor `[DONE]` it is now a turn cut off, with a notice; either one on its own
  still ends the turn as it did.
- **Arguments streamed as an object are the call's arguments.** A server that sends the object
  where the dialect says a string had it read as `{}` on the streamed path, while the whole-answer
  path already took it as it was.
- **A call's name is written once, and two whole calls under one identifier are two calls.** A
  server that repeats the name on every fragment made `fsfs`, and two calls sharing an identifier
  with no index folded into one such call with both sets of arguments run together, `_unparsed`.
  A name equal to the one held is no longer appended, and a whole call arriving after a complete
  one under the same identifier starts a call of its own.
- **A server that never answers is said to have been asked four times.** A stream whose headers
  never came is sent again, up to four tries of 150s each, and the error then said "giving up
  after 150s" over ten minutes of waiting. It names the tries and each one's wait, and the readme
  says which silences are sent again and which are not.
- **A byte-order mark before a stream's first event does not cost the event.** The mark is not
  whitespace, so the first line did not begin with `data:` and was skipped.
- **An event spread over several `data:` lines is read as one.** The format allows it, joined with
  newlines and ended by a blank line, and each line was read as an event on its own and dropped.
  A line that parses alone is still an event at once, so a server that sends no blank lines
  between events reads as it did.
- **A `[DONE]` the endpoint split across two `data:` lines ends the stream.** The sentinel was
  matched on one line, while the reader beside it had already learned that an event may be
  spread over several. A server that re-wraps a stream - which a proxy is enough for - writes
  it that way, and the split was dropped as something that would not parse, so an answer that
  had already arrived sat out the whole stall bound and was reported as an interrupt.
- **A stream that carried no choice is refused, as the same body is refused whole.** The check
  for a completion ran on a whole answer and on a body that was never a stream, and on neither
  the streamed path nor the events in it, so `{"choices":[]}` finished a turn with nothing said
  and a session that ended normally. The server's own body is in the error, as it is on the
  other two paths.
- **A body that is not JSON is read the same way whichever path it arrives on.** The
  non-streaming path quoted the first three hundred characters verbatim where the streaming
  path takes the words out of them, so a web page in place of an answer - what a mistyped
  `base_url` produces - put its doctype, its tags and its whole stylesheet into the
  conversation, the session log and any file a user is invited to send on.
- **A body the endpoint compressed is named rather than quoted.** The bytes of one are not the
  words of it, and quoted as prose they are a third of a header with a replacement character
  wherever a byte was not UTF-8. The error now says which encoding arrived and asks for it
  uncompressed, which is the diagnosis a reader needs: an endpoint that compressed its answer
  and one that sent nonsense are different faults, and nothing told them apart.
- **Arguments that are neither a string nor an object are shown to the model.** A number, a
  list or a boolean was read as nothing written, which is a call to a tool that takes no
  arguments, and the tool was run on that. Streamed, such a value contributed no fragment at
  all, so a call whose arguments arrived as `42` and then as a string kept the second half
  only. Both are `_unparsed` now, as a string that will not parse already was.
- **A turn the server ended on its own `[DONE]` is `EndTurn`, not `unreported`.** The marker is
  the dialect's own end of the turn, and a turn ended on purpose is not one whose reason nobody
  gave - which is the word left for a stream cut off before it said anything.
- **A listed `context_length` of `0` is no limit.** It was reported as the model's window, so a
  client showed a context of `0` tokens and measured its budget against it; a limit elsewhere in
  the entry is read instead, and failing that the limit is unknown.

## [0.6.1] - 2026-09-24

### fixed

- **A long line arriving over many chunks is read in time linear in its length.** The reader
  searched for its newline from the start of the line at every chunk, so a Gemini call's arguments
  or an image, one event of megabytes, cost time quadratic in its size.
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
- **Three more conformance checks fail a provider they used to pass.** The fragmented and the
  broken arguments each want exactly one call back, where a provider that also made a call of
  every fragment passed on the first; and two reasoning summaries want to be kept in the order
  they came.
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
