//! Providers for the [`nachalnik`] agent runtime: somebody else's HTTP API, read carefully.
//!
//! The runtime ships no provider and never will - it has no network in it, and a kernel that
//! opened a socket would be a kernel with an opinion about who you talk to. That leaves every
//! adopter writing the same thousand lines of streamed HTTP before they can ask a model anything,
//! which is what this crate is here to stop.
//!
//! Two dialects, each behind a feature:
//!
//! - [`openai`] - `POST /chat/completions`, `choices[].delta`, tool calls assembled from
//!   fragments. What OpenRouter, ollama, vLLM, LM Studio, Together and most of the rest speak.
//! - [`gemini`] - Google's own `generateContent`: `candidates[].content.parts`, whole calls, and
//!   ordered `thought` parts, which is the dialect [`nachalnik::Content::Blocks`] was built for.
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

mod endpoint;

#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "openai")]
pub mod openai;
#[cfg(any(feature = "gemini", feature = "openai"))]
pub(crate) mod waiting;

pub use crate::endpoint::Endpoint;
#[cfg(feature = "gemini")]
pub use crate::gemini::Gemini;
#[cfg(feature = "openai")]
pub use crate::openai::OpenAiCompatible;

/// Whether a listed identifier names the model being asked about, allowing for the decorations
/// listings put on them: Google's `models/` prefix, ollama's implicit `:latest` tag.
pub fn same_model(listed: &str, model: &str) -> bool {
    listed == model
        || listed.strip_prefix("models/") == Some(model)
        || listed.strip_suffix(":latest") == Some(model)
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
