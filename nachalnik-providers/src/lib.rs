//! Providers for the [`nachalnik`] agent runtime: somebody else's HTTP API, read carefully.
//!
//! The runtime ships no provider and never will - it has no network in it, and a kernel that
//! opened a socket would be a kernel with an opinion about who you talk to. That leaves every
//! adopter writing the same thousand lines of streamed HTTP before they can ask a model anything,
//! which is what this crate is here to stop.
//!
//! Everything in here is an [`Endpoint`]: an address, a key, a model identifier, a listing, and
//! something to say when the server is slow. What splits the crate in two is whether the model
//! behind it answers in *turns*.
//!
//! A [`Dialect`] does. It implements [`nachalnik::Provider`], so a kernel can be handed one and a
//! session is what comes out. Two of them, each behind a feature:
//!
//! - [`openai`] - `POST /chat/completions`, `choices[].delta`, tool calls assembled from
//!   fragments. What OpenRouter, ollama, vLLM, LM Studio, Together and most of the rest speak.
//! - [`gemini`] - Google's own `generateContent`: `candidates[].content.parts`, whole calls, and
//!   ordered `thought` parts, which is the dialect [`nachalnik::Content::Blocks`] was built for.
//!
//! An `Endpoint` that is not a `Dialect` answers something other than a turn, and there is no
//! kernel in its path at all:
//!
//! - [`system1`] - System One models, of which this speaks to TypeSafe's `jev`. Typed questions
//!   put to a state and answered with probabilities: no text, no tool calls, nothing to stream.
//!   What it is for is the decisions a program makes *around* a conversation rather than the
//!   conversation.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use nachalnik::{Config, Kernel};
//! # use nachalnik_providers::OpenAiCompatible;
//! # async fn go() -> Result<(), nachalnik::BoxError> {
//! let provider = Arc::new(OpenAiCompatible::new(
//!     "openai/gpt-5",
//!     "https://openrouter.ai/api/v1",
//!     std::env::var("OPENROUTER_API_KEY")?,
//! ));
//! provider.probe().await;
//!
//! let kernel = Kernel::new(Config::default());
//! kernel.set_provider(provider);
//! # Ok(())
//! # }
//! ```
//!
//! note: neither dialect reads the environment. Where the requests go, which key pays for them
//! and what limit to measure against are the caller's to decide and its business to say out
//! loud: a library that quietly picked up `OPENAI_API_KEY` would be spending somebody's money on
//! the strength of a variable they exported for another reason. [`OpenAiCompatible::new`] takes
//! all three, and [`OpenAiCompatible::with_context_limit`] takes the override.
//!
//! note: both providers report fragments through a [`nachalnik::DeltaSink`] and print nothing.
//! The screen belongs to whoever owns it, and a provider writing to it would be drawing over
//! somebody's frame.

#![deny(unsafe_code)]
#![deny(missing_docs)]

use std::ops::RangeInclusive;

#[cfg(any(feature = "gemini", feature = "openai"))]
use nachalnik::BoxError;
use nachalnik::{Overrun, TooLong};

mod endpoint;

#[cfg(feature = "conformance")]
pub mod conformance;

#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "openai")]
pub mod openai;
#[cfg(feature = "system1")]
pub mod system1;
#[cfg(any(feature = "gemini", feature = "openai"))]
pub(crate) mod waiting;

pub use crate::endpoint::{Dialect, Endpoint};
#[cfg(feature = "gemini")]
pub use crate::gemini::Gemini;
#[cfg(feature = "openai")]
pub use crate::openai::OpenAiCompatible;
#[cfg(feature = "system1")]
pub use crate::system1::Jev;

/// Whether an error means an account is out of free requests for the day, rather than having hit
/// a momentary upstream limit.
///
/// note: the two are the same HTTP status and the difference matters to whoever is deciding what
/// to do next. The first is worth skipping over - it will still be true in a minute - and the
/// second is worth waiting out. A provider here already refuses to sit through a `Retry-After`
/// longer than a minute for the same reason; this is for a caller reading the error afterwards
/// and deciding whether the rest of a run is worth attempting.
pub fn out_of_quota(error: &str) -> bool {
    error.contains("per-day") || error.contains("daily")
}

