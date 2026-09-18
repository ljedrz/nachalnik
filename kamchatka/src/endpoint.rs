//! Where this program's requests go, which key pays for them, and what it says about itself.
//!
//! note: the providers themselves are [`nachalnik_providers`], a crate of their own, and they
//! read no environment at all - where to send a request and what to measure it against are the
//! caller's to decide. This is that caller: four variables, documented in `--help` and in the
//! README, and one place that turns them into a provider. A dialect each, because the address
//! they default to is the one thing the two do not share.
//!
//! note: named for what it settles rather than for what it hands back. It was `provider`, which
//! was the truth while the dialects were files in this crate and stopped being it the day they
//! became [`nachalnik_providers`] - and a module called `provider` next to a crate of providers
//! reads as the place one is implemented rather than the place one is addressed.

use std::{env, sync::Arc};

use nachalnik::BoxError;
use nachalnik_providers::{Gemini, OpenAiCompatible, gemini::DEFAULT_BASE_URL};

/// The project these requests are made on behalf of, where the endpoint keeps a ranking of apps.
///
/// note: this crate's own directory rather than the repository root, because the identifier
/// should name the thing making the requests. `nachalnik` is the runtime and makes none - it owns
/// no client and no provider - and a page called `kamchatka` that resolved to the library would
/// leave a visitor to find the program inside it. It also keeps the identifier free for anything
/// else in this workspace that ever calls out.
///
/// note: `HEAD` rather than a branch name. This is a primary key: OpenRouter builds the app's
/// page against it, so changing it later does not rename the app, it starts a new one and orphans
/// what the old one had. `tree/master/...` would have been that change waiting to happen the day
/// the default branch is renamed.
///
/// note: what goes out is the name of the program and what kind of program it is - not the key,
/// not the model, not a word of what was asked - and it goes only to OpenRouter.
/// `KAMCHATKA_NO_ATTRIBUTION` turns it off, because a program that names its user's tooling to a
/// third party should say so and let them stop it.
const APP_URL: &str = "https://github.com/ljedrz/nachalnik/tree/HEAD/kamchatka";
const APP_TITLE: &str = "kamchatka";

/// Which of OpenRouter's categories the app page is filed under, which is what puts it in the
/// marketplace rather than only in the rankings.
///
/// note: two, because two per request is the documented limit, and these are the two that are
/// simply true - `cli-agent` is "terminal-based coding assistants" in their own words, and
/// `programming-app` is the wider one it also is. The rest of the coding group is somebody else:
/// this is not an editor plugin, it does not run in anybody's cloud, and it builds no apps.
///
/// note: nothing checks the spelling, here or in the provider, because OpenRouter drops a category
/// it does not recognise without an error. What catches a typo is opening the page the attribution
/// built and seeing what it says.
const APP_CATEGORIES: [&str; 2] = ["cli-agent", "programming-app"];

/// The context limit somebody set by hand, if they set one.
pub fn configured_limit() -> Option<usize> {
    env::var("KAMCHATKA_CONTEXT_LIMIT")
        .ok()
        .and_then(|limit| limit.parse().ok())
}

/// The API key, under whichever of the documented names it is set.
pub fn api_key() -> Result<String, BoxError> {
    env::var("KAMCHATKA_API_KEY")
        .or_else(|_| env::var("OPENROUTER_API_KEY"))
        .or_else(|_| env::var("OPENAI_API_KEY"))
        .map_err(|_| "set KAMCHATKA_API_KEY (or OPENROUTER_API_KEY / OPENAI_API_KEY)".into())
}

