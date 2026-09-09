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

- `OpenAiCompatible::with_context_limit` and `Gemini::with_context_limit`, for the two cases
  `probe` cannot settle: an endpoint that publishes no context length, and one whose published
  length is not what the model is really being served with. It replaces a `KAMCHATKA_CONTEXT_LIMIT`
  the providers used to read for themselves - see below.

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
