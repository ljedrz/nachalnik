//! The app a request is made on behalf of, for an endpoint that keeps a ranking of them.
//!
//! note: its own module rather than part of one client, because unrelated clients send it: the
//! chat-completions dialect, Anthropic's, which OpenRouter also serves, and the System One client,
//! which is deliberately not a `Dialect`. A program that asks one service for a conversation and
//! for advice is one app to that service, and headers each client wrote for itself would be copies
//! of one rule about what is said about it - which is what [`crate::is_openrouter`] was
//! consolidated out of.

/// The app a request is being made on behalf of, for an endpoint that keeps a ranking of them.
///
/// note: the URL is an identifier rather than a link anybody follows - OpenRouter keeps the app's
/// page against it - so it wants to be the project's own address and to stay the same.
///
/// note: built once and handed to every client that should send it -
/// [`OpenAiCompatible::attributed_to`](crate::OpenAiCompatible::attributed_to),
/// [`Anthropic::attributed_to`](crate::Anthropic::attributed_to) and
/// [`system1::Client::attributed_to`](crate::system1::Client::attributed_to) - so that one switch
/// turning attribution off turns it off for all of them. Somebody who turned it off for their
/// conversation has not agreed to be named by a second client on the same account.
///
/// note: the [`Default`] names no app, and sends nothing at all. A category or a visibility with
/// no URL beside it describes nothing, because the page is built against the URL - which is the
/// API's own rule - so they are kept, and sent only once there is a URL to send them with.
///
/// note: `#[non_exhaustive]` because nothing outside this crate builds one field by field.
/// [`Attribution::new`] and the builders are the way one is made, and a header the ranking wants
/// next should not be a break.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Attribution {
    /// The app's own URL, which is what the ranking is kept against; empty for none.
    pub url: String,
    /// What to call it on the page.
    pub title: String,
    /// What kind of program it is, in the endpoint's own vocabulary; see
    /// [`Attribution::filed_under`].
    pub categories: Vec<String>,
    /// Whether the page these requests would create is kept off the public listings; see
    /// [`Attribution::unlisted`].
    pub unlisted: bool,
}

impl Attribution {
    /// Names the app, by its URL and what to call it.
    ///
    /// note: sent only to the endpoint that reads it. The headers name the program, never the
    /// person running it or what they asked - but a `HTTP-Referer` volunteered to whatever address
    /// the caller has pointed a client at is still something the person running it did not ask to
    /// send, and the address is a setting.
    pub fn new(url: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            title: title.into(),
            ..Self::default()
        }
    }

    /// Says what kind of program it is, in the categories the endpoint files apps under.
    ///
    /// note: what it is given, verbatim. OpenRouter documents two per request, ten in total, and
    /// its own list of names - and an unrecognised one is *dropped*, not refused: no error, no
    /// notice, a 200 like any other, so `cli_agent` for `cli-agent` is a typo that fails nothing
    /// and shows up only as an app filed under nothing. Neither the count nor the spelling is
    /// checked here, because a crate that guessed at somebody else's list would go stale the day
    /// it grew: [the attribution page](https://openrouter.ai/docs/app-attribution) has the names.
    ///
    /// note: they accumulate on the app rather than replace what it has, so this is not a way to
    /// correct one. Changing what an app is already filed under is a conversation with OpenRouter.
    #[must_use]
    pub fn filed_under<C: Into<String>>(mut self, categories: impl IntoIterator<Item = C>) -> Self {
        self.categories = categories.into_iter().map(Into::into).collect();
        self
    }

    /// Keeps the app page off the public rankings and out of the marketplace, for attribution kept
    /// as somebody's own telemetry rather than as a listing.
    ///
    /// note: this only ever reaches the request that *creates* the page. A URL that already has
    /// one keeps whatever visibility it has, in either direction - so a program with one fixed URL
    /// has a single request, once, in which this means anything, and nobody running it later can
    /// change that with a header. Which is also what makes it safe: a caller of somebody else's
    /// app cannot hide it.
    #[must_use]
    pub fn unlisted(mut self, unlisted: bool) -> Self {
        self.unlisted = unlisted;
        self
    }

    /// Adds the app headers to a request for `address`, if there is an app to name and the
    /// address is one that keeps a ranking of them.
    ///
    /// note: the address test is [`crate::is_openrouter`], the same one the request paths and
    /// `kamchatka`'s key rule use. An engine of one's own keeps no ranking of the apps calling it,
    /// so a client pointed at one has nothing to send and must not send it.
    pub(crate) fn sign(
        &self,
        request: reqwest::RequestBuilder,
        address: &str,
    ) -> reqwest::RequestBuilder {
        if self.url.is_empty() || !crate::is_openrouter(address) {
            return request;
        }
        // `X-OpenRouter-Title` is the current name; `X-Title` is the one it replaced and is still
        // accepted
        let request = request
            .header(reqwest::header::REFERER, &self.url)
            .header("X-OpenRouter-Title", &self.title);
        let request = match self.categories.is_empty() {
            true => request,
            false => request.header("X-OpenRouter-Categories", self.categories.join(",")),
        };
        // and nothing at all for the public case, which is the default and has no value of its
        // own: `hidden` or the absence of the header, there is no third thing to say
        match self.unlisted {
            true => request.header("X-OpenRouter-App-Visibility", "hidden"),
            false => request,
        }
    }
}