/// Reads a server's complaint that a request was longer than the model takes, where it is one,
/// against what the caller already knows the model takes.
///
/// note: the wordings differ per vendor and the arithmetic does not, so this reads the numbers
/// rather than the sentence, and it reads exactly one of them: the request, which is the largest
/// token count a complaint of this kind can name. The limit is *not* taken from the prose, and a
/// sentence that reads `you requested about 92674 tokens (92174 of text input, 500 in the output)`
/// against a model that takes 65536 is why - the second-largest number there is neither of the
/// two, and the difference is what somebody would be told to prune.
///
/// note: `limit` is what says the reading is a sane one, since a refusal for length names a
/// request larger than the model and nothing else in the sentence is. Without one, a message
/// naming a single number is left alone: which number it is - the request, or what the model
/// takes - is the whole of what this is for, and reading it the wrong way round calibrates a
/// counter *down* on its way to a refusal.
///
/// note: and the sentence has to *name* that limit, which is the one thing that says it is
/// counting in the same units as the session reading it. An aggregator normalises token counts
/// into its own tokenizer and enforces its own window, but the model behind it refuses in its
/// native one - and both refusals arrive from the same address, for the same model id, in the
/// same afternoon. One said `maximum context length is 65536 tokens ... you requested about
/// 71311` against a request this counter had put at 71231, which is a correction worth having;
/// the other named neither that limit nor anything near that request, because it was the
/// upstream's own count of the same bytes. Reading the second would tell somebody to prune tens
/// of thousands of tokens that were never there, and would teach the counter a scale belonging
/// to a tokenizer this session is not held to. An unread refusal is still the server's own
/// sentence, which names the problem in words.
pub fn too_long(said: &str, limit: Option<u64>) -> Option<TooLong> {
    /// What names this kind of refusal, whoever phrased it.
    const COMPLAINTS: [&str; 5] = [
        "context length",
        "context_length",
        "too long",
        "token count",
        "tokens allowed",
    ];
    /// What a token count can be, either side of it. The upper bound is what keeps an identifier
    /// out of the reading.
    const PLAUSIBLE: RangeInclusive<u64> = 256..=100_000_000;
    /// Beyond which a number is something other than a request that overshot.
    const OVERSHOT_BY: u64 = 1_000;

    let lowered = said.to_lowercase();
    if !COMPLAINTS.iter().any(|which| lowered.contains(which)) {
        return None;
    }

    let mut numbers: Vec<u64> = said
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|run| run.parse().ok())
        .filter(|number| PLAUSIBLE.contains(number))
        .collect();

    numbers.sort_unstable();
    numbers.dedup();

    let tokens = numbers.pop()?;
    let read = match limit {
        Some(limit) => {
            numbers.contains(&limit) && tokens > limit && tokens < limit.saturating_mul(OVERSHOT_BY)
        }
        // nothing to check it against, so the sentence has to name a second figure for the
        // largest to be the request rather than the only number in it
        None => !numbers.is_empty(),
    };

    read.then(|| TooLong {
        overrun: Overrun { tokens, limit },
        said: said.to_owned(),
    })
}

/// The error a refused request becomes, reading a complaint about its length as one.
///
/// note: every dialect here ends at the same line - a sentence from a server, boxed and returned -
/// and this is that line, so that the one refusal a session can *act* on arrives as [`TooLong`]
/// rather than as prose somebody has to read twice. The sentence survives either way: `TooLong`
/// prints what the server said and nothing else.
#[cfg(any(feature = "gemini", feature = "openai"))]
pub(crate) fn refused(said: String, limit: Option<usize>) -> BoxError {
    match too_long(&said, limit.map(|limit| limit as u64)) {
        Some(too_long) => too_long.into(),
        None => said.into(),
    }
}

/// Whether a listed identifier names the model being asked about, allowing for the decorations
/// listings put on them: Google's `models/` prefix, ollama's implicit `:latest` tag.
pub fn same_model(listed: &str, model: &str) -> bool {
    listed == model
        || listed.strip_prefix("models/") == Some(model)
        || listed.strip_suffix(":latest") == Some(model)
}

