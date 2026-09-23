//! System One models: typed questions put to a state, answered with numbers. [`Jev`] is the one
//! this crate speaks to.
//!
//! note: the one thing in this crate that is not a [`Dialect`](crate::Dialect). It generates no
//! text, calls no tools and streams nothing, so there is no turn for it to drive and no
//! [`nachalnik::Provider`] for it to implement - a kernel never sees any of this. What it answers
//! is the question a program asks *around* a conversation: whether to run that command, which of
//! four branches this is, how bad the thing it just read is.
//!
//! note: named for the kind of model rather than for the company selling one. The three question
//! types are the category's and not this vendor's: `laya`, the open one, has the same three under
//! the same names.
//!
//! note: [`SystemOne`] is the seam, and there is a second implementation for it to fit. The open
//! engines ship as *libraries* rather than services, so the second implementation is a local
//! process - and this crate does not spawn processes, which is the line `nachalnik-mcp` exists on
//! the other side of. So the trait is here, [`Jev`] implements it, and the local one lives in
//! whichever crate is already spawning things. A caller holds `dyn SystemOne` and never learns
//! which it got.
//!
//! note: what a third *service* takes, as against a second engine, is an address and nothing
//! else. Anything answering a `state` and a map of typed questions at the path below works
//! through [`Jev`] with [`crate::Endpoint::set_endpoint`] and a model name, because an address
//! this does not recognise is read as keeping TypeSafe's paths - which is the shape a self-hosted
//! one has.
//!
//! note: three question types and they are asked together in one request. Each is evaluated on
//! its own against the same state, which is the reason to ask them that way rather than in one
//! bundled sentence: the answers do not interfere, and the round trip is paid for once.
//!
//! note: two services serve it, and the request body is the same at both. TypeSafe's own API takes
//! it at `/systemone`; OpenRouter resells it behind `/decisions`, on an `/api/alpha` path of its
//! own rather than the `/api/v1` the rest of that service lives on. Which one a client is talking
//! to is read off the address it was given, so a caller chooses by handing over a base URL and a
//! key that belong together - [`Jev::latest`] and [`Jev::through_openrouter`] are the pairs.
//!
//! ```no_run
//! # use nachalnik_providers::system1::{Jev, Question};
//! # async fn go() -> Result<(), nachalnik::BoxError> {
//! let jev = Jev::latest(std::env::var("TYPESAFE_API_KEY")?);
//! let answers = jev
//!     .ask(
//!         "rm -rf /",
//!         [
//!             ("destructive", Question::noul("Does this destroy data irreversibly?")),
//!             ("blast", Question::score("How far does it reach?", ["one file", "one project", "the machine"])),
//!         ],
//!     )
//!     .await?;
//!
//! if answers.noul("destructive").is_some_and(|it| it > 0.9) {
//!     // ...
//! }
//! # Ok(())
//! # }
//! ```

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use nachalnik::{BoxError, Usage, async_trait};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::{Endpoint, install_crypto, same_model, waiting::RETRIES};

/// Where `jev` lives.
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai/v1";

/// The model identifier the documentation tells a caller to use.
///
/// note: it resolves to a version on the way through - a request naming this one comes back
/// naming a version, such as `jev-1.13.0` - so [`Answers::model`] is what actually answered, and
/// is the one worth recording rather than what was asked for.
pub const DEFAULT_MODEL: &str = "jev-latest";

/// Where OpenRouter takes these, which is not where it takes everything else.
///
/// note: `/api/alpha`, not the `/api/v1` its chat endpoint is on. The two halves of that service
/// do not overlap in either direction: a decision sent to `/api/v1` is a 404, and this model sent
/// to `/chat/completions` is refused for being a decisions model.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/alpha";

/// And what the model is called there.
///
/// note: a version rather than a moving name, because there is no moving name to use. TypeSafe's
/// own API resolves `jev-latest`; OpenRouter lists the versions it serves, `typesafe/jev-latest`
/// is not one of them, and this is the identifier its own documentation uses. So it is a constant
/// somebody has to bump, and [`Answers::model`] is what says which version actually answered.
pub const OPENROUTER_MODEL: &str = "typesafe/jev-1.13";

/// Which of the two services serving `jev` an address belongs to.
///
/// note: the model is the same one and the request body is the same JSON, so what this decides is
/// only the paperwork around it: the path a question goes to, whether there is a listing to ask
/// for, and which envelope a refusal arrives in. Getting any of them from the wrong service is a
/// 404 or an unreadable error rather than a wrong answer.
///
/// note: read off the address rather than passed in beside it, for the reason
/// `openai::ranks_apps` is: the two cannot then disagree, and [`Endpoint::set_endpoint`] moving a
/// live client from one service to the other moves the path with it. What that costs is a gateway
/// standing in front of OpenRouter under somebody else's name: it is read as TypeSafe's own API
/// and asked for `/systemone`. Reading it the other way round would be wrong for a proxy of
/// TypeSafe, which keeps TypeSafe's paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    /// TypeSafe's own API.
    TypeSafe,
    /// OpenRouter, which resells it.
    OpenRouter,
}

