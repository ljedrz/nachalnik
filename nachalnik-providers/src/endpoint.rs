//! The half of a provider the person driving it asks about, rather than the kernel.

use nachalnik::{LinearProjector, Provider, async_trait};

/// The half of a provider its user drives, rather than the kernel.
///
/// note: [`Provider`] is the kernel's half, and it is one method: the kernel asks for an answer
/// and has no opinion about where the answer comes from. Where the requests go, what the endpoint
/// serves, which model is being asked and what the last retry was about belong to whoever built
/// the provider - a `/model` command, a status line, a run that compares two endpoints - and they
/// are the same questions whichever dialect is in use. So they are a trait of this crate's own
/// rather than something the runtime was made to carry: the runtime has no network in it and no
/// business knowing that one exists.
///
/// note: [`Provider`] is a supertrait, so one `Arc` answers both. That is what lets a program
/// hold a provider without knowing which wire format is behind it, and what lets it swap one for
/// the other while a session is running.
#[async_trait]
pub trait Endpoint: Provider {
    /// Where the requests are going.
    fn endpoint(&self) -> String;

    /// Which model is being asked.
    fn model(&self) -> String;

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
    /// ordinary case and the one this was written against. An endpoint that knows better says so.
    fn lists_every_parameter(&self) -> bool {
        true
    }

    /// The projection this dialect can carry.
    ///
    /// note: it is answered here, beside the `to_wire` that has to honour it, because the two
    /// were decided in different places and drifted. The budget is counted over the messages the
    /// projector produced - which is what makes it the size of the request rather than the size
    /// of the context - so a projector that hands over something the wire format then drops does
    /// not merely waste the effort. It charges the person for bytes that never leave the process,
    /// and goes on doing it for as long as those messages are in the context.
    fn projection(&self) -> LinearProjector {
        LinearProjector {
            // note: `to_wire` has never put an assistant turn's thinking on the wire, and cannot:
            // most endpoints speaking this dialect reject a message carrying a field they do not
            // know, and there is no agreed name for that one. So sending it was never on offer -
            // only paying for it was. The turn keeps its reasoning either way: it is in the
            // record, on the context tab, and prunable like everything else.
            send_reasoning: false,
            ..Default::default()
        }
    }

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

        after_scheme
            .split('/')
            .next()
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

    /// Takes whatever the provider last wanted to say for itself, if anything.
    fn take_notice(&self) -> Option<String>;
}