/// Whether an address is OpenRouter's. Takes a whole URL or a bare authority.
///
/// note: one rule, in one place, because three things in this workspace need it and each one is a
/// decision about somebody's credentials or about somebody's data. Whether to send the app headers
/// that put a program in a public ranking; which of the two services serving `jev` a question is
/// shaped for; and, in `kamchatka`, whether a session's own key may be spent on anything else.
/// Three copies of a host test is three chances for one of them to be read as saying what the
/// others say when it no longer does.
///
/// note: on the authority alone, so a path, a port and a regional subdomain all still count, and
/// `openrouter.ai.example.com` does not. That last one is the whole reason this is not a
/// `contains`.
pub fn is_openrouter(address: &str) -> bool {
    let authority = address
        .split_once("://")
        .map_or(address, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or_default();
    let host = authority.split(':').next().unwrap_or(authority);

    host == "openrouter.ai" || host.ends_with(".openrouter.ai")
}

/// Installs the cryptography `rustls` will use, and says nothing if it is already installed.
///
/// note: reqwest is built here with `rustls-no-provider`, so there is no default waiting behind
/// this - a client built without it fails at the first `https://` with "no process-level
/// CryptoProvider available". Every constructor in this crate calls it, so that a test or an
/// example that builds a provider and never goes near `main` is not the one that finds out. It is
/// public because a caller building its own [`reqwest::Client`] needs the same thing, and because
/// a program that installs a different provider wants to do so before any of this runs.
pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whose address it is, from either a whole URL or a bare authority.
    ///
    /// note: three things turn this into a decision - app attribution, which of two services a
    /// `jev` question is shaped for, and whether `kamchatka` may spend a session's key on advice -
    /// and the third is the reason the URL forms are here. `ranks_apps` only ever saw a bare host,
    /// so a scheme and a path went untested until something passed one.
    ///
    /// note: `openrouter.ai.example.com` is the case that decides the shape of the whole function.
    /// A `contains` or a suffix test over the raw address would match it, and the three callers
    /// would then name a program, shape a request and spend a key against a host that merely put
    /// somebody else's name in front of its own.
    #[test]
    fn an_address_is_openrouters_by_its_authority_and_nothing_else() {
        for theirs in [
            "openrouter.ai",
            "openrouter.ai:443",
            "api.openrouter.ai",
            "https://openrouter.ai/api/v1",
            "https://openrouter.ai/api/alpha",
            "https://openrouter.ai",
        ] {
            assert!(is_openrouter(theirs), "{theirs}");
        }

        for not in [
            "localhost:11434",
            "http://localhost:11434/v1",
            "https://generativelanguage.googleapis.com/v1beta",
            "https://api.typesafe.ai/v1",
            "openrouter.ai.example.com",
            "https://openrouter.ai.example.com/api/v1",
            "notopenrouter.ai",
            "",
        ] {
            assert!(!is_openrouter(not), "{not}");
        }
    }

    /// Every vendor writes the same complaint differently, and the arithmetic is the same in all
    /// of them.
    ///
    /// note: each of these is a sentence an endpoint sent. The first carries a status code, which
    /// is a number in the message and not a token count; the second carries three of them, of
    /// which the second-largest is neither the request nor the limit - which is why what is read
    /// out of the prose is the request alone.
    #[test]
    fn a_refusal_for_length_is_read_whoever_phrased_it() {
        let shapes = [
            (
                "400 Bad Request: Provider returned error - {\"error\":{\"message\":\"The \
                 request is 286315 tokens long and exceeds this model's context length of \
                 262144 tokens.\",\"code\":\"context_length_exceeded\"}}",
                262_144,
                286_315,
            ),
            (
                "400 Bad Request: This endpoint's maximum context length is 65536 tokens. \
                 However, you requested about 92674 tokens (92174 of text input, 500 in the \
                 output). Please reduce the length of either one.",
                65_536,
                92_674,
            ),
            (
                "This model's maximum context length is 8192 tokens. However, your messages \
                 resulted in 10000 tokens. Please reduce the length of the messages.",
                8_192,
                10_000,
            ),
            (
                "prompt is too long: 250000 tokens > 200000 maximum",
                200_000,
                250_000,
            ),
            (
                "The input token count (1050000) exceeds the maximum number of tokens allowed \
                 (1048576).",
                1_048_576,
                1_050_000,
            ),
            // the aggregator refusing in its own units, and the reading this is for: the counter
            // had put the same request at 71231, which is the correction arriving
            (
                "400 Bad Request: This endpoint's maximum context length is 65536 tokens. \
                 However, you requested about 71311 tokens (70014 of text input, 1297 of tool \
                 input). Please reduce the length of either one, or use the context-compression \
                 plugin to compress your prompt automatically.",
                65_536,
                71_311,
            ),
        ];

        for (said, limit, tokens) in shapes {
            let read = too_long(said, Some(limit)).unwrap_or_else(|| panic!("not read: {said}"));
            assert_eq!(
                read.overrun,
                Overrun {
                    tokens,
                    limit: Some(limit)
                },
                "read wrongly: {said}"
            );
            assert_eq!(read.to_string(), said, "the sentence survives the reading");

            // and the same sentence from an endpoint that never said what the model takes: the
            // request is still there to be read, and there is nothing to check it against
            assert_eq!(
                too_long(said, None).map(|read| read.overrun.tokens),
                Some(tokens),
                "not read without a limit: {said}"
            );
        }
    }

    /// What is not a refusal for length, and what is one but says too little to act on.
    #[test]
    fn a_refusal_that_cannot_be_read_is_left_as_a_sentence() {
        let nothing = [
            // not about length at all, and the second one has two numbers in it
            ("429 Too Many Requests: rate limit exceeded", Some(8_192)),
            (
                "401 Unauthorized: key 12345678 is not valid for model 4096",
                Some(8_192),
            ),
            // about length, and naming only what the model takes. With a limit to check it
            // against it fails the check; without one, a lone number could be either of the two
            // and reading it as the wrong one is worse than not reading it
            ("This model's maximum context length is 8192 tokens.", None),
            (
                "This model's maximum context length is 8192 tokens.",
                Some(8_192),
            ),
            // about length, and the only number in it that could be a count is an identifier
            (
                "context length exceeded on request 987654321 (limit 8192)",
                Some(8_192),
            ),
            // about length, and counted by somebody else: the same model id at the same address
            // as the fixture above, refused by the model behind the aggregator rather than by the
            // aggregator, in the model's own tokenizer. Every number here is true and none of
            // them is in the units this session is held to
            (
                "400 Bad Request: Provider returned error - {\"error\":{\"message\":\"This \
                 model's maximum context length is 131072 tokens. However, you requested 8192 \
                 output tokens and your prompt contains at least 122881 input tokens, for a \
                 total of at least 131073 tokens.\"}}",
                Some(65_536),
            ),
        ];

        for (said, limit) in nothing {
            assert_eq!(too_long(said, limit), None, "read anyway: {said}");
        }
    }
}
