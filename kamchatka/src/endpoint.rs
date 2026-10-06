//! Where this program's requests go, which key pays for them, and what it says about itself.
//!
//! note: the providers themselves are [`nachalnik_providers`], a crate of their own, and they
//! read no environment at all - where to send a request and what to measure it against are the
//! caller's to decide. This is that caller: four variables, documented in `--help` and in the
//! README, and one place that turns them into a provider. A dialect each, because the address
//! they default to is the one thing the two do not share.
//!
//! note: named for what it settles rather than for what it hands back. A module called
//! `provider` next to [`nachalnik_providers`] would read as the place one is implemented rather
//! than the place one is addressed.

use std::{env, sync::Arc};

use nachalnik::BoxError;
use nachalnik_providers::{
    Anthropic, Gemini, OpenAiCompatible, anthropic::DEFAULT_BASE_URL as ANTHROPIC_BASE_URL,
    gemini::DEFAULT_BASE_URL, is_openrouter,
};

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
/// what the old one had. `tree/master/...` would be that change, waiting for the day the default
/// branch is renamed.
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
///
/// note: a value that is not a number, or not a positive one, is [`None`] here rather than a
/// refusal: `checked_limit` is what a program reads at startup, and this is here for a caller
/// that wants the figure and has no startup to refuse one in.
pub fn configured_limit() -> Option<usize> {
    checked_limit().ok().flatten()
}

/// The context limit somebody set by hand, or what is wrong with what they set.
///
/// note: refused rather than dropped, and at startup. A value that does not parse used to read as
/// no limit at all, so a mistyped figure, or one written where a shell variable's name was wanted,
/// left a session measuring against whatever the endpoint advertises, with nothing on the screen
/// saying the setting was not in force. A `0` was worse: it is a limit, and a request is refused
/// against it on the first turn, with the budget reading `the limit: 0, which the next request
/// would fill 0.0% of`.
///
/// note: the sentence names the variable and shows what it held, because the environment is the one
/// place a figure can be wrong with no file to open and nothing on the screen to point at. `0` is
/// refused rather than read as "no limit", which is what the other `0`s in this program mean - a
/// setting that says `0` here and "no limit" everywhere else is a trap, and this one is the only
/// place the value is spent rather than merely held.
pub(crate) fn checked_limit() -> Result<Option<usize>, BoxError> {
    let limit = match env::var("KAMCHATKA_CONTEXT_LIMIT") {
        Ok(limit) => limit,
        Err(_) => return Ok(None),
    };

    match limit.parse::<usize>() {
        // `usize` is what it is parsed as and `0` is in range, so the two are told apart here
        // rather than by asking the parse
        Ok(0) => Err(
            "KAMCHATKA_CONTEXT_LIMIT is `0`, which is not a number of tokens: it is how many the \
             model holds, and the first request would already be over it. Leave it unset to \
             measure against what the endpoint says"
                .into(),
        ),
        Ok(limit) => Ok(Some(limit)),
        Err(_) => Err(format!(
            "KAMCHATKA_CONTEXT_LIMIT is `{limit}`, which is not a number of tokens: a whole number \
             above 0, as in `128000`"
        )
        .into()),
    }
}

/// The API key, under whichever of the documented names it is set.
///
/// note: an error where none is, although a session no longer needs one everywhere - see
/// `key_for`, which is what decides. The signature is the one 0.18.0 published, kept for a patch
/// release; the error is the sentence a session refused for want of a key used to be given.
///
/// note: nothing here sends what this returns any more. It reads the three names in order whatever
/// the address, which is how an OpenRouter key reached OpenAI; `key_for` is the rule now.
pub fn api_key() -> Result<String, BoxError> {
    env::var("KAMCHATKA_API_KEY")
        .or_else(|_| env::var("OPENROUTER_API_KEY"))
        .or_else(|_| env::var("OPENAI_API_KEY"))
        .map_err(|_| "set KAMCHATKA_API_KEY (or OPENROUTER_API_KEY / OPENAI_API_KEY)".into())
}