/// The endpoint to talk to; OpenRouter unless told otherwise.
pub fn base_url() -> String {
    env::var("KAMCHATKA_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_owned())
}

/// Where this session's own requests go, in whichever dialect it was asked to speak.
///
/// note: the dialect is half the answer and cannot be read out of the environment, which is why
/// this takes the flag rather than working it out. `KAMCHATKA_BASE_URL` is read by both, and the
/// two defaults behind it are different services - so a `--gemini` session that never set the
/// variable would otherwise report OpenRouter's address and Google's key.
///
/// note: what asks is the advisor, and what it is asking is whose key [`api_key`] just handed it.
/// A key is an OpenRouter key because it is being sent to OpenRouter, not because of the variable
/// it was read from: all three names are ordinary things to export, and `KAMCHATKA_API_KEY` is
/// whatever the endpoint this points at issued.
pub fn session_endpoint(gemini: bool) -> String {
    match gemini {
        true => gemini::base_url(),
        false => base_url(),
    }
}

/// Builds a provider from the environment, asking the endpoint what the model's limit is.
pub async fn connect(model: impl Into<String>) -> Result<Arc<OpenAiCompatible>, BoxError> {
    let mut provider =
        OpenAiCompatible::new(model, base_url(), api_key()?).with_context_limit(configured_limit());
    if env::var_os("KAMCHATKA_NO_ATTRIBUTION").is_none() {
        provider = provider
            .on_behalf_of(APP_URL, APP_TITLE)
            .filed_under(APP_CATEGORIES);
    }

    let provider = Arc::new(provider);
    provider.probe().await;

    Ok(provider)
}

/// The advisor: a second model, asked about tool calls rather than about turns.
///
/// note: its own key under its own name first, and this program's own only where there is no
/// second one and the session is already talking to OpenRouter. That second condition is the whole
/// of what makes the fallback safe, because a key is an OpenRouter key by virtue of being sent to
/// OpenRouter and not by virtue of the variable it was read from. A session pointed at ollama, at
/// Google with `--gemini`, or at any gateway of somebody's own holds a key that service issued,
/// and borrowing it here would hand a third party a credential that has no business with them -
/// which is the thing the old refusal to fall back was protecting, pointing the other way.
///
/// note: so `--advise` still asks for a dedicated key everywhere except the one configuration
/// where there is nothing to disclose: the requests already go to OpenRouter, and the advice goes
/// to OpenRouter. A dedicated key is checked first, so a session holding both pays TypeSafe.
///
/// note: what the fallback widens is who is told, which is the question `tools::advice` is about.
/// `--advise` already sends a tool's arguments off the machine; without a dedicated key they go to
/// OpenRouter as well as to the model behind it. Nothing in here is read unless `--advise` was
/// asked for - the flag is what decides whether they leave at all, and a key sitting in the
/// environment is not a decision to send them.
#[cfg(feature = "advise")]
pub mod advise {
    use std::fmt;

    use nachalnik_providers::{
        is_openrouter,
        typesafe::{self, Jev},
    };

    use super::*;

    /// Which account pays for the advice, and the key that proves it.
    ///
    /// note: a key and a service together, because the three settings have to agree and three
    /// variables read on their own would not. A TypeSafe key sent to OpenRouter is a 401,
    /// `jev-latest` asked of OpenRouter is a name it does not serve, and the address decides which
    /// of the two shapes the request even has. So the choice is made once - is there a key of
    /// TypeSafe's own - and the endpoint and the model follow from it.
    ///
    /// note: `#[non_exhaustive]`, which is what every public enum in this workspace carries. A
    /// third service serving the same model is exactly the kind of thing that happened once
    /// already, and it should be a patch rather than a break.
    #[non_exhaustive]
    pub enum Account {
        /// TypeSafe's own, under either of the documented names.
        TypeSafe(String),
        /// The key that already pays for the conversation, which pays for this too. A session that
        /// was not given a second key is not thereby a session that cannot have an advisor.
        OpenRouter(String),
    }

    /// Which account, and never the key.
    ///
    /// note: written out rather than derived, because the field is a credential. A derived `Debug`
    /// prints every field, so the first panic message, `assert_eq!` or log line that ever formatted
    /// one of these would put somebody's key where they did not put it - and a type whose whole
    /// purpose is deciding where a key may go should not be the thing that spills it.
    impl fmt::Debug for Account {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let service = match self {
                Self::TypeSafe(_) => "TypeSafe",
                Self::OpenRouter(_) => "OpenRouter",
            };

            write!(f, "Account::{service}(<key>)")
        }
    }

    impl Account {
        /// The key itself.
        pub fn api_key(&self) -> &str {
            match self {
                Self::TypeSafe(key) | Self::OpenRouter(key) => key,
            }
        }

        /// The endpoint to talk to; the one this account is with unless told otherwise.
        ///
        /// note: `KAMCHATKA_TYPESAFE_BASE_URL` moves the address and does not move the account.
        /// It is for a proxy in front of one of the two, and somebody pointing it at the *other*
        /// service is setting the model by hand as well - which is the same bargain the variable
        /// made before there were two.
        pub fn base_url(&self) -> String {
            env::var("KAMCHATKA_TYPESAFE_BASE_URL").unwrap_or_else(|_| {
                match self {
                    Self::TypeSafe(_) => typesafe::DEFAULT_BASE_URL,
                    Self::OpenRouter(_) => typesafe::OPENROUTER_BASE_URL,
                }
                .to_owned()
            })
        }

        /// Which model answers; the name the service it is with knows it by.
        ///
        /// note: the two do not call it the same thing. TypeSafe resolves `jev-latest` to whatever
        /// version is current; OpenRouter lists the versions it serves and has no moving name
        /// among them, so what goes there names one version.
        pub fn model(&self) -> String {
            env::var("KAMCHATKA_TYPESAFE_MODEL").unwrap_or_else(|_| {
                match self {
                    Self::TypeSafe(_) => typesafe::DEFAULT_MODEL,
                    Self::OpenRouter(_) => typesafe::OPENROUTER_MODEL,
                }
                .to_owned()
            })
        }
    }

    /// Whose key is available to pay for it, given where this session's own requests go.
    ///
    /// note: `session_endpoint` is not where the advisor's questions will go - it is where the
    /// *conversation* goes, which is the only thing that says whose key [`api_key`] just handed
    /// over. It is a parameter rather than something read here so that a caller cannot get it by
    /// accident: the answer decides whether somebody's credential is sent to a third party.
    pub fn account(session_endpoint: &str) -> Result<Account, BoxError> {
        chosen(
            env::var("KAMCHATKA_TYPESAFE_API_KEY")
                .or_else(|_| env::var("TYPESAFE_API_KEY"))
                .ok(),
            api_key().ok(),
            session_endpoint,
        )
    }

    /// Which of the two keys may pay, and whether either may.
    ///
    /// note: split out from [`account`] so the rule can be checked without the environment, the
    /// way `tools::advice::advised` is split out of `evaluate` - what is left above is three
    /// `env::var` calls and no decision. The rule is the one thing here that can leak a
    /// credential, so it is the one thing that wants a test with no key in it.
    fn chosen(
        dedicated: Option<String>,
        own: Option<String>,
        session_endpoint: &str,
    ) -> Result<Account, BoxError> {
        if let Some(key) = dedicated {
            return Ok(Account::TypeSafe(key));
        }

        if !is_openrouter(session_endpoint) {
            return Err(format!(
                "--advise needs a key: set KAMCHATKA_TYPESAFE_API_KEY (or TYPESAFE_API_KEY). This \
                 session talks to {session_endpoint}, so its own key is not OpenRouter's to borrow"
            )
            .into());
        }

        own.map(Account::OpenRouter).ok_or_else(|| {
            "--advise needs a key: set KAMCHATKA_TYPESAFE_API_KEY (or TYPESAFE_API_KEY) for \
             TypeSafe's own API, or KAMCHATKA_API_KEY to ask the same model through OpenRouter"
                .into()
        })
    }

    /// Builds the advisor from the environment, checking that the model it names is served.
    ///
    /// note: the listing is asked for here rather than on the first refusal, for the reason
    /// `Gemini::probe` is called at startup: a model name that is not served comes back a 400,
    /// and the moment to find that out is before a session is running rather than the first time
    /// a permission question depends on it. OpenRouter publishes no listing for this model, so a
    /// session paying through it gets no such warning - which is why the identifier this program
    /// sends there is a constant rather than something a person types.
    pub async fn connect(session_endpoint: &str) -> Result<Arc<Jev>, BoxError> {
        let account = account(session_endpoint)?;
        let jev = Arc::new(Jev::new(
            account.model(),
            account.base_url(),
            account.api_key(),
        ));
        jev.probe().await;

        Ok(jev)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The rule that decides whether somebody's credential leaves for a service that did not
        /// issue it.
        ///
        /// note: the most important test in this file, and the one the fallback needed before it
        /// was written. A key is an OpenRouter key because it is being *sent* to OpenRouter, not
        /// because of the variable it was read from - `KAMCHATKA_API_KEY` is whatever the endpoint
        /// it points at issued, and every address below is one this program documents somebody
        /// pointing it at.
        #[test]
        fn a_session_key_is_only_borrowed_where_it_was_already_going() {
            let dedicated = || Some("apikey_typesafe".to_owned());
            let own = || Some("sk-the-session-key".to_owned());

            // a dedicated key pays wherever the session is pointed, and is the only thing that
            // reaches TypeSafe at all
            for anywhere in ["https://openrouter.ai/api/v1", "http://localhost:11434/v1"] {
                let account = chosen(dedicated(), own(), anywhere).expect("a dedicated key pays");
                assert!(matches!(&account, Account::TypeSafe(_)), "{anywhere}");
                assert_eq!(account.api_key(), "apikey_typesafe");
            }

            // without one, the session's own key pays where it is already being sent
            let borrowed = chosen(None, own(), "https://openrouter.ai/api/v1")
                .expect("an OpenRouter session may spend its own key at OpenRouter");
            assert!(matches!(&borrowed, Account::OpenRouter(_)));
            assert_eq!(borrowed.api_key(), "sk-the-session-key");

            // and nowhere else. Each of these holds a key somebody other than OpenRouter issued,
            // and borrowing it would hand a third party a credential with no business with them -
            // the last one because a host is not a suffix match
            for elsewhere in [
                "http://localhost:11434/v1",
                "https://generativelanguage.googleapis.com/v1beta",
                "https://api.openai.com/v1",
                "https://openrouter.ai.example.com/api/v1",
            ] {
                let refused = chosen(None, own(), elsewhere)
                    .expect_err("a key that is not OpenRouter's is not spent there");
                let said = refused.to_string();
                assert!(said.contains("KAMCHATKA_TYPESAFE_API_KEY"), "{said}");
                // and it names the address it refused over, since the alternative is somebody
                // reading "needs a key" while holding one
                assert!(said.contains(elsewhere), "{said}");
            }

            // a session with no key at all is refused whatever it is pointed at
            assert!(chosen(None, None, "https://openrouter.ai/api/v1").is_err());
            assert!(chosen(None, None, "http://localhost:11434/v1").is_err());
        }
    }
}

/// The same four variables, pointed at Google's own API.
pub mod gemini {
    use super::*;

    /// The endpoint to talk to; Google's own unless told otherwise.
    pub fn base_url() -> String {
        env::var("KAMCHATKA_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned())
    }

    /// Builds a provider from the environment, asking the endpoint what the model's limit is.
    ///
    /// note: no attribution. It is OpenRouter that keeps a ranking of the apps calling it, and
    /// there is nothing to tell Google that it has asked for.
    pub async fn connect(model: impl Into<String>) -> Result<Arc<Gemini>, BoxError> {
        let provider = Arc::new(
            Gemini::new(model, base_url(), api_key()?).with_context_limit(configured_limit()),
        );
        provider.probe().await;

        Ok(provider)
    }
}