impl Service {
    /// Which one an address belongs to.
    ///
    /// note: anything that is not OpenRouter's is read as TypeSafe's own API rather than refused,
    /// because that is the shape a self-hosted proxy of it has - a proxy keeps the upstream's
    /// paths.
    fn of(address: &str) -> Self {
        match crate::is_openrouter(address) {
            true => Self::OpenRouter,
            false => Self::TypeSafe,
        }
    }

    /// The path a question goes to, under the base URL.
    ///
    /// note: each service's own word for the same API, which is why this is not one name with two
    /// spellings. TypeSafe calls it System One; OpenRouter calls it Decisions.
    fn route(self) -> &'static str {
        match self {
            Self::TypeSafe => "systemone",
            Self::OpenRouter => "decisions",
        }
    }

    /// Whether there is a listing of what it serves to ask for.
    fn publishes_a_listing(self) -> bool {
        self == Self::TypeSafe
    }
}

/// The documented request body: a model, a state, and the questions put to it by name.
///
/// note: public, and the one place the body is written, because not every engine that answers it
/// is reached through a [`Jev`]. A local engine spoken to over a pipe takes the same body, and a
/// second copy of it is a second place for the shape to drift.
pub fn render(model: &str, state: &Value, questions: &[(String, Question)]) -> Value {
    json!({
        "model": model,
        "state": state,
        "questions": questions
            .iter()
            .map(|(name, question)| (name.clone(), question.to_wire()))
            .collect::<serde_json::Map<_, _>>(),
    })
}

/// How long the first retry waits, doubling from there.
const BACKOFF: Duration = Duration::from_millis(500);

/// How long one request may take before the transport gives up on it.
///
/// note: seconds rather than the minutes a reasoning model gets, because this model answers in
/// about one. A wait of ten minutes would only ever be a hang, and the place this is wired into is
/// a permission gate with somebody sitting in front of it.
const PATIENCE: Duration = Duration::from_secs(30);

/// One typed question to put to a state.
///
/// note: the three the model answers, and they are not interchangeable. A [`Question::Noul`] is a
/// claim that is more or less true; a [`Question::Choice`] is one of a closed set; a
/// [`Question::Score`] is a position on an ordered rubric. What decides between them is the shape
/// of the answer a program needs, and the documentation's own advice is to ask several small ones
/// rather than one that needs reasoning to unpack.
///
/// note: `#[non_exhaustive]`, with [`Answer`] beside it. Three is what the engines answer today
/// and not what a question can be - a fourth primitive is the upstream's to add, and this crate
/// exists to speak whatever it grows. What the attribute costs is an exhaustive match, and nothing
/// outside this crate needs one: a caller builds these through [`Question::noul`],
/// [`Question::choice`] and [`Question::score`], and reads the answers back through the accessors
/// on [`Answers`], which already answer `None` for a variant they were not asked about.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Question {
    /// A claim, answered with how true it is.
    Noul {
        /// What is being asked.
        instructions: Value,
        /// What it would mean for this to be true, where saying so helps.
        when_true: Option<String>,
        /// And what it would mean for it to be false.
        when_false: Option<String>,
    },
    /// A closed set, answered with one of them and the distribution over all of them.
    Choice {
        /// What is being asked.
        instructions: Value,
        /// The options, each optionally described.
        ///
        /// note: a list of pairs rather than a map, so that the payload carries them in the order
        /// they were written. It becomes a JSON object either way and the model is not promised
        /// an order; what this is for is the person reading [`Jev::render`] beside the code that
        /// built it.
        options: Vec<(String, Option<String>)>,
    },
    /// An ordered rubric, answered with a position on it that may fall between two levels.
    Score {
        /// What is being asked.
        instructions: Value,
        /// The levels, lowest first.
        levels: Vec<String>,
    },
}

