//! The half of a model's service the person driving it asks about, rather than the kernel.

use nachalnik::{LinearProjector, Provider, async_trait};
use serde_json::{Number, Value};

/// The half of a model's service its user drives, rather than the kernel.
///
/// note: [`Provider`] is the kernel's half, and it is one method: the kernel asks for an answer
/// and has no opinion about where the answer comes from. Where the requests go, what the endpoint
/// serves, which model is being asked and what the last retry was about belong to whoever built
/// the client - a `/model` command, a status line, a run that compares two endpoints - and they
/// are the same questions whichever model is behind them. So they are a trait of this crate's own
/// rather than something the runtime was made to carry: the runtime has no network in it and no
/// business knowing that one exists.
///
/// note: [`Provider`] is deliberately *not* a supertrait, because a model can answer every
/// question below and none of the questions a turn is made of. A System One
/// [`Client`](crate::system1::Client) has an address, a key, a model identifier, a listing and a
/// usage report, and it generates no text and calls no tools, so there is no turn for it to drive.
/// Requiring one of everything in here would mean either shutting it out of the crate or handing
/// the kernel an assistant turn manufactured out of probabilities. The turn-driving half is
/// [`Dialect`], and it is the one that carries the `Provider` bound.
#[async_trait]
pub trait Endpoint: Send + Sync {
    /// Where the requests are going.
    fn endpoint(&self) -> String;

    /// Which model is being asked.
    fn model(&self) -> String;

    /// Just the authority of [`Endpoint::endpoint`] - `openrouter.ai`, `localhost:11434` - for
    /// the status line, which has no room for the rest of it.
    ///
    /// note: hand-parsed rather than through a URL crate, because the whole use of it is a string
    /// to draw. Anything this does not recognise as a URL is handed back as it came, since a
    /// status line showing nothing would be worse than one showing something odd.
    fn host(&self) -> String {
        let endpoint = self.endpoint();
        let after_scheme = endpoint
            .split_once("://")
            .map_or(endpoint.as_str(), |(_, rest)| rest);

        // the query and the fragment too, and not only the path: a key passed as `?key=` would
        // otherwise be drawn on the status line of an endpoint given with no path - and for the
        // same reason not a `user:password@` before the host, which is a credential too
        after_scheme
            .split(['/', '?', '#'])
            .next()
            .map(|authority| {
                authority
                    .rsplit_once('@')
                    .map_or(authority, |(_, host)| host)
            })
            .filter(|host| !host.is_empty())
            .unwrap_or(&endpoint)
            .to_owned()
    }

    /// What this endpoint serves, which is what [`Endpoint::set_model`] takes.
    async fn models(&self) -> Vec<String>;

    /// Switches models.
    async fn set_model(&self, model: String);

    /// Switches the address the requests go to, and optionally the model with it.
    async fn set_endpoint(&self, url: String, model: Option<String>);

    /// Takes whatever the client last wanted to say for itself, if anything.
    fn take_notice(&self) -> Option<String>;
}

/// An [`Endpoint`] whose model answers in turns: the wire format a conversation is carried in.
///
/// note: this is the trait a client of this crate holds when what it wants is a session. Both
/// halves in one bound, so a single `Arc` answers the kernel and the status line, which is what
/// lets a program hold a model without knowing which wire format is behind it and swap one for
/// the other while a session is running.
///
/// note: the two methods below live here rather than on [`Endpoint`] because both are questions
/// about a *turn*, and a model that does not produce one has no answer to either. `projection`
/// is about the shape of a message on the wire, and `lists_every_parameter` is about
/// [`nachalnik::ModelInfo::parameters`], which arrives through `Provider`.
pub trait Dialect: Endpoint + Provider {
    /// Whether [`nachalnik::ModelInfo::parameters`] is the whole of what the model takes, or only
    /// the part of it this endpoint publishes.
    ///
    /// note: it decides what may be said about a parameter that is *not* on the list, and the two
    /// answers are different claims. An exhaustive list settles it - the parameter is sent, the
    /// model does not take it, and it is ignored, which is the thing `/params` exists to say. A
    /// list of the sampling knobs alone settles nothing: `reasoning_effort` is absent from
    /// `mercury-2.5`'s and is read all the same, validated hard enough that a bad value comes back
    /// a 400. Reporting that one as ignored would be inventing a restriction out of a list that
    /// never claimed to be complete, which is the failure this crate takes seriously everywhere
    /// else it reads somebody's metadata.
    ///
    /// note: defaulted to `true` because a dialect that publishes one list of everything is the
    /// ordinary case. An endpoint that knows better says so.
    fn lists_every_parameter(&self) -> bool {
        true
    }

    /// What the endpoint publishes about one of [`nachalnik::ModelInfo::parameters`] beyond its
    /// name, where it publishes anything.
    ///
    /// note: read from the listing and from nothing else. No endpoint here publishes a
    /// parameter's type or its range, and a table of them kept in this crate would be somebody's
    /// documentation as of the day it was copied, said of a model it may not describe. A default
    /// is a value, so its type is in it.
    ///
    /// note: defaulted to nothing, which is what an endpoint that publishes nothing has said.
    fn published(&self, parameter: &str) -> Published {
        let _ = parameter;
        Published::default()
    }

    /// The projection this dialect can carry.
    ///
    /// note: it is answered here, beside the `to_wire` that has to honour it, because the two drift
    /// apart when they are decided in different places. The budget is counted over the messages the
    /// projector produced - which is what makes it the size of the request rather than the size
    /// of the context - so a projector that hands over something the wire format then drops does
    /// not merely waste the effort. It charges the person for bytes that never leave the process,
    /// and goes on doing it for as long as those messages are in the context.
    fn projection(&self) -> LinearProjector {
        LinearProjector {
            // note: `to_wire` does not put an assistant turn's thinking on the wire, and cannot:
            // most endpoints speaking this dialect reject a message carrying a field they do not
            // know, and there is no agreed name for that one. Projecting it would only mean
            // paying for it. The turn keeps its reasoning either way: it is in the record, on the
            // context tab, and prunable like everything else.
            send_reasoning: false,
            ..Default::default()
        }
    }
}

/// What an endpoint publishes about one parameter beyond its name; see [`Dialect::published`].
///
/// note: `#[non_exhaustive]`, so that what a listing says about a parameter can grow. Build one
/// with [`Published::default`] and the `with_` methods.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Published {
    /// The value the endpoint publishes as the model's default.
    pub default: Option<Value>,
    /// The largest value the endpoint publishes the model taking.
    pub maximum: Option<Number>,
}

impl Published {
    /// The same, with the published default.
    pub fn with_default(mut self, default: impl Into<Option<Value>>) -> Self {
        self.default = default.into();
        self
    }

    /// The same, with the published maximum.
    pub fn with_maximum(mut self, maximum: impl Into<Option<Number>>) -> Self {
        self.maximum = maximum.into();
        self
    }
}

#[cfg(test)]
mod tests {
    /// A dialect that says nothing about its list claims the list is everything the model takes.
    ///
    /// note: asked of `Anthropic`, which leaves the answer to the default. A client builds a
    /// different sentence on each answer, and a default of `false` would have it call every
    /// parameter off the list unchecked rather than ignored.
    #[cfg(feature = "anthropic")]
    #[test]
    fn a_dialect_that_says_nothing_claims_its_list_is_all_of_it() {
        use crate::{Anthropic, Dialect as _};

        let provider = Anthropic::new("m", "http://127.0.0.1:1", "no key");
        assert!(provider.lists_every_parameter());
    }
}
