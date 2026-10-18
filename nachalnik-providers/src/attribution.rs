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
/// note: the URL is an identifier, not a link anyone follows (OpenRouter keeps the app's page
/// against it), so it should be the project's own address and stay the same.
///
/// note: built once and handed to every client that sends it -
/// [`OpenAiCompatible::attributed_to`](crate::OpenAiCompatible::attributed_to),
/// [`Anthropic::attributed_to`](crate::Anthropic::attributed_to) and
/// [`system1::Client::attributed_to`](crate::system1::Client::attributed_to) - so one switch turns
/// attribution off for all of them: somebody who turned it off for their conversation has not
/// agreed to be named by a second client on the same account.
///
/// note: the [`Default`] names no app and sends nothing. A category or a visibility without a URL
/// describes nothing, since the page is built against the URL (the API's own rule), so they are
/// kept and sent once there is a URL to send them with.
///
/// note: `#[non_exhaustive]` because nothing outside this crate builds one field by field;
/// [`Attribution::new`] and the builders make one, and a header the ranking wants next should not
/// be a break.
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
    /// note: sent verbatim. OpenRouter documents two per request, ten in total, and its own list of
    /// names, and drops an unknown one silently with a 200, so `cli_agent` for `cli-agent` fails
    /// nothing and only files the app under nothing. Neither the count nor the spelling is checked
    /// here, since a copy of somebody else's list would go stale; [the attribution
    /// page](https://openrouter.ai/docs/app-attribution) has the names.
    ///
    /// note: they accumulate on the app rather than replace what it has, so this cannot correct
    /// one; changing an app's categories is a conversation with OpenRouter.
    #[must_use]
    pub fn filed_under<C: Into<String>>(mut self, categories: impl IntoIterator<Item = C>) -> Self {
        self.categories = categories.into_iter().map(Into::into).collect();
        self
    }

    /// Keeps the app page off the public rankings and out of the marketplace, for attribution kept
    /// as somebody's own telemetry rather than as a listing.
    ///
    /// note: this only reaches the request that creates the page; a URL that already has one keeps
    /// its visibility either way. So a program with one fixed URL has a single request, once, in
    /// which this means anything, and nobody running it later can change that, which is also why a
    /// caller of somebody else's app cannot hide it.
    #[must_use]
    pub fn unlisted(mut self, unlisted: bool) -> Self {
        self.unlisted = unlisted;
        self
    }

    /// Adds the app headers to a request for `address` if an app is named and the address is
    /// OpenRouter.
    ///
    /// note: the address test is [`crate::is_openrouter`], the same one the request paths and
    /// `kamchatka`'s key rule use. An engine you run yourself keeps no ranking of the apps calling
    /// it, so a client pointed at one has nothing to send and must not send it.
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
        // The public case is the default and sends no header. The only values are `hidden` and no
        // header
        match self.unlisted {
            true => request.header("X-OpenRouter-App-Visibility", "hidden"),
            false => request,
        }
    }
}