impl Question {
    /// A claim to be weighed, with nothing said about either side of it.
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: instructions.into().into(),
            when_true: None,
            when_false: None,
        }
    }

    /// Says what the two sides of a [`Question::Noul`] would mean; ignored by the others.
    pub fn between(self, when_true: impl Into<String>, when_false: impl Into<String>) -> Self {
        match self {
            Self::Noul { instructions, .. } => Self::Noul {
                instructions,
                when_true: Some(when_true.into()),
                when_false: Some(when_false.into()),
            },
            other => other,
        }
    }

    /// A closed set of bare options.
    pub fn choice(
        instructions: impl Into<String>,
        options: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::Choice {
            instructions: instructions.into().into(),
            options: options
                .into_iter()
                .map(|option| (option.into(), None))
                .collect(),
        }
    }

    /// The same, with each option described.
    pub fn between_described(
        instructions: impl Into<String>,
        options: impl IntoIterator<Item = (impl Into<String>, impl Into<String>)>,
    ) -> Self {
        Self::Choice {
            instructions: instructions.into().into(),
            options: options
                .into_iter()
                .map(|(option, says)| (option.into(), Some(says.into())))
                .collect(),
        }
    }

    /// An ordered rubric, lowest level first.
    ///
    /// note: the documentation asks for at least two levels and the endpoint does not enforce it.
    /// One level comes back `score: 0.0` with `confidence: 1.0`, which is a confident answer to a
    /// question that had only one possible answer - see [`Answer::Score`].
    pub fn score(
        instructions: impl Into<String>,
        levels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::Score {
            instructions: instructions.into().into(),
            levels: levels.into_iter().map(Into::into).collect(),
        }
    }

    /// Gives this question structured instructions, rather than the plain text of them.
    ///
    /// note: `instructions` takes a string, an object or an array, like `state` does. This is how
    /// to hand over an object or an array without going through a `String`.
    pub fn structured(self, instructions: impl Into<Value>) -> Self {
        let instructions = instructions.into();
        match self {
            Self::Noul {
                when_true,
                when_false,
                ..
            } => Self::Noul {
                instructions,
                when_true,
                when_false,
            },
            Self::Choice { options, .. } => Self::Choice {
                instructions,
                options,
            },
            Self::Score { levels, .. } => Self::Score {
                instructions,
                levels,
            },
        }
    }

    /// The payload for this one question, as the documented request shape has it.
    ///
    /// note: public because [`Jev`] is not the only thing that builds one of these bodies - a
    /// local engine is spoken to over a pipe in the same shape, by a caller in another crate,
    /// and two renderers for one documented format eventually disagree. [`Jev::render`] is the
    /// whole body; this is one question of it.
    pub fn to_wire(&self) -> Value {
        match self {
            Self::Noul {
                instructions,
                when_true,
                when_false,
            } => {
                let mut question = json!({ "type": "noul", "instructions": instructions });
                // note: the key is `criteria`, and either side of it may be left out; an empty
                // object is not sent at all, because a question with nothing to say about its
                // own sides is the ordinary case
                let mut criteria = json!({});
                if let Some(says) = when_true {
                    criteria["true"] = says.clone().into();
                }
                if let Some(says) = when_false {
                    criteria["false"] = says.clone().into();
                }
                if criteria.as_object().is_some_and(|it| !it.is_empty()) {
                    question["criteria"] = criteria;
                }

                question
            }
            Self::Choice {
                instructions,
                options,
            } => {
                let criteria: Value = options
                    .iter()
                    .map(|(option, says)| {
                        (
                            option.clone(),
                            says.clone().map_or(Value::Null, Value::String),
                        )
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into();

                json!({ "type": "choice", "instructions": instructions, "criteria": criteria })
            }
            Self::Score {
                instructions,
                levels,
            } => {
                json!({ "type": "score", "instructions": instructions, "criteria": levels })
            }
        }
    }
}

/// What came back for one question.
///
/// note: the variant is read off the answer's own `type` rather than assumed from what was asked,
/// and the accessors on [`Answers`] return `None` for a mismatch instead of a default. A `choice`
/// answered as a `noul` is a change at the other end, and a program that quietly read `0.0` out
/// of it would act on a number nobody sent.
///
/// note: `#[non_exhaustive]` for the reason [`Question`] is - an answer shape arrives because a
/// question shape did.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Answer {
    /// How true the claim is, from 0 to 1.
    Noul {
        /// The probability itself.
        noul: f64,
    },
    /// Which option, and how the rest of the distribution fell.
    Choice {
        /// The likeliest option.
        choice: String,
        /// The whole distribution, which sums to 1.
        probabilities: BTreeMap<String, f64>,
        /// How sure the model is, from 0 to 1.
        confidence: f64,
    },
    /// Where on the rubric, which may fall between two levels.
    Score {
        /// The probability-weighted position, from 0 to `levels - 1`.
        score: f64,
        /// Which level each index was, echoed back.
        legend: BTreeMap<String, String>,
        /// The whole distribution over the level indices, which sums to 1.
        probabilities: BTreeMap<String, f64>,
        /// How sure the model is, from 0 to 1.
        ///
        /// note: not a guard against a badly built question. A rubric with one level comes back
        /// `score: 0.0, confidence: 1.0` - the endpoint does not enforce the two the
        /// documentation asks for - so this says how concentrated the distribution is and
        /// nothing at all about whether the question was worth asking.
        confidence: f64,
    },
}

impl Answer {
    /// Reads one back off the wire, where it is one of the three.
    fn from_wire(answer: &Value) -> Option<Self> {
        let numbers = |at: &Value| -> BTreeMap<String, f64> {
            at.as_object()
                .map(|it| {
                    it.iter()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_f64()?)))
                        .collect()
                })
                .unwrap_or_default()
        };

        match answer["type"].as_str()? {
            "noul" => Some(Self::Noul {
                noul: answer["noul"].as_f64()?,
            }),
            "choice" => Some(Self::Choice {
                choice: answer["choice"].as_str()?.to_owned(),
                probabilities: numbers(&answer["probabilities"]),
                confidence: answer["confidence"].as_f64().unwrap_or(f64::NAN),
            }),
            "score" => Some(Self::Score {
                score: answer["score"].as_f64()?,
                legend: answer["legend"]
                    .as_object()
                    .map(|it| {
                        it.iter()
                            .filter_map(|(key, value)| {
                                Some((key.clone(), value.as_str()?.to_owned()))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                probabilities: numbers(&answer["probabilities"]),
                confidence: answer["confidence"].as_f64().unwrap_or(f64::NAN),
            }),
            _ => None,
        }
    }

    /// How sure the model is, where the question was one that reports it.
    ///
    /// note: a [`Answer::Noul`] has none, and does not need one: the number *is* the confidence.
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Noul { .. } => None,
            Self::Choice { confidence, .. } | Self::Score { confidence, .. } => Some(*confidence),
        }
    }
}