/// The key requests to `url` go with: the one set, or none at all - which the providers send as
/// no header - unless `url` is a service that checks one.
///
/// note: a key is not a prerequisite, because a model served on the machine in front of you wants
/// none: ollama, llama.cpp and vLLM check nothing unless they were started with a key of their own,
/// and a session pointed at one used to be refused at startup until somebody exported a key it
/// would never read. Refused still for OpenRouter and Google, which are the two defaults and refuse
/// every request without one - there the line at startup is worth more than a 401 on the first
/// turn. Anywhere else the endpoint is the one that knows, and its 401 says so.
///
/// note: whose key it is decides where it may go. `KAMCHATKA_API_KEY` is the one somebody set for
/// this program, so it goes wherever they pointed it; `OPENROUTER_API_KEY` and `OPENAI_API_KEY`
/// are named for who issued them, and go there and nowhere else. Read as fallbacks for any
/// address, a session pointed at `api.openai.com` with both exported sent OpenAI the OpenRouter
/// key - and one at Google's, Anthropic's or a local server's sent either to whoever was there.
fn key_for(url: &str) -> Result<String, BoxError> {
    keyed(url, |name| env::var(name).ok())
}

/// [`key_for`], with the variables read through `read`: the rule, apart from the environment, so
/// that it can be checked with no key in it - it is the one thing here that decides whether a
/// credential leaves for somebody who did not issue it.
fn keyed(url: &str, read: impl Fn(&str) -> Option<String>) -> Result<String, BoxError> {
    let named = match () {
        _ if is_openrouter(url) => Some("OPENROUTER_API_KEY"),
        _ if is_openai(url) => Some("OPENAI_API_KEY"),
        _ => None,
    };
    let key = read("KAMCHATKA_API_KEY").or_else(|| named.and_then(&read));

    match key {
        Some(key) => Ok(key),
        None if checks_a_key(url) => Err(format!(
            "set {}: {} refuses a request without one",
            match named {
                Some(name) => format!("{name} (or KAMCHATKA_API_KEY)"),
                None => "KAMCHATKA_API_KEY".to_owned(),
            },
            shown(url)
        )
        .into()),
        None => Ok(String::new()),
    }
}

/// Where OpenAI's own API is, which is the one address `OPENAI_API_KEY` is sent to.
const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Whether `url` is OpenAI's own API.
fn is_openai(url: &str) -> bool {
    host(url) == host(OPENAI_BASE_URL)
}

/// Whether `url` is one of the services known to refuse every request that comes without a key.
///
/// note: on the host alone, as [`is_openrouter`] reads one, so Google's `/v1beta/openai` counts
/// as well as its native `/v1beta`, whatever the port and whoever is named before an `@`.
fn checks_a_key(url: &str) -> bool {
    is_openrouter(url)
        || is_openai(url)
        || host(url) == host(DEFAULT_BASE_URL)
        || host(url) == host(ANTHROPIC_BASE_URL)
}

/// Whether `url` is Anthropic's own API, which is the one address `ANTHROPIC_API_KEY` is sent to.
fn is_anthropic(url: &str) -> bool {
    host(url) == host(ANTHROPIC_BASE_URL)
}

