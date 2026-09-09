# changelog

All notable changes to this crate are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[semantic versioning](https://semver.org/spec/v2.0.0.html) - with the usual pre-1.0 caveat that a
minor bump may break you.

## [unreleased]

### added

- The crate: the two providers this workspace already had, taken out of `kamchatka` and published
  on their own. `OpenAiCompatible` speaks the OpenAI chat-completions dialect and `Gemini` speaks
  Google's, both streamed, both retried, both interruptible, and both answering `Endpoint` as well
  as `nachalnik::Provider` so that one `Arc` holds either.

  Nothing about the code changed in the move. What changed is that it can now be depended on: the
  runtime ships no provider by design, and the only two complete implementations were locked
  inside a terminal program - one of them behind ratatui, crossterm, clap and landlock, the other
  in a crate marked `publish = false` for ever. An adopter's first task was a thousand lines of
  streamed HTTP.

- The second OpenAI-compatible implementation folds in. `nachalnik-utils` held one too - the one
  `nachalnik`'s examples and its live suite talk through - and it could not be merged with
  `kamchatka`'s while one crate was published and the other never would be. Six things it had and
  the published one did not:

  - `streaming(false)`, and the whole-answer path behind it. Worth having where nothing is
    watching an answer arrive, and the only way to reach some endpoints' non-streaming code, which
    is not always the same code as their streaming code.
  - `recording(true)` and `requests()`: every request the provider was asked to send, in order,
    for a caller that wants to assert on what actually went out rather than on what it believes
    went out. Off by default, because a session that ran all afternoon would otherwise hold every
    request it ever made.
  - `client()` and `client_with()`, and `with_client()` to send through one: several models on one
    host then share a connection pool, and the timeout is the caller's to set. Ten minutes by
    default, because reqwest's covers the whole request and a shorter one fires *during
    generation* - which surfaces as `error decoding response body` and looks like a network fault.
  - `attempts()`, how many HTTP requests this has made in its life. The field existed and was a
    backoff counter reset by every success; it is two fields now.
  - `labelled()`, which is what `nachalnik::ModelInfo::provider` reports. A panel comparing four
    models through three endpoints had three providers all called `openai-compatible`.
  - `out_of_quota`, for telling a daily limit from a momentary one. They are the same status code
    and the first is not worth waiting out.

  One thing came the other way. The reasoning figure a turn reports is now inferred from a
  `total_tokens` residual where the endpoint publishes no `reasoning_tokens` by name - the
  unpublished copy did that and the published one did not, and it is the case where a turn's cost
  is otherwise invisible. Both paths read it through one function now, and one `stop_reason`.

- Both dialects carry `nachalnik::Content::Blob`, each where its own API takes one: a `data:` URI
  inside a typed content part here, an `inline_data` part there. A list of content parts only
  where there is a blob to carry, because a plain string is what every endpoint speaking the
  conventional dialect accepts and some of the smaller ones accept nothing else - a turn with no
  picture in it goes out exactly as it did before.

  A turn that is a sentence *and* a picture - `Content::Blocks` holding both, which is the shape a
  multimodal client reaches for - goes out as both, in the order it was put in. Reading it as one
  content slot would have meant `to_text`, which names the picture instead of carrying it: right
  for a transcript, wrong for a request.

  Neither dialect accepts one in a *tool result*: `tool` content is a string in the first and a
  `functionResponse` object in the second. A tool that returned a picture therefore sends the
  sentence naming it, `[image/png, 12048 bytes]`, which is the answer `nachalnik-mcp` has always
  given and beats a 400 by enough to be deliberate about.

- The conformance suite, as `conformance`, off by default. It was written to keep three copies of
  this code honest and outlives them because what it holds is not agreement between copies but a
  list of shapes some server really sent, asked through a real socket. It is here for whoever
  writes a third provider.

- `OpenAiCompatible::with_context_limit` and `Gemini::with_context_limit`, for the two cases
  `probe` cannot settle: an endpoint that publishes no context length, and one whose published
  length is not what the model is really being served with. It replaces a `KAMCHATKA_CONTEXT_LIMIT`
  the providers used to read for themselves - see below.

### fixed

- `stream_options` follows whatever the request's parameters settled `stream` on, rather than
  whichever way the provider was built. It is wrong in both directions otherwise, and one of them
  is silent: sent to an endpoint that was asked for a whole answer it is a 400 about a field
  nobody set, and left off a request whose parameters turned streaming *on* - which is how a
  caller asks one question of a provider built for whole answers - the endpoint reports no usage
  at all and the turn goes into the record with its cost unknown.

- A rate limit that arrives as an `error` object inside a 200 is waited out like any other. This
  dialect answers a whole-answer request its upstream refused that way rather than with a status,
  and reading only the status turned two seconds of waiting into a hard failure. A spent daily
  quota is still told apart and not waited out, because it will still be spent in a minute.

### changed

- Neither provider reads the environment. `KAMCHATKA_API_KEY`, `KAMCHATKA_BASE_URL` and
  `KAMCHATKA_CONTEXT_LIMIT` were read inside the constructors, which was tolerable while the only
  caller was the program those variables are named after and is not something a library may do:
  where the requests go and which key pays for them are the caller's to decide and its business to
  say out loud. All three are arguments now, and `kamchatka` reads its own variables and passes
  them in.

- The two notices a provider writes for its caller no longer name a client's commands. `/model to
  pick one` and `/models lists them` were instructions for one particular terminal, handed to
  whoever else was reading.