/// Everything one request came back with.
///
/// note: `#[non_exhaustive]` because nothing outside this crate builds one. [`Answers::read`] is
/// where they come from, so the attribute costs a caller nothing and makes the next field a patch
/// rather than a break - and what a response carries is the other end's to widen.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Answers {
    /// The version that actually answered - `jev-1.13.0` for a request naming `jev-latest`.
    pub model: String,
    /// One answer per question, under the name it was asked under.
    pub answers: BTreeMap<String, Answer>,
    /// What the request cost.
    pub usage: Option<Usage>,
    /// The response exactly as it arrived.
    ///
    /// note: kept for the same reason [`nachalnik::ModelResponse::raw`] is - it is the only way
    /// to check what the model said against what this module mapped it to.
    pub raw: Value,
}

impl Answers {
    /// Reads a whole response back, in the shape the documented API answers in.
    ///
    /// note: the answers are read by the name they were asked under, and a name that did not
    /// come back is simply absent rather than an error. One question failing to arrive is not a
    /// reason to throw away the others, and every accessor below already answers `None` for a
    /// question nobody answered.
    ///
    /// note: public, and one reader for both engines. A local one answers over a pipe rather
    /// than a socket and is parsed by a caller in another crate; a second reader written there
    /// would be a second opinion about what `confidence` means the first time either moved.
    pub fn read(raw: Value) -> Self {
        let answers = raw["answers"]
            .as_object()
            .map(|answered| {
                answered
                    .iter()
                    .filter_map(|(name, answer)| Some((name.clone(), Answer::from_wire(answer)?)))
                    .collect()
            })
            .unwrap_or_default();

        Self {
            model: raw["model"].as_str().unwrap_or_default().to_owned(),
            answers,
            usage: read_usage(&raw["usage"]),
            raw,
        }
    }

    /// How true the named claim is, if it was asked and came back a `noul`.
    pub fn noul(&self, name: &str) -> Option<f64> {
        match self.answers.get(name)? {
            Answer::Noul { noul } => Some(*noul),
            _ => None,
        }
    }

    /// Which option the named question chose, if it was asked and came back a `choice`.
    pub fn choice(&self, name: &str) -> Option<&str> {
        match self.answers.get(name)? {
            Answer::Choice { choice, .. } => Some(choice),
            _ => None,
        }
    }

    /// Where on the rubric the named question landed, if it was asked and came back a `score`.
    pub fn score(&self, name: &str) -> Option<f64> {
        match self.answers.get(name)? {
            Answer::Score { score, .. } => Some(*score),
            _ => None,
        }
    }

    /// How sure the model was about the named question.
    pub fn confidence(&self, name: &str) -> Option<f64> {
        self.answers.get(name)?.confidence()
    }
}

/// Anything that answers typed questions put to a state.
///
/// note: [`SystemOne::ask`] is the whole of what a caller of this module does. [`Jev`] is the
/// implementation here; the reason there is a trait at all is that the other kind of System One
/// engine is a *local* one - the open ones ship as libraries rather than services, so a caller
/// reaching one is spawning a process rather than opening a socket, and this crate does not spawn
/// processes. So the second implementation lives above this crate rather than in it, and this is
/// the seam it fits.
///
/// note: concrete argument types where [`Jev::ask`] takes `impl Into<Value>` and an iterator,
/// because a trait with generic methods is not one a caller can hold as `dyn`, and a caller has
/// to: what decides which engine answers is an environment variable read at startup, and every
/// caller downstream of that is written against this and finds out nothing.
///
/// note: it does *not* extend [`Endpoint`]. Most of that trait is about an address and a listing,
/// and a local engine has neither - so a local one implementing it would be answering those with
/// nothing in order to be asked one question. [`SystemOne::notice`] is the one thing out of
/// `Endpoint` worth having here, because a caller that has quietly stopped getting answers should
/// be able to see why.
#[async_trait]
pub trait SystemOne: Send + Sync {
    /// Puts the questions to the state, and answers all of them in one request.
    async fn ask(
        &self,
        state: Value,
        questions: Vec<(String, Question)>,
    ) -> Result<Answers, BoxError>;

    /// Whatever it last wanted to say for itself, if anything.
    ///
    /// note: defaulted to nothing, so that an implementation with no story to tell - no retries,
    /// no listing to be missing from - does not have to write one.
    fn notice(&self) -> Option<String> {
        None
    }

    /// What to call this engine on a screen, or in a line saying what the advice cost.
    ///
    /// note: a sentence for a person rather than an address, because a local engine has no
    /// address - what identifies one is the command somebody started it with. Not for matching
    /// on, which is the same rule the four `name()` seams in the runtime carry.
    fn named(&self) -> String;
}

#[async_trait]
impl SystemOne for Jev {
    async fn ask(
        &self,
        state: Value,
        questions: Vec<(String, Question)>,
    ) -> Result<Answers, BoxError> {
        Jev::ask(self, state, questions).await
    }

    fn notice(&self) -> Option<String> {
        self.take_notice()
    }

    fn named(&self) -> String {
        self.host()
    }
}