/// The host of an address, lowercased, without the port, the path or whoever is named before an
/// `@`.
fn host(url: &str) -> String {
    let authority = shown(url);
    let authority = authority
        .split_once("://")
        .map_or(authority.as_str(), |(_, rest)| rest);
    let authority = authority.split('/').next().unwrap_or_default();
    authority
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Every variable this program reads a key from, none of which a command it runs for the model
/// inherits.
///
/// note: all of them rather than the one that won. A command the model writes is the model's
/// hands, and what `printenv` prints goes into the context, the request and the record - and with
/// the network granted, anywhere. The confinement holds a command to its directory and says
/// nothing about what the command was handed when it started, so a key in its environment is the
/// one secret it could always reach.
///
/// note: the shell only. An MCP server is a program the person chose, running unconfined with
/// everything they can read, and a server that calls an API of its own may read one of these names
/// for its own key; taking them from it would be a nuisance and not a boundary.
pub const KEYS: [&str; 5] = [
    "KAMCHATKA_API_KEY",
    "OPENROUTER_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "KAMCHATKA_SYSTEM1_API_KEY",
];

/// Whether requests can be sent to `url` at all: an `http://` or `https://` scheme, a host, and
/// nothing after the path.
///
/// note: read by hand rather than parsed, like `Endpoint::host`, because what it catches is the
/// ways a person gets an address wrong - a word that is not one, and a scheme left off, which
/// makes `localhost:11434` a URL whose scheme is `localhost`. Either left to the provider fails on
/// the next request, as a transport error, after the switch has already been announced. Anything
/// subtler still reaches the provider, and its error says why.
///
/// note: and a query string or a fragment, which no request can be built on. Every path is
/// appended to the base as it stands, so `…/v1?token=…` was asked for `…/v1?token=…/chat/
/// completions` - a request that never worked, and whose failure wrote the token into the record,
/// since the transport's error names the URL it could not reach.
pub fn is_an_address(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };

    matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
        && !rest.contains(['?', '#'])
        && rest.split('/').next().is_some_and(|host| !host.is_empty())
}

/// An address as this program says it back: without a `user:password@` before the host.
///
/// note: the request still goes with it, and a person may well have meant it there; what is left
/// out is its being said. Every line naming an address goes to a screen, down a pipe into a log
/// somebody pastes, and to every client of a served session, and the record already writes it
/// down without one - so the status line, `/model`, `/endpoint` and `/seams` say it as the record
/// does.
pub fn shown(url: &str) -> String {
    let (scheme, rest) = url
        .split_once("://")
        .map_or(("", url), |(scheme, rest)| (scheme, rest));
    let (authority, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);

    match scheme {
        "" => format!("{host}{path}"),
        scheme => format!("{scheme}://{host}{path}"),
    }
}

/// The base URL, if it is one; refused at startup rather than on the first request.
///
/// note: what the variable held is said back with the variable's name, because the failure it
/// saves is a `builder error` on the first turn that names neither.
fn addressed(url: String) -> Result<String, BoxError> {
    match is_an_address(&url) {
        true => Ok(url),
        false => Err(format!(
            "KAMCHATKA_BASE_URL is `{url}`, which is not an address: it wants http:// or https:// \
             and a host, and no `?` or `#`, since every path is added after it"
        )
        .into()),
    }
}

/// The endpoint to talk to; OpenRouter unless told otherwise.
pub fn base_url() -> String {
    env::var("KAMCHATKA_BASE_URL").unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_owned())
}

/// The wire format a session speaks, which is what decides the address it goes to by default.
///
/// note: `#[non_exhaustive]`, which is what every public enum in this workspace carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Wire {
    /// OpenAI chat-completions, which OpenRouter and most local servers speak.
    OpenAi,
    /// Google's own `generateContent`, with `--gemini`.
    Gemini,
    /// Anthropic's Messages API, with `--anthropic`.
    Anthropic,
    /// OpenAI's Responses API, with `--responses`: the same address and key as chat completions.
    Responses,
}

/// Where this session's own requests go, in whichever dialect it was asked to speak.
///
/// note: the dialect is half the answer and cannot be read out of the environment, which is why
/// this takes the flag rather than working it out. `KAMCHATKA_BASE_URL` is read by both, and the
/// defaults behind it are different services - so a `--gemini` session that never set the
/// variable would otherwise report OpenRouter's address and Google's key.
///
/// note: the advisor is what asks, and the question is whose key [`api_key`] just handed it. A key
/// is an OpenRouter key because it is being sent to OpenRouter, not because of the variable
/// it was read from: all three names are ordinary things to export, and `KAMCHATKA_API_KEY` is
/// whatever the endpoint this points at issued.
pub fn session_endpoint(wire: Wire) -> String {
    match wire {
        Wire::OpenAi | Wire::Responses => base_url(),
        Wire::Gemini => gemini::base_url(),
        Wire::Anthropic => anthropic::base_url(),
    }
}

