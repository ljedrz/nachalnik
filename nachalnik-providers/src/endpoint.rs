//! The half of a model's service the person driving it asks about, rather than the kernel.

use nachalnik::{LinearProjector, Provider, async_trait};
use serde_json::{Number, Value};

/// The half of a model's service its user drives, not the kernel's.
///
/// note: [`Provider`] is the kernel's half: one method, with no opinion on where the answer comes
/// from. Where requests go, what the endpoint serves, which model is asked and what the last retry
/// was about belong to whoever built the client (a `/model` command, a status line, a run comparing
/// endpoints), whatever model is behind them. This is this crate's trait, not the runtime's, which
/// has no network.
///
/// note: not a supertrait of [`Provider`]: a System One [`Client`](crate::system1::Client) has an
/// address, a key, a model, a listing and a usage report but no turn to drive, and requiring one
/// would shut it out or hand the kernel a turn made out of probabilities. The turn-driving half is
/// [`Dialect`], which carries the `Provider` bound.
#[async_trait]
pub trait Endpoint: Send + Sync {
    /// Where the requests are going.
    fn endpoint(&self) -> String;

    /// Which model is being asked.
    fn model(&self) -> String;

    /// Just the authority of [`Endpoint::endpoint`] - `openrouter.ai`, `localhost:11434` - for the
    /// status line, which has no room for the rest.
    ///
    /// note: hand-parsed, not through a URL crate, because the whole use is a string to draw.
    /// Anything not recognised as a URL is handed back as it came: a status line showing nothing is
    /// worse than one showing something odd.
    fn host(&self) -> String {
        let endpoint = self.endpoint();
        let after_scheme = endpoint
            .split_once("://")
            .map_or(endpoint.as_str(), |(_, rest)| rest);

        // Split on `?` and `#` too, not just `/`: a `?key=` would otherwise be drawn on the status
        // line of an endpoint with no path. Also strip `user:password@` before the host: it is a
        // credential
        let authority = after_scheme
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        match authority.rsplit_once('@') {
            Some((_, host)) if !host.is_empty() => host.to_owned(),
            // no host to show, so the address it was not found in, without the credential
            Some((credentials, _)) => endpoint.replacen(&format!("{credentials}@"), "", 1),
            None if !authority.is_empty() => authority.to_owned(),
            None => endpoint,
        }
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
/// note: the trait a client holds for a session. Both halves in one bound, so a single `Arc`
/// answers the kernel and the status line, letting a program hold a model without knowing the wire
/// format behind it and swap one for the other during a session.
///
/// note: the two methods live here rather than on [`Endpoint`] because both are questions about a
/// turn, and a model that does not produce one has no answer to either. `projection` is about the
/// shape of a message on the wire; `lists_every_parameter` is about
/// [`nachalnik::ModelInfo::parameters`], which arrives through `Provider`.
pub trait Dialect: Endpoint + Provider {
    /// Whether [`nachalnik::ModelInfo::parameters`] lists everything the model takes, or only the
    /// part this endpoint publishes.
    ///
    /// note: it decides what may be said about a parameter not on the list. An exhaustive list
    /// settles it: the parameter is sent and ignored, which is what `/params` exists to say. A
    /// partial list settles nothing: `reasoning_effort` is absent from `mercury-2.5`'s list yet is
    /// read, and a bad value is a 400, so calling it ignored would invent a restriction from a list
    /// that never claimed to be complete.
    ///
    /// note: `true` by default, since one list of everything is the ordinary case; an endpoint that
    /// knows better says so.
    fn lists_every_parameter(&self) -> bool {
        true
    }

    /// What the endpoint publishes about one of [`nachalnik::ModelInfo::parameters`] beyond its
    /// name.
    ///
    /// note: read from the listing and from nothing else. No endpoint here publishes a parameter's
    /// type or its range, and a table of them kept in this crate would be somebody's documentation
    /// as of the day it was copied, said of a model it may not describe. A default is a value, so
    /// its type is in it.
    ///
    /// note: defaults to nothing, which is what an endpoint that publishes nothing has said.
    fn published(&self, parameter: &str) -> Published {
        let _ = parameter;
        Published::default()
    }

    /// How many times a request for a turn is sent while the server says it is busy (`429`, `503`,
    /// or a timed-out connection), including the first attempt. Four unless set, and never fewer
    /// than one.
    ///
    /// note: four suits someone watching, who would rather be told than kept waiting; a run nobody
    /// watches can ask for more, as can a limit shared by several sessions. The waits double from
    /// two seconds and stop growing at a minute, so ten tries take about five minutes. A server
    /// asking to be left longer than a minute is not waited for, however many tries remain, since
    /// that is a quota spent rather than a server busy.
    ///
    /// note: does nothing by default, for a dialect that never sends a request again.
    fn set_tries(&self, tries: usize) {
        let _ = tries;
    }

    /// How long a stream may say nothing - before its first byte or between two - before the
    /// request is given up on. Two and a half minutes unless set.
    ///
    /// note: a free endpoint under load can hold a request in its queue longer than that before the
    /// first byte and still answer it normally. Giving up on it there means a model that never
    /// answers, though it would if waited for.
    ///
    /// note: a client with its own whole-request timeout - see `OpenAiCompatible::client_with` -
    /// cuts off a long queue followed by a long answer first, as a connection error. Give such a
    /// client the patience plus the time an answer takes. The clients the providers build for
    /// themselves have no such timeout.
    ///
    /// note: defaults to doing nothing, for a dialect that does not stream.
    fn set_patience(&self, patience: std::time::Duration) {
        let _ = patience;
    }

    /// The projection this dialect can carry.
    ///
    /// note: here beside `to_wire`, which must honour it, because decided in two places they drift.
    /// The budget is counted over the projector's messages, so a projector handing over something
    /// the wire format then drops charges the person for bytes that never leave the process, for as
    /// long as those messages are in the context.
    fn projection(&self) -> LinearProjector {
        LinearProjector {
            // note: `to_wire` does not put an assistant turn's thinking on the wire, and cannot:
            // most endpoints speaking this dialect reject a message carrying a field they do not
            // know, and there is no agreed name for that one. Projecting it would only mean paying
            // for it. The turn keeps its reasoning either way: it is in the record, on the context
            // tab, and can be excluded like everything else.
            send_reasoning: false,
            ..Default::default()
        }
    }
}

/// What an endpoint publishes about one parameter beyond its name; see [`Dialect::published`].
///
/// note: `#[non_exhaustive]`, so what a listing says about a parameter can grow. Build one with
/// [`Published::default`] and the `with_` methods.
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

    /// A dialect that says nothing about its projection leaves a turn's thinking out of it.
    #[test]
    fn a_dialect_that_says_nothing_projects_no_thinking() {
        use nachalnik::{BoxError, DeltaSink, ModelInfo, ModelRequest, ModelResponse, async_trait};

        use crate::{Dialect, Endpoint};

        /// A dialect that answers nothing and leaves everything it may to the defaults.
        struct Plain;

        #[async_trait]
        impl nachalnik::Provider for Plain {
            fn info(&self) -> ModelInfo {
                ModelInfo::new("plain", "m")
            }

            async fn respond(
                &self,
                _request: ModelRequest,
                _deltas: DeltaSink,
            ) -> Result<ModelResponse, BoxError> {
                unreachable!("nothing is asked")
            }
        }

        #[async_trait]
        impl Endpoint for Plain {
            fn endpoint(&self) -> String {
                String::new()
            }

            fn model(&self) -> String {
                "m".to_owned()
            }

            async fn models(&self) -> Vec<String> {
                Vec::new()
            }

            async fn set_model(&self, _model: String) {}

            async fn set_endpoint(&self, _url: String, _model: Option<String>) {}

            fn take_notice(&self) -> Option<String> {
                None
            }
        }

        impl Dialect for Plain {}

        assert!(!Plain.projection().send_reasoning);
    }
}