/// TypeSafe's `jev`: an [`Endpoint`] that answers typed questions rather than turns.
pub struct Jev {
    client: reqwest::Client,
    base_url: Mutex<String>,
    api_key: String,
    model: Mutex<String>,
    /// Every HTTP request this has made, never reset.
    attempts: AtomicUsize,
    notice: Mutex<Option<String>>,
}

impl Jev {
    /// Builds a client for one model at one address.
    pub fn new(
        model: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        install_crypto();
        Self {
            client: reqwest::Client::builder()
                .timeout(PATIENCE)
                .build()
                .unwrap_or_default(),
            base_url: Mutex::new(base_url.into()),
            api_key: api_key.into(),
            model: Mutex::new(model.into()),
            attempts: AtomicUsize::new(0),
            notice: Mutex::new(None),
        }
    }

    /// [`DEFAULT_MODEL`] at [`DEFAULT_BASE_URL`], which is what a caller with a TypeSafe key and
    /// no opinions wants.
    pub fn latest(api_key: impl Into<String>) -> Self {
        Self::new(DEFAULT_MODEL, DEFAULT_BASE_URL, api_key)
    }

    /// [`OPENROUTER_MODEL`] at [`OPENROUTER_BASE_URL`], for a caller whose key is an OpenRouter
    /// one.
    pub fn through_openrouter(api_key: impl Into<String>) -> Self {
        Self::new(OPENROUTER_MODEL, OPENROUTER_BASE_URL, api_key)
    }

    /// Where the requests are going.
    pub fn endpoint(&self) -> String {
        self.base_url.lock().clone()
    }

    /// Which of the two services that address belongs to.
    fn service(&self) -> Service {
        Service::of(&self.host())
    }

    /// The address one question is posted to.
    ///
    /// note: separate from [`Jev::send`] so the path can be checked without a socket. It is the
    /// one part of a request that differs between the two services, and the failure it produces
    /// when it is wrong - a 404 from a service that does serve the model - is the kind that reads
    /// as an outage.
    fn url(&self) -> String {
        format!("{}/{}", self.endpoint(), self.service().route())
    }

    /// Which model is being asked.
    pub fn model(&self) -> String {
        self.model.lock().clone()
    }

    /// How many HTTP requests this has made, retries counted separately.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    /// Takes whatever this last wanted to say for itself, if anything.
    pub fn take_notice(&self) -> Option<String> {
        self.notice.lock().take()
    }

    /// The payload [`Jev::ask`] would send for these questions.
    ///
    /// note: `ask` renders through this rather than building a second one, for the reason
    /// [`nachalnik::Provider::render`] gives: two code paths that are supposed to agree
    /// eventually do not, and a preview that has quietly stopped matching is worse than none.
    pub fn render(&self, state: &Value, questions: &[(String, Question)]) -> Value {
        render(&self.model(), state, questions)
    }

    /// Puts the questions to the state, and answers all of them in one request.
    pub async fn ask(
        &self,
        state: impl Into<Value>,
        questions: impl IntoIterator<Item = (impl Into<String>, Question)>,
    ) -> Result<Answers, BoxError> {
        let state = state.into();
        let questions: Vec<(String, Question)> = questions
            .into_iter()
            .map(|(name, question)| (name.into(), question))
            .collect();
        if questions.is_empty() {
            return Err("no questions to ask".into());
        }

        let body = self.render(&state, &questions);
        let raw = self.send(&body).await?;

        Ok(Answers::read(raw))
    }