/// Builds a provider from the environment, asking the endpoint what the model's limit is.
///
/// note: the model is optional, and having none is how a session started without `-m` begins.
/// Everything that does not depend on which model it is - the address, the key, the attribution,
/// the listing `/models` reads - is settled here all the same, so that picking one afterwards is a
/// command rather than a restart. The probe is the one thing skipped: there is no model for it to
/// ask a limit about, and `set_model` runs it the moment there is.
pub async fn connect(model: Option<&str>) -> Result<Arc<OpenAiCompatible>, BoxError> {
    connected(model, false).await
}

/// The same provider, asking OpenAI's Responses API: everything about where the requests go is
/// the same, and only what they say differs.
pub mod responses {
    use super::*;

    /// Builds the provider [`super::connect`] builds, in Responses mode.
    pub async fn connect(model: Option<&str>) -> Result<Arc<OpenAiCompatible>, BoxError> {
        connected(model, true).await
    }
}

/// [`connect`], in whichever of the two modes.
async fn connected(
    model: Option<&str>,
    responses: bool,
) -> Result<Arc<OpenAiCompatible>, BoxError> {
    let mut provider = OpenAiCompatible::new(
        model.unwrap_or_default(),
        addressed(base_url())?,
        key_for(&base_url())?,
    )
    .with_context_limit(checked_limit()?)
    .responses(responses);
    if env::var_os("KAMCHATKA_NO_ATTRIBUTION").is_none() {
        provider = provider
            .on_behalf_of(APP_URL, APP_TITLE)
            .filed_under(APP_CATEGORIES);
    }

    let provider = Arc::new(provider);
    if model.is_some() {
        provider.probe().await;
    }

    Ok(provider)
}

/// The advisor: a System One model, asked about tool calls rather than about turns - see
/// `tools::advice` for what the program asks it, and `system1_assisted_compaction` for another use.
///
/// note: any of the ones OpenRouter serves, named by `KAMCHATKA_SYSTEM1_MODEL` and with no default,
/// so that no vendor's is chosen for anybody. A provider OpenRouter does not list is reached
/// through its bring-your-own-key; `KAMCHATKA_SYSTEM1_BASE_URL` is for an engine of one's own.
///
/// note: its own key under its own name first, and otherwise an OpenRouter key this session already
/// has - its own where the session talks to OpenRouter, or `OPENROUTER_API_KEY` - and only where
/// the advice goes to OpenRouter. That condition is the whole of what makes the fallback safe,
/// because a key is an OpenRouter key by virtue of being sent to OpenRouter and not by virtue of
/// the variable it was read from. A session pointed at ollama, at Google with `--gemini`, or at
/// any gateway of somebody's own holds a key that service issued, and borrowing it here would hand
/// a third party a credential that has no business with them.
///
/// note: what the fallback widens is who is told, which is the question `tools::advice` is about.
/// `--advise` sends a command's arguments off the machine, to OpenRouter as well as to the model
/// behind it, unless `KAMCHATKA_SYSTEM1_BASE_URL` points at an engine of one's own. Nothing in here
/// is read unless `--advise` was asked for - the flag is what decides whether they leave at all, and a key sitting in the
/// environment is not a decision to send them.
#[cfg(feature = "advise")]
pub mod advise {
    use std::fmt;

    use nachalnik_providers::{
        is_openrouter,
        system1::{self, Client, SystemOne},
    };

    use super::*;

    /// Where the decision models are listed, for a refusal that has to say where to pick one.
    pub const LISTING: &str = "https://openrouter.ai/api/v1/models?output_modalities=decisions";

    /// Which key pays for the advice.
    ///
    /// note: named for *whose key it is*, because that is what carries a rule about where it may
    /// go. A key held for this goes wherever `KAMCHATKA_SYSTEM1_BASE_URL` points it, since the
    /// person who set both decided that; a borrowed one is an OpenRouter key and goes to OpenRouter
    /// and nowhere else.
    ///
    /// note: `#[non_exhaustive]`, which is what every public enum in this workspace carries.
    #[non_exhaustive]
    pub enum Account {
        /// `KAMCHATKA_SYSTEM1_API_KEY`, held for the advisor itself.
        Dedicated(String),
        /// An OpenRouter key this session already has: its own, where the conversation goes to
        /// OpenRouter too, or `OPENROUTER_API_KEY`. A session that was not given a second key is
        /// not thereby a session that cannot have an advisor.
        Borrowed(String),
        /// None at all, for an advisor pointed somewhere other than OpenRouter and given no key
        /// of its own: an engine on this machine checks none, and no OpenRouter key is borrowed
        /// for anywhere else.
        Keyless,
    }