    /// Sends one rendered payload, waiting out a server that says it is busy.
    async fn send(&self, body: &Value) -> Result<Value, BoxError> {
        let url = self.url();
        let mut waited = BACKOFF;

        // note: `waiting::RETRIES`, counted the way the dialects count it - the first send
        // included. The documentation asks for an exponential backoff and does not say how far,
        // and a question put to `jev` has no better reason to be sent more often than a turn
        for attempt in 1..=RETRIES {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let sent = self
                .client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(body)
                .send()
                .await;

            let response = match sent {
                Ok(response) => response,
                // note: a timeout is the one transport failure worth repeating, for the reason
                // `waiting::worth_waiting_out` gives: everything else is either a decision or a
                // bug in what was built, and neither improves by being sent again
                Err(e) if e.is_timeout() && attempt < RETRIES => {
                    *self.notice.lock() = Some(busy(&self.model(), "timed out", waited));
                    tokio::time::sleep(waited).await;
                    waited *= 2;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            let status = response.status();
            let said = response.text().await.unwrap_or_default();
            let parsed: Value = serde_json::from_str(&said).unwrap_or(Value::Null);

            if status.is_success() {
                // note: checked even on a 200. A `detail` or an `error` beside the answers would
                // mean the service reported a failure inside a successful response, which is the
                // shape `Provider::respond` warns about and the one that otherwise reads as the
                // model having said nothing
                if let Some(complaint) = complaint(&parsed) {
                    return Err(complaint.into());
                }

                return Ok(parsed);
            }

            // 429 is a rate limit and 529 is an overloaded upstream; the documentation names both
            // and asks for a backoff. Everything else here is a decision - 400 for a model that
            // does not exist, 401 for a key, 422 for a request that would not validate - and
            // sending it again would only spend the wait
            let busy_now = status.as_u16() == 429 || status.as_u16() == 529;
            if busy_now && attempt < RETRIES {
                *self.notice.lock() = Some(busy(&self.model(), status.as_str(), waited));
                tokio::time::sleep(waited).await;
                waited *= 2;
                continue;
            }

            let said = complaint(&parsed).unwrap_or_else(|| match said.is_empty() {
                true => status.to_string(),
                false => format!("{status}: {said}"),
            });

            return Err(said.into());
        }

        Err(format!("{} never answered", self.model()).into())
    }

    /// Asks the endpoint what it serves, and says so on the notice if the model being asked for
    /// is not on the list.
    ///
    /// note: the same courtesy the Gemini dialect does, and for the same reason: a name that is
    /// not there comes back a 400 on the next request, which is a worse place to find out. An
    /// endpoint that answers no listing at all says nothing, rather than claiming every model is
    /// missing.
    ///
    /// note: there is nothing else for a probe to ask. What the sibling dialects use one for is
    /// the model's context limit, and this model has no window to measure a conversation against:
    /// a request is one state and a handful of questions, and the endpoint prices it afterwards.
    pub async fn probe(&self) {
        self.say_if_the_model_is_not_there().await;
    }

    /// Says so on the notice if the model being asked for is not one the endpoint lists.
    async fn say_if_the_model_is_not_there(&self) {
        let model = self.model();
        let listed = self.models().await;
        if listed.is_empty() || listed.iter().any(|it| same_model(it, &model)) {
            return;
        }

        *self.notice.lock() = Some(format!(
            "{} does not list {model}; it serves {}",
            self.host(),
            listed.join(", ")
        ));
    }
}

/// What a request that was refused said about itself, in whichever envelope it arrived in.
///
/// note: two, because the services do not agree on one. TypeSafe's is `detail`, carrying an
/// `error_type` beside the sentence - `authentication_error` for a key, `api_usage_error` for a
/// model that does not exist. OpenRouter's is the `error` the rest of its API and most of this
/// crate's endpoints use, carrying a `code`. Both are read here rather than at the call site,
/// which does not know and has no reason to learn which service answered.
///
/// note: the label is kept wherever there is one, and it is the part that does not get reworded.
/// A `code` is a number at one service and a string at the other, so both are read; what is never
/// invented is a label where the envelope carried none.
fn complaint(parsed: &Value) -> Option<String> {
    let envelope = match parsed.get("detail") {
        Some(detail) => detail,
        None => &parsed["error"],
    };
    let said = envelope["message"].as_str()?;

    let label =
        envelope["error_type"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| match &envelope["code"] {
                Value::String(code) => Some(code.clone()),
                Value::Number(code) => Some(code.to_string()),
                _ => None,
            });

    Some(match label {
        Some(label) => format!("{label}: {said}"),
        None => said.to_owned(),
    })
}

/// What the wait is for, as a sentence somebody reads on a status line.
fn busy(model: &str, why: &str, waiting: Duration) -> String {
    format!(
        "{model} said {why}; trying again in {}ms",
        waiting.as_millis()
    )
}

/// The token counts, where the response reported them.
///
/// note: `input_tokens` and `output_tokens` under those exact names, which is the one dialect
/// question this endpoint does not make anybody guess at. Nothing is reported about caching or
/// reasoning, so those two stay `None` - and [`Usage`] is explicit that `None` is not zero.
fn read_usage(usage: &Value) -> Option<Usage> {
    let input_tokens = usage["input_tokens"].as_u64();
    let output_tokens = usage["output_tokens"].as_u64();
    if input_tokens.is_none() && output_tokens.is_none() {
        return None;
    }

    Some(Usage {
        input_tokens,
        output_tokens,
        ..Usage::default()
    })
}

#[async_trait]
impl Endpoint for Jev {
    fn endpoint(&self) -> String {
        self.endpoint()
    }

    fn model(&self) -> String {
        self.model()
    }

    /// What the endpoint serves, which TypeSafe publishes at `/models`.
    ///
    /// note: `models[].name`, and the listing carries a description and a release date beside it
    /// that nothing here reads.
    ///
    /// note: nothing is asked of OpenRouter, which publishes no listing on the path it takes these
    /// on - and the listing it publishes elsewhere does not carry this model at all, since it is
    /// served out of an alpha route `/api/v1/models` does not report. Answering with that one would
    /// report a model that *is* served as missing. An empty answer does not:
    /// `say_if_the_model_is_not_there` reads it as nothing having been said.
    async fn models(&self) -> Vec<String> {
        if !self.service().publishes_a_listing() {
            return Vec::new();
        }

        let base = self.endpoint();
        let Ok(response) = self
            .client
            .get(format!("{base}/models"))
            .bearer_auth(&self.api_key)
            .send()
            .await
        else {
            return Vec::new();
        };
        let Ok(body) = response.json::<Value>().await else {
            return Vec::new();
        };

        body["models"]
            .as_array()
            .map(|listed| {
                listed
                    .iter()
                    .filter_map(|model| model["name"].as_str())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn set_model(&self, model: String) {
        *self.model.lock() = model;
        self.say_if_the_model_is_not_there().await;
    }

    async fn set_endpoint(&self, url: String, model: Option<String>) {
        *self.base_url.lock() = url;
        match model {
            Some(model) => self.set_model(model).await,
            None => self.say_if_the_model_is_not_there().await,
        }
    }

    fn take_notice(&self) -> Option<String> {
        self.take_notice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload, against the shapes the API reference documents for the three question types.
    #[test]
    fn each_question_goes_out_in_the_shape_its_type_is_documented_with() {
        let jev = Jev::new("jev-latest", DEFAULT_BASE_URL, "k");
        let asked = vec![
            ("plain".to_owned(), Question::noul("Is this destructive?")),
            (
                "sided".to_owned(),
                Question::noul("Is this destructive?").between("it deletes data", "it only reads"),
            ),
            (
                "bare".to_owned(),
                Question::choice("What now?", ["allow", "deny"]),
            ),
            (
                "described".to_owned(),
                Question::between_described("What now?", [("allow", "harmless")]),
            ),
            (
                "rubric".to_owned(),
                Question::score("How bad?", ["none", "some", "total"]),
            ),
        ];

        let body = jev.render(&"rm -rf /".into(), &asked);
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["state"], "rm -rf /");

        let questions = &body["questions"];
        // a noul with nothing said about its sides sends no `criteria` at all, rather than an
        // empty object the endpoint would have to ignore
        assert_eq!(questions["plain"]["type"], "noul");
        assert!(questions["plain"].get("criteria").is_none());
        assert_eq!(questions["plain"]["instructions"], "Is this destructive?");

        // and one with them uses the documented `true`/`false` keys
        assert_eq!(questions["sided"]["criteria"]["true"], "it deletes data");
        assert_eq!(questions["sided"]["criteria"]["false"], "it only reads");

        // a choice's `criteria` is a map, and an undescribed option is an explicit null rather
        // than a missing key
        assert_eq!(questions["bare"]["type"], "choice");
        assert_eq!(questions["bare"]["criteria"]["allow"], Value::Null);
        assert_eq!(questions["bare"]["criteria"]["deny"], Value::Null);
        assert_eq!(questions["described"]["criteria"]["allow"], "harmless");

        // a score's is an ordered array, lowest first
        assert_eq!(questions["rubric"]["type"], "score");
        assert_eq!(
            questions["rubric"]["criteria"],
            json!(["none", "some", "total"])
        );
    }

    /// One recorded response, read back into the three answer types.
    ///
    /// note: the body is one the live endpoint sent, kept verbatim. What it pins is the mapping,
    /// which is the half a test without a network can check.
    #[test]
    fn a_recorded_answer_is_read_back_as_the_type_it_says_it_is() {
        let raw: Value = serde_json::from_str(
            r#"{"model":"jev-1.13.0","answers":{
                 "dangerous":{"type":"noul","noul":0.98},
                 "verdict":{"type":"choice","choice":"deny","confidence":0.99,
                            "probabilities":{"ask":0.01,"allow":0.0,"deny":0.99}},
                 "risk":{"type":"score","score":2.96,"confidence":0.96,
                         "legend":{"0":"none","1":"one file","2":"one project","3":"the whole machine"},
                         "probabilities":{"0":0.01,"1":0.0,"2":0.0,"3":0.99}}},
               "usage":{"input_tokens":434,"output_tokens":71}}"#,
        )
        .expect("the recorded body parses");

        let answers: BTreeMap<String, Answer> = raw["answers"]
            .as_object()
            .expect("an object")
            .iter()
            .filter_map(|(name, answer)| Some((name.clone(), Answer::from_wire(answer)?)))
            .collect();
        let read = Answers {
            model: raw["model"].as_str().unwrap_or_default().to_owned(),
            answers,
            usage: read_usage(&raw["usage"]),
            raw,
        };

        // the version that answered, rather than the `jev-latest` that was asked for
        assert_eq!(read.model, "jev-1.13.0");

        assert_eq!(read.noul("dangerous"), Some(0.98));
        assert_eq!(read.choice("verdict"), Some("deny"));
        assert_eq!(read.score("risk"), Some(2.96));
        assert_eq!(read.confidence("verdict"), Some(0.99));

        // a noul's number *is* its confidence, so there is no second one to report
        assert_eq!(read.confidence("dangerous"), None);

        // and the accessors refuse a type they were not asked for, rather than defaulting: a
        // `choice` read as a `noul` would otherwise hand a program a 0.0 nobody sent
        assert_eq!(read.noul("verdict"), None);
        assert_eq!(read.score("verdict"), None);
        assert_eq!(read.choice("risk"), None);
        assert_eq!(read.noul("never asked"), None);

        assert_eq!(
            read.usage,
            Some(Usage {
                input_tokens: Some(434),
                output_tokens: Some(71),
                ..Usage::default()
            })
        );
    }

    /// Which service an address belongs to, and what follows from it.
    ///
    /// note: the addresses, not the enum. What this is actually holding is that a client built the
    /// way each service documents posts to the path that service serves - the failure otherwise is
    /// a 404 from a service that does serve the model, which reads as an outage and is not one.
    #[test]
    fn the_address_decides_which_service_a_question_goes_to() {
        let url = |base| Jev::new("jev-latest", base, "k").url();

        assert_eq!(
            url(DEFAULT_BASE_URL),
            "https://api.typesafe.ai/v1/systemone"
        );
        assert_eq!(
            url(OPENROUTER_BASE_URL),
            "https://openrouter.ai/api/alpha/decisions"
        );
        // and the constructors, since each names one of the two and nothing checks that they agree
        assert_eq!(Jev::latest("k").url(), url(DEFAULT_BASE_URL));
        assert_eq!(Jev::through_openrouter("k").url(), url(OPENROUTER_BASE_URL));

        // a self-hosted proxy of TypeSafe keeps TypeSafe's paths, which is why anything
        // unrecognised is read as that one rather than refused
        assert_eq!(
            url("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080/v1/systemone"
        );

        // the authority alone, the way `openai::ranks_apps` reads one: a port and a subdomain
        // still count, and a host that merely ends in those letters does not
        assert_eq!(Service::of("openrouter.ai:443"), Service::OpenRouter);
        assert_eq!(Service::of("api.openrouter.ai"), Service::OpenRouter);
        assert_eq!(Service::of("openrouter.ai.example.com"), Service::TypeSafe);
        assert_eq!(Service::of("notopenrouter.ai"), Service::TypeSafe);

        // and only one of the two has a listing to ask for. OpenRouter publishes none on this
        // path, and the one it publishes elsewhere does not carry this model - asking it would
        // report a model that is served as missing
        assert!(Service::TypeSafe.publishes_a_listing());
        assert!(!Service::OpenRouter.publishes_a_listing());
    }

    /// The two refusals the endpoint actually sends, which use `detail` rather than `error`.
    #[test]
    fn a_refusal_is_read_out_of_the_envelope_this_endpoint_uses() {
        let auth: Value = serde_json::from_str(
            r#"{"detail":{"error_type":"authentication_error","message":"Cannot authenticate with the server. Please check your API key and try again."}}"#,
        )
        .expect("it parses");
        let read = complaint(&auth).expect("a refusal names itself");
        assert!(read.starts_with("authentication_error: "), "{read}");
        assert!(read.contains("check your API key"), "{read}");

        let unknown: Value = serde_json::from_str(
            r#"{"detail":{"error_type":"api_usage_error","message":"Unknown model: jev-nope"}}"#,
        )
        .expect("it parses");
        assert_eq!(
            complaint(&unknown).as_deref(),
            Some("api_usage_error: Unknown model: jev-nope")
        );

        // and OpenRouter's, which is the `error` envelope with a numeric code rather than a named
        // type. Read by the same function, because nothing holding one of these knows or needs to
        // know which of the two services answered
        let router: Value = serde_json::from_str(
            r#"{"error":{"code":401,"message":"No auth credentials found"},"user_id":null}"#,
        )
        .expect("it parses");
        assert_eq!(
            complaint(&router).as_deref(),
            Some("401: No auth credentials found")
        );

        // a code is a number at one service and a string at the other, and a refusal that carried
        // no label at all is the sentence on its own rather than an invented one
        let worded: Value = serde_json::from_str(
            r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#,
        )
        .expect("it parses");
        assert_eq!(
            complaint(&worded).as_deref(),
            Some("context_length_exceeded: too long")
        );
        let bare: Value =
            serde_json::from_str(r#"{"error":{"message":"Not Found"}}"#).expect("it parses");
        assert_eq!(complaint(&bare).as_deref(), Some("Not Found"));

        // an ordinary answer is not a complaint, which is what lets a 200 be checked for one
        let fine: Value =
            serde_json::from_str(r#"{"model":"jev-1.13.0","answers":{}}"#).expect("it parses");
        assert_eq!(complaint(&fine), None);
    }

    /// A usage block that says nothing is `None` rather than a pair of zeroes.
    #[test]
    fn an_unreported_cost_is_not_a_free_one() {
        assert_eq!(read_usage(&Value::Null), None);
        assert_eq!(read_usage(&json!({})), None);
        assert_eq!(
            read_usage(&json!({ "input_tokens": 10 })),
            Some(Usage {
                input_tokens: Some(10),
                ..Usage::default()
            })
        );
    }

    /// Asking nothing is a caller's mistake, and is refused before a request is made.
    #[tokio::test]
    async fn a_request_with_no_questions_in_it_is_not_sent() {
        let jev = Jev::new("jev-latest", "http://127.0.0.1:1", "k");
        let empty: Vec<(String, Question)> = Vec::new();
        assert!(jev.ask("anything", empty).await.is_err());
        assert_eq!(jev.attempts(), 0, "nothing should have gone out");
    }

    /// A server that stays busy is asked as many times as a dialect would ask it, and no more.
    ///
    /// note: `waiting::RETRIES` counts the first send, and this client once counted only the
    /// retries against the same number. It waits out the real backoff, so it takes a few seconds.
    #[tokio::test]
    async fn a_busy_server_is_asked_as_often_as_a_dialect_would_ask_it() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\
                              Connection: close\r\n\r\n",
                        )
                        .await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        let jev = Jev::new("jev-latest", format!("http://{at}"), "k");
        assert!(
            jev.ask("anything", [("q", Question::noul("Is this fine?"))])
                .await
                .is_err()
        );
        assert_eq!(jev.attempts(), RETRIES);
    }
}