    /// Which account, and never the key.
    ///
    /// note: written out rather than derived, because the field is a credential. A derived `Debug`
    /// prints every field, so the first panic message, `assert_eq!` or log line that ever formatted
    /// one of these would put somebody's key where they did not put it - and a type whose whole
    /// purpose is deciding where a key may go should not be the thing that spills it.
    impl fmt::Debug for Account {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let held = match self {
                Self::Dedicated(_) => "Dedicated",
                Self::Borrowed(_) => "Borrowed",
                Self::Keyless => return f.write_str("Account::Keyless"),
            };

            write!(f, "Account::{held}(<key>)")
        }
    }

    impl Account {
        /// The key itself.
        pub fn api_key(&self) -> &str {
            match self {
                Self::Dedicated(key) | Self::Borrowed(key) => key,
                // which the client sends as no header at all
                Self::Keyless => "",
            }
        }
    }

    /// Where the advisor's questions go: OpenRouter, unless `KAMCHATKA_SYSTEM1_BASE_URL` names
    /// something else.
    ///
    /// note: what something else is for is an engine of one's own - `laya-serve` on this machine
    /// answers the same route. A provider that sells a System One model and is not on OpenRouter's
    /// list is reached through OpenRouter's bring-your-own-key, so it needs nothing here.
    pub fn base_url() -> String {
        env::var("KAMCHATKA_SYSTEM1_BASE_URL")
            .unwrap_or_else(|_| system1::DEFAULT_BASE_URL.to_owned())
    }

    /// Which model answers: `KAMCHATKA_SYSTEM1_MODEL`, and nothing if it is not set.
    ///
    /// note: no default, because a default would be one vendor's model chosen for everybody who
    /// asked for advice. A session asked for with `--advise` and no model is refused at startup,
    /// with the listing to pick one from.
    pub fn model() -> Result<String, BoxError> {
        env::var("KAMCHATKA_SYSTEM1_MODEL").map_err(|_| {
            format!(
                "--advise needs a model: set KAMCHATKA_SYSTEM1_MODEL to one of the System One \
                 models at {LISTING}"
            )
            .into()
        })
    }

    /// Whose key is available to pay for it, given where this session's own requests go.
    ///
    /// note: `session_endpoint` is not where the advisor's questions will go - it is where the
    /// *conversation* goes, which is the only thing that says whose key [`api_key`] just handed
    /// over. It is a parameter rather than something read here so that a caller cannot get it by
    /// accident: the answer decides whether somebody's credential is sent to a third party.
    pub fn account(session_endpoint: &str) -> Result<Account, BoxError> {
        chosen(
            env::var("KAMCHATKA_SYSTEM1_API_KEY").ok(),
            key_for(session_endpoint).ok().filter(|key| !key.is_empty()),
            env::var("OPENROUTER_API_KEY").ok(),
            session_endpoint,
            &base_url(),
        )
    }

    /// Which key may pay, and whether any may.
    ///
    /// note: split out from [`account`] so the rule can be checked without the environment - what
    /// is left above reads the environment and decides nothing. The rule is the one thing here
    /// that can leak a credential, so it is the one thing that wants a test with no key in it.
    ///
    /// note: a borrowed key is borrowed only where *both* ends are OpenRouter's. `own` is whatever
    /// the conversation's endpoint issued, so it is OpenRouter's only where the conversation goes
    /// there; and an advisor pointed at a local engine would otherwise be handed an OpenRouter
    /// credential it has no business with. `OPENROUTER_API_KEY` is OpenRouter's by its name.
    fn chosen(
        dedicated: Option<String>,
        own: Option<String>,
        openrouter: Option<String>,
        session_endpoint: &str,
        advisor_endpoint: &str,
    ) -> Result<Account, BoxError> {
        if let Some(key) = dedicated {
            return Ok(Account::Dedicated(key));
        }

        // note: no key rather than a refusal, for the reason a session needs none - see
        // `key_for`. An engine that does check one answers the first question with a 401
        if !is_openrouter(advisor_endpoint) {
            return Ok(Account::Keyless);
        }

        own.filter(|_| is_openrouter(session_endpoint))
            .or(openrouter)
            .map(Account::Borrowed)
            .ok_or_else(|| {
                format!(
                    "--advise needs an OpenRouter key: set OPENROUTER_API_KEY, or \
                     KAMCHATKA_SYSTEM1_API_KEY. This session talks to {session_endpoint}, so its \
                     own key is not OpenRouter's to borrow"
                )
                .into()
            })
    }

    /// Builds the advisor from the environment, checking that the model it names is served.
    ///
    /// note: the listing is asked for here rather than on the first refusal, for the reason
    /// `Gemini::probe` is called at startup: a model name that is not served comes back a 400,
    /// and the moment to find that out is before a session is running rather than the first time
    /// a permission question depends on it.
    pub async fn connect(session_endpoint: &str) -> Result<Arc<dyn SystemOne>, BoxError> {
        let model = model()?;
        let account = account(session_endpoint)?;
        let engine = Arc::new(Client::new(model, base_url(), account.api_key()));
        engine.probe().await;

        Ok(engine)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The rule that decides whether somebody's credential leaves for a service that did not
        /// issue it.
        ///
        /// note: a key is an OpenRouter key because it is being *sent* to OpenRouter, not because
        /// of the variable it was read from - `KAMCHATKA_API_KEY` is whatever the endpoint it
        /// points at issued, and every address below is one this program documents somebody
        /// pointing it at.
        #[test]
        fn a_key_is_only_borrowed_where_it_is_openrouters() {
            let openrouter = "https://openrouter.ai/api/v1";
            let dedicated = || Some("sk-held-for-advice".to_owned());
            let own = || Some("sk-the-session-key".to_owned());
            let named = || Some("sk-or-by-name".to_owned());

            // a dedicated key pays wherever the session and the advisor are pointed
            for (session, advisor) in [
                (openrouter, openrouter),
                ("http://localhost:11434/v1", openrouter),
                (openrouter, "http://127.0.0.1:8000/v1"),
            ] {
                let account = chosen(dedicated(), own(), named(), session, advisor)
                    .expect("a dedicated key pays");
                assert!(
                    matches!(&account, Account::Dedicated(_)),
                    "{session} {advisor}"
                );
                assert_eq!(account.api_key(), "sk-held-for-advice");
            }

            // without one, the session's own key pays where it is OpenRouter's, ahead of the one
            // by name
            let borrowed = chosen(None, own(), named(), openrouter, openrouter)
                .expect("an OpenRouter session may spend its own key at OpenRouter");
            assert!(matches!(&borrowed, Account::Borrowed(_)));
            assert_eq!(borrowed.api_key(), "sk-the-session-key");

            // and where it is not - each of these holds a key somebody else issued, the last one
            // because a host is not a suffix match - `OPENROUTER_API_KEY` pays, or nothing does
            for elsewhere in [
                "http://localhost:11434/v1",
                "https://generativelanguage.googleapis.com/v1beta",
                "https://api.openai.com/v1",
                "https://openrouter.ai.example.com/api/v1",
            ] {
                let named_pays = chosen(None, own(), named(), elsewhere, openrouter)
                    .expect("an OpenRouter key by name pays at OpenRouter");
                assert_eq!(named_pays.api_key(), "sk-or-by-name", "{elsewhere}");

                let refused = chosen(None, own(), None, elsewhere, openrouter)
                    .expect_err("a key that is not OpenRouter's is not spent there");
                let said = refused.to_string();
                assert!(said.contains("OPENROUTER_API_KEY"), "{said}");
                // and it names the address it refused over, since the alternative is somebody
                // reading "needs a key" while holding one
                assert!(said.contains(elsewhere), "{said}");
            }

            // and an advisor pointed somewhere other than OpenRouter is handed no OpenRouter key
            // at all, the session's or the named one: it goes with none
            for advisor in [
                "http://127.0.0.1:8000/v1",
                "https://openrouter.ai.example.com/api/v1",
            ] {
                let keyless = chosen(None, own(), named(), openrouter, advisor)
                    .expect("an engine of one's own may check no key");
                assert!(matches!(&keyless, Account::Keyless), "{advisor}");
                assert_eq!(
                    keyless.api_key(),
                    "",
                    "a borrowed key is not sent past OpenRouter"
                );
            }

            // a session with no key at all is refused where the advice goes to OpenRouter, which
            // checks one, whatever the session is pointed at
            assert!(chosen(None, None, None, openrouter, openrouter).is_err());
            assert!(chosen(None, None, None, "http://localhost:11434/v1", openrouter).is_err());
        }

        /// Which account is which, said out loud, and the key never is.
        ///
        /// note: an account is decided in an `assert_eq!`, a panic message or a log line sooner
        /// or later, and the field is a credential - so a `Debug` that named it would put somebody's
        /// key wherever such a line ends up. What a reader needs is which of the two accounts was
        /// chosen, and which is the answer either word carries.
        #[test]
        fn an_account_says_which_it_is_and_never_says_its_key() {
            let held = [
                (
                    Account::Dedicated("apikey_typesafe".to_owned()),
                    "Dedicated",
                ),
                (
                    Account::Borrowed("sk-the-session-key".to_owned()),
                    "Borrowed",
                ),
            ];

            for (account, which) in held {
                let said = format!("{account:?}");
                assert_eq!(said, format!("Account::{which}(<key>)"), "{said}");
                // and the whole of it, in the shapes a key is written in
                for key in ["apikey_typesafe", "sk-the-session-key"] {
                    assert!(!said.contains(key), "the key is in `{said}`");
                }
            }
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
    ///
    /// note: the model is optional for the reason it is above, and here the probe is not merely
    /// pointless without one: it asks for `{base}/models/{model}` by name, so with nothing to put
    /// there it would be asking the endpoint about a model called nothing.
    pub async fn connect(model: Option<&str>) -> Result<Arc<Gemini>, BoxError> {
        let provider = Arc::new(
            Gemini::new(
                model.unwrap_or_default(),
                addressed(base_url())?,
                key_for(&base_url())?,
            )
            .with_context_limit(checked_limit()?),
        );
        if model.is_some() {
            provider.probe().await;
        }

        Ok(provider)
    }
}

/// The same four variables, pointed at Anthropic's own API - and `ANTHROPIC_API_KEY`, where they
/// are.
pub mod anthropic {
    use super::*;

    /// The endpoint to talk to; Anthropic's own unless told otherwise.
    ///
    /// note: OpenRouter answers this dialect too, at `https://openrouter.ai/api/v1`, and that is
    /// how a model there is asked in its own dialect rather than through the OpenAI one.
    pub fn base_url() -> String {
        env::var("KAMCHATKA_BASE_URL").unwrap_or_else(|_| ANTHROPIC_BASE_URL.to_owned())
    }

    /// The key: `ANTHROPIC_API_KEY` where the requests go to Anthropic, and otherwise the one
    /// every other dialect reads.
    ///
    /// note: read only for Anthropic's own address, because a key belongs to whoever issued it,
    /// and one somebody exported for Anthropic has no business being sent to OpenRouter.
    fn key(url: &str) -> Result<String, BoxError> {
        match is_anthropic(url) {
            true => match env::var("ANTHROPIC_API_KEY") {
                Ok(key) => Ok(key),
                Err(_) => key_for(url).map_err(|_| {
                    format!(
                        "set ANTHROPIC_API_KEY (or KAMCHATKA_API_KEY): {} refuses a request \
                         without one",
                        shown(url)
                    )
                    .into()
                }),
            },
            false => key_for(url),
        }
    }

    /// Builds a provider from the environment, asking the endpoint what the model takes.
    ///
    /// note: no attribution, and the model optional, for the reasons the Gemini one gives.
    pub async fn connect(model: Option<&str>) -> Result<Arc<Anthropic>, BoxError> {
        let url = base_url();
        let provider = Arc::new(
            Anthropic::new(
                model.unwrap_or_default(),
                addressed(url.clone())?,
                key(&url)?,
            )
            .with_context_limit(checked_limit()?),
        );
        if model.is_some() {
            provider.probe().await;
        }

        Ok(provider)
    }
}

#[cfg(test)]
mod tests {

    /// A key named for who issued it goes to them and nowhere else; the one set for this program
    /// goes wherever it was pointed.
    ///
    /// note: every address here is one this program documents somebody pointing it at, and every
    /// key is exported at once - which is the case that leaked: an OpenRouter key went to OpenAI,
    /// and either went to Google, Anthropic or a local server whenever `KAMCHATKA_API_KEY` was not
    /// set.
    #[test]
    fn a_key_goes_only_to_whoever_issued_it() {
        let every = |name: &str| match name {
            "OPENROUTER_API_KEY" => Some("sk-or".to_owned()),
            "OPENAI_API_KEY" => Some("sk-openai".to_owned()),
            _ => None,
        };
        for (url, sent) in [
            ("https://openrouter.ai/api/v1", Some("sk-or")),
            ("https://api.openai.com/v1", Some("sk-openai")),
            // the ones that refuse a request without a key, refused at startup rather than sent
            // somebody else's
            ("https://generativelanguage.googleapis.com/v1beta", None),
            ("https://api.anthropic.com/v1", None),
            // and the ones that take none, sent none
            ("http://localhost:11434/v1", Some("")),
            ("https://gateway.example.com/v1", Some("")),
        ] {
            let got = keyed(url, every);
            assert_eq!(got.as_deref().ok(), sent, "{url}");
        }

        // the program's own key is the person's choice for wherever they pointed it
        let own = |name: &str| (name == "KAMCHATKA_API_KEY").then(|| "sk-own".to_owned());
        for url in [
            "https://openrouter.ai/api/v1",
            "https://api.openai.com/v1",
            "https://api.anthropic.com/v1",
            "http://localhost:11434/v1",
        ] {
            assert_eq!(keyed(url, own).ok().as_deref(), Some("sk-own"), "{url}");
        }

        // and a refusal names the variable that address reads
        let none = |_: &str| None;
        let said = keyed("https://api.openai.com/v1", none)
            .unwrap_err()
            .to_string();
        assert!(
            said.starts_with("set OPENAI_API_KEY (or KAMCHATKA_API_KEY)"),
            "{said}"
        );
        let said = keyed("https://openrouter.ai/api/v1", none)
            .unwrap_err()
            .to_string();
        assert!(
            said.starts_with("set OPENROUTER_API_KEY (or KAMCHATKA_API_KEY)"),
            "{said}"
        );
        let said = keyed("https://generativelanguage.googleapis.com/v1beta", none)
            .unwrap_err()
            .to_string();
        assert!(said.starts_with("set KAMCHATKA_API_KEY:"), "{said}");
    }
    use super::*;

    /// A key is asked for only where the service is known to refuse every request without one.
    ///
    /// note: the two defaults, under the spellings somebody copies them in, and the addresses a
    /// model served on the machine is reached at - which are the reason a key is not a
    /// prerequisite any more.
    #[test]
    fn only_the_services_that_check_a_key_need_one() {
        for checks in [
            "https://openrouter.ai/api/v1",
            "https://eu.openrouter.ai/api/v1",
            DEFAULT_BASE_URL,
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "https://GenerativeLanguage.googleapis.com:443/v1beta",
            ANTHROPIC_BASE_URL,
            "https://api.anthropic.com:443/v1",
        ] {
            assert!(checks_a_key(checks), "{checks}");
        }

        for checks_none in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8080/v1",
            "http://gpu-box.lan:8000/v1",
            "https://generativelanguage.googleapis.com.example.com/v1beta",
            "https://openrouter.ai.example.com/api/v1",
        ] {
            assert!(!checks_a_key(checks_none), "{checks_none}");
        }
    }
}
