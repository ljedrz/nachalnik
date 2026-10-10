//! System One models: typed questions put to a state, answered with numbers. [`Client`] speaks to
//! any of them.
//!
//! note: the one thing in this crate that is not a [`Dialect`](crate::Dialect). It generates no
//! text, calls no tools and streams nothing, so there is no turn for it to drive and no
//! [`nachalnik::Provider`] for it to implement - a kernel never sees any of this. What it answers
//! is the question a program asks *around* a conversation: whether to run that command, which of
//! four branches this is, how bad the thing it just read is.
//!
//! note: named for the kind of model rather than for any company selling one, and favouring none
//! of them. OpenRouter serves a family of these under one API - TypeSafe's, Liquid's, Inception's,
//! Upstage's and more, listed at `/models?output_modalities=decisions` - so a client takes a model
//! identifier from that listing and has no default of its own. A provider that sells one and is not
//! on OpenRouter's list is reached through OpenRouter's bring-your-own-key, rather than by a path
//! of its own in here.
//!
//! note: one route, `/systemone` under the base URL, and the request body is the same wherever it
//! goes. That is OpenRouter's path under its ordinary `/api/v1`, so a session and its advisor can
//! share an address and a key; it is also the path a self-hosted engine keeps - `laya-serve`
//! answers there - which is what [`crate::Endpoint::set_endpoint`] is for. [`SystemOne`] is the
//! seam a caller holds as `dyn`, so that what it asks with is decided in one place and a test can
//! answer in its place.
//!
//! note: one exception to the one route, Cloudflare's Workers AI, which this client knows by its
//! address - see [Engines](#engines). And one request shape and one answer shape is all the engines
//! agree on: what is in an answer is each engine's own, passed through rather than reconciled -
//! see [Where engines differ](#where-engines-differ).
//!
//! note: three question types and they are asked together in one request. Each is evaluated on
//! its own against the same state, which is the reason to ask them that way rather than in one
//! bundled sentence: the answers do not interfere, and the round trip is paid for once.
//!
//! ```no_run
//! # use nachalnik_providers::system1::{Client, DEFAULT_BASE_URL, Question};
//! # async fn go() -> Result<(), nachalnik::BoxError> {
//! let model = std::env::var("SYSTEM1_MODEL")?;
//! let engine = Client::new(model, DEFAULT_BASE_URL, std::env::var("OPENROUTER_API_KEY")?);
//! let answers = engine
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
//!
//! # Engines
//!
//! Every engine below takes the same body and answers in the same shape. Where they part company
//! is the address, the key and how they are run; what they answer is the next section's business.
//!
//! **OpenRouter**, [`DEFAULT_BASE_URL`], with an OpenRouter key. The models are those its listing
//! names for `output_modalities=decisions`, which is what [`Client::probe`] asks for.
//!
//! **laya**, on the machine. [`laya`](https://github.com/NandhaKishorM/laya)'s `laya-serve`
//! answers `/v1/systemone`, with no key:
//!
//! ```text
//! pip install "laya[serve]" && laya-serve      # base URL http://127.0.0.1:8000/v1
//! ```
//!
//! What it answers has not been checked against a running server from here.
//!
//! **Clef-Flash on llama.cpp**, on the machine. `llama-server` serving
//! [Clef-Flash](https://huggingface.co/Cloudflare/clef-flash)'s GGUF answers `/v1/systemone`, with no
//! key, and lists the model under the name to ask for at `/v1/models`:
//!
//! ```text
//! llama-server -hf ggml-org/Clef-Flash-GGUF -b 4096 -ub 4096   # base URL http://127.0.0.1:8080/v1
//! ```
//!
//! The batch size is not optional: a request is read in one batch, and past it llama.cpp refuses
//! with `500: input (1261 tokens) is too large to process`. Its default of 512 holds a question or
//! two and a short state, and little more. This is the engine the live suite has been run against
//! on the machine.
//!
//! **Clef and Clef-flash on Workers AI**, Cloudflare's, with a Cloudflare API token that may use
//! Workers AI. The base URL is an account's `/ai/run`, and the model is named the way Workers AI
//! names it:
//!
//! ```text
//! base URL  https://api.cloudflare.com/client/v4/accounts/<account id>/ai/run
//! model     @cf/cloudflare/clef        (or @cf/cloudflare/clef-flash)
//! ```
//!
//! The model's whole URL off its page works as the base URL too, with the model named `clef`. An
//! address on [`is_workers_ai`] is posted to under the model's name rather than at `/systemone`, the
//! body carries the short name the schema there takes (`clef`, not `@cf/cloudflare/clef`), and the
//! answer and the refusals are read out of the `result` and `errors` its REST API wraps everything
//! in. No listing is asked for, since there is none at that address. Three things it offers are not
//! used: an address through Cloudflare's AI Gateway is not recognised, and is asked at `/systemone`
//! and refused; Clef's `images`, which this client has no way to send; and the listing elsewhere on
//! the account. None of this has met a live account: the shapes are the ones Cloudflare's schema
//! and Clef's reference implementation publish, pinned by tests that answer in them over a socket.
//!
//! # Where engines differ
//!
//! What has been seen to differ, so that an answer that surprises a caller can be put down to the
//! engine before it is put down to the state. None of it is corrected here.
//!
//! **Whether it answers at all.** Some take only part of the API - one on OpenRouter's list answers
//! plain `noul` questions about a conversation and refuses a `score` - and a refusal comes back as
//! the engine's own error.
//!
//! **What `confidence` means.** Each engine reports its own figure, and three have been seen: the
//! likeliest option's or level's probability (Clef's reference implementation); how near a `score`
//! is to a whole level, `1 - |score - round(score)|` (llama.cpp, and a recorded Jev answer); and, for
//! a `choice`, the top probability rescaled so that an even split is `0` (llama.cpp). They give
//! different figures for one distribution, and the second calls a split even between the bottom and
//! top levels a certain middle. What they share is a bound: for a `score` below the middle, each is
//! at most the bottom level's probability, so a caller that requires confidence before reading a
//! score as the bottom level gets no more of them under one than another. An engine whose figure ran
//! above that bound would break it, and none has been checked for that except those named here.
//!
//! **Where a state lands.** The same state and question can come back a level apart from two
//! engines, and one engine has read every part of a question carrying a fragment of a state as the
//! whole state. Asserting on a placement is asserting on somebody's weights.
//!
//! **What it will take.** A rubric of one level is refused by some (Clef's schema asks for two to
//! ten) and answered by others with `score: 0.0, confidence: 1.0`. A request larger than an engine
//! reads at once is refused by some - llama.cpp at its batch size - and cut short by others: Workers
//! AI truncates a long state, and Clef's reference implementation does it by keeping the beginning.
//! An engine serving one model answers whatever model is named, and [`Answers::model`] says which
//! answered; OpenRouter refuses a name it does not serve.
//!
//! **How to see what one does.** The crate's live `tests/system1.rs` checks an engine answers in the
//! shapes this client reads: `NACHALNIK_SYSTEM1_MODEL` names the model, and
//! `NACHALNIK_SYSTEM1_BASE_URL` an engine of one's own, which needs no `OPENROUTER_API_KEY`.

use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use nachalnik::{BoxError, Usage, async_trait};
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::{Attribution, Endpoint, Keyed, RETRIES, install_crypto, same_model};

/// Where the questions go unless a caller says otherwise: OpenRouter, which serves every System
/// One model on its list from the same `/api/v1` its chat completions are on.
pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// The path a question is posted to, under the base URL.
const ROUTE: &str = "systemone";

/// Whether an address is Cloudflare's Workers AI REST API, which serves System One models under
/// their own names rather than at `/systemone`. Takes a whole URL.
///
/// note: on the authority and the start of the path, for the reason [`crate::is_openrouter`] is on
/// the authority: `api.cloudflare.com` serves a great deal that is not Workers AI, and only an
/// address under an account's `/ai/run` is one a question can be posted to.
pub fn is_workers_ai(address: &str) -> bool {
    let rest = address.split_once("://").map_or(address, |(_, rest)| rest);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = crate::host_of(authority);

    host == "api.cloudflare.com" && path.contains("/ai/run")
}

/// The model's name as Workers AI's body takes it: the last part of `@cf/cloudflare/clef`.
///
/// note: the path wants the whole name and the body the short one - the schema there accepts
/// `clef` and `clef-flash` and nothing else - so the one identifier a person sets is split here
/// rather than asking for two.
fn short_name(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

/// What a listing is asked for, so that one carrying every chat model answers with these alone.
///
/// note: OpenRouter's `/models` lists the chat models and none of these unless asked, and a model
/// missing from a listing is reported as not served. A self-hosted engine with a listing of its own
/// ignores the query.
const LISTING: &str = "models?output_modalities=decisions";

/// The documented request body: a model, a state, and the questions put to it by name.
///
/// note: public, and the one place the body is written: a caller building the body for an engine
/// of its own builds it here, and a second copy of it is a second place for the shape to drift.
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
        /// note: a list of pairs rather than a map, so that the question keeps the options in the
        /// order they were written. The payload is a JSON object either way, whose keys go out
        /// sorted unless something in the build turns on `serde_json`'s `preserve_order`, and the
        /// model is not promised an order.
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
    /// note: the documentation asks for at least two levels and not every endpoint enforces it.
    /// At one that does not, one level comes back `score: 0.0` with `confidence: 1.0`, which is a
    /// confident answer to a question that had only one possible answer - see [`Answer::Score`].
    /// Clef's schema on Workers AI refuses one, and more than ten.
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
    /// note: public for the reason [`render`] is: two renderers for one documented format
    /// eventually disagree. [`Client::render`] is the whole body; this is one question of it.
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
        /// How sure the model is, from 0 to 1; `NaN` where the answer did not say, which
        /// [`Answer::confidence`] reports as `None`.
        ///
        /// note: the engine's figure, passed through, and engines do not compute the same one -
        /// see [Where engines differ](self#where-engines-differ). Those seen so far are at most the
        /// chosen option's probability, which is the property a caller drawing a threshold on it
        /// can rely on.
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
        /// How sure the model is, from 0 to 1; `NaN` where the answer did not say, which
        /// [`Answer::confidence`] reports as `None`.
        ///
        /// note: the engine's figure, passed through, and engines do not compute the same one -
        /// see [Where engines differ](self#where-engines-differ). Some say how concentrated the
        /// distribution is and some how near [`Answer::Score::score`] is to a whole level, and the
        /// two part company on a split.
        ///
        /// note: not a guard against a badly built question. At an endpoint that takes a rubric
        /// with one level, it comes back `score: 0.0, confidence: 1.0`, which says nothing at all
        /// about whether the question was worth asking.
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
    ///
    /// note: `None` for an answer that did not say, too. It is held as `NaN`, and handed out as
    /// one it was a figure between 0 and 1 by type and none by value: a caller reading "no
    /// confidence" as `None` got a rating it would print as "NaN% sure".
    pub fn confidence(&self) -> Option<f64> {
        match self {
            Self::Noul { .. } => None,
            Self::Choice { confidence, .. } | Self::Score { confidence, .. } => {
                Some(*confidence).filter(|it| !it.is_nan())
            }
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
    /// The version that actually answered, which an engine may resolve the name asked for to -
    /// `typesafe/jev-1.13-20260917` for a request naming `typesafe/jev-1.13`.
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
    /// note: public, and the one reader. A caller standing in for an engine - a test's, most
    /// often - answers through it, and a second reader written there would be a second opinion
    /// about what `confidence` means the first time either moved.
    ///
    /// note: an answer inside a `result` is read out of it, which is the envelope Workers AI's
    /// REST API wraps every response in. Only where the top level carries no `answers` of its own,
    /// so that an engine answering plainly is read exactly as before; [`Answers::raw`] keeps the
    /// wrapper, since it is the response as it arrived.
    pub fn read(raw: Value) -> Self {
        let body = match raw.get("answers").is_none() && raw["result"].is_object() {
            true => &raw["result"],
            false => &raw,
        };
        let answers = body["answers"]
            .as_object()
            .map(|answered| {
                answered
                    .iter()
                    .filter_map(|(name, answer)| Some((name.clone(), Answer::from_wire(answer)?)))
                    .collect()
            })
            .unwrap_or_default();

        let model = body["model"].as_str().unwrap_or_default().to_owned();
        let usage = read_usage(&body["usage"]);

        Self {
            model,
            answers,
            usage,
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
/// note: [`SystemOne::ask`] is the whole of what a caller of this module does. [`Client`] is the
/// implementation here, and every engine is reached through it; the trait is what a caller holds,
/// so that code downstream of the one place an advisor is built finds out nothing about it, and a
/// test can stand in for the engine without a socket.
///
/// note: concrete argument types where [`Client::ask`] takes `impl Into<Value>` and an iterator,
/// because a trait with generic methods is not one a caller can hold as `dyn`, and a caller has
/// to: what decides which engine answers is an environment variable read at startup, and every
/// caller downstream of that is written against this and finds out nothing.
///
/// note: it does *not* extend [`Endpoint`]. Most of that trait is about an address and a listing,
/// and a stand-in has neither - so one implementing it would be answering those with nothing in
/// order to be asked one question. [`SystemOne::notice`] is the one thing out of
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
    /// note: a sentence for a person rather than an address. Not for matching on, which is the
    /// same rule the four `name()` seams in the runtime carry.
    fn named(&self) -> String;
}

#[async_trait]
impl SystemOne for Client {
    async fn ask(
        &self,
        state: Value,
        questions: Vec<(String, Question)>,
    ) -> Result<Answers, BoxError> {
        Client::ask(self, state, questions).await
    }

    fn notice(&self) -> Option<String> {
        self.take_notice()
    }

    fn named(&self) -> String {
        self.host()
    }
}

/// Any System One model at an address answering `/systemone`: an [`Endpoint`] that answers typed
/// questions rather than turns.
pub struct Client {
    client: reqwest::Client,
    base_url: Mutex<String>,
    api_key: String,
    model: Mutex<String>,
    /// Every request for an answer this has sent, retries included, never reset.
    attempts: AtomicUsize,
    notice: Mutex<Option<String>>,
    /// Who to say these questions are asked on behalf of, where the endpoint keeps a ranking; see
    /// [`Client::attributed_to`].
    app: Attribution,
}

impl Client {
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
            base_url: Mutex::new(crate::address(base_url)),
            api_key: api_key.into(),
            model: Mutex::new(model.into()),
            attempts: AtomicUsize::new(0),
            notice: Mutex::new(None),
            app: Attribution::default(),
        }
    }

    /// Says which app these questions are being asked on behalf of.
    ///
    /// note: the same [`Attribution`] a program hands its conversation's provider, so that a
    /// session asking OpenRouter for advice is one app to it rather than an attributed
    /// conversation beside anonymous advice on the same account. Sent only where the address is
    /// OpenRouter's, which is the test the provider uses: an engine of one's own keeps no ranking
    /// of the apps calling it.
    #[must_use]
    pub fn attributed_to(mut self, app: Attribution) -> Self {
        self.app = app;
        self
    }

    /// Where the requests are going.
    pub fn endpoint(&self) -> String {
        self.base_url.lock().clone()
    }

    /// The address one question is posted to.
    ///
    /// note: separate from [`Client::send`] so the path can be checked without a socket. The
    /// failure it produces when it is wrong - a 404 from a service that does serve the model - is
    /// the kind that reads as an outage.
    ///
    /// note: on [`is_workers_ai`], the model is the route. Two ways of giving the address are
    /// taken, because both are what somebody copies: the account's `.../ai/run` with the model
    /// named `@cf/cloudflare/clef`, and the model's whole URL off its page, which already ends in
    /// the name. What is not guessed at is a base that stops short of both - `.../ai/run` with the
    /// model named `clef` - which would need this to know which vendor's `@cf/` prefix to add.
    fn url(&self) -> String {
        let base = self.endpoint();
        if !is_workers_ai(&base) {
            return format!("{base}/{ROUTE}");
        }

        let model = self.model();
        match base.ends_with(&format!("/{model}"))
            || base.ends_with(&format!("/{}", short_name(&model)))
        {
            true => base,
            false => format!("{base}/{model}"),
        }
    }

    /// Which model is being asked.
    pub fn model(&self) -> String {
        self.model.lock().clone()
    }

    /// How many requests for an answer this has sent, retries counted separately; a model
    /// listing or a probe is not one.
    pub fn attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }

    /// Takes whatever this last wanted to say for itself, if anything.
    pub fn take_notice(&self) -> Option<String> {
        self.notice.lock().take()
    }

    /// The payload [`Client::ask`] would send for these questions.
    ///
    /// note: `ask` renders through this rather than building a second one, for the reason
    /// [`nachalnik::Provider::render`] gives: two code paths that are supposed to agree
    /// eventually do not, and a preview that has quietly stopped matching is worse than none.
    pub fn render(&self, state: &Value, questions: &[(String, Question)]) -> Value {
        let model = self.model();
        let named = match is_workers_ai(&self.endpoint()) {
            true => short_name(&model),
            false => &model,
        };

        render(named, state, questions)
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

        // note: `RETRIES`, counted the way the dialects count it - the first send
        // included. The documentation asks for an exponential backoff and does not say how far,
        // and a question put to one of these has no better reason to be sent more often than a
        // turn
        for attempt in 1..=RETRIES {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let sent = self
                .app
                .sign(self.client.post(&url), &url)
                .bearer(&self.api_key)
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
                // with its causes, for the reason `with_causes` gives: the error's own
                // line names the URL, and the part somebody can act on is under it
                Err(e) => return Err(crate::with_causes(&e).into()),
            };

            let status = response.status();
            let said = match response.text().await {
                Ok(said) => said,
                Err(e) if status.is_success() => return Err(crate::with_causes(&e).into()),
                // a refusal whose body never arrived is still a refusal, and its status says
                // which kind
                Err(_) => String::new(),
            };
            let parsed: Value = match serde_json::from_str(&said) {
                Ok(parsed) => parsed,
                // note: an error, not an empty answer. Read as one, a 200 that is not JSON - a
                // body cut short, a proxy's page - comes back as every question unanswered, which
                // is also what a model that declined all of them looks like
                Err(e) if status.is_success() => {
                    let words = crate::markup::quoted(&crate::markup::unmarked(&said));
                    return Err(
                        format!("{} answered with no JSON ({e}): {words}", self.model()).into(),
                    );
                }
                Err(_) => Value::Null,
            };

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
            // and asks for a backoff. 502 and 503 are what a proxy in front of it says for a
            // moment, and get one more try and no more. The dialects retry every 5xx but 501 and
            // 505, and this is answering a person at a permission prompt, where a service that is
            // down is better said at once. Everything else here is a decision - 400 for a model
            // that does not exist, 401 for a key, 422 for a request that would not validate - and
            // sending it again would only spend the wait
            let busy_now = status.as_u16() == 429 || status.as_u16() == 529;
            let blip = matches!(status.as_u16(), 502 | 503) && attempt == 1;
            if (busy_now && attempt < RETRIES) || blip {
                *self.notice.lock() = Some(busy(&self.model(), status.as_str(), waited));
                tokio::time::sleep(waited).await;
                waited *= 2;
                continue;
            }

            // note: the body's words rather than the body, because what answers here is not
            // always the service. A firewall in front of it refuses a request with a web page,
            // and the page's doctype and stylesheet are not a reason anybody can read
            let said = complaint(&parsed)
                .unwrap_or_else(|| crate::markup::status_and_words(status, &said));

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

    /// Says so on the notice if the model being asked for is not one the endpoint lists, and
    /// clears the notice if it is: whatever was waiting there is older than the switch.
    async fn say_if_the_model_is_not_there(&self) {
        let model = self.model();
        let listed = self.models().await;
        // no model is not a model the endpoint lacks; see `crate::unlisted`
        if model.is_empty() || listed.is_empty() || listed.iter().any(|it| same_model(it, &model)) {
            *self.notice.lock() = None;
            return;
        }

        *self.notice.lock() = Some(format!(
            "{} does not list {model}; it serves {} models ({})",
            self.host(),
            listed.len(),
            crate::some_of(&listed)
        ));
    }
}

/// What a request that was refused said about itself, in whichever envelope it arrived in.
///
/// note: three, because the engines do not agree on one. OpenRouter's is the `error` the rest of
/// its API and most of this crate's endpoints use, carrying a `code`. The TypeSafe SDKs' is
/// `detail`, carrying an `error_type` beside the sentence - `authentication_error` for a key,
/// `api_usage_error` for a model that does not exist - which an engine written against them
/// answers in. Cloudflare's is a list, `errors`, each with a numeric `code`, beside a `success`
/// that says `false`; the first is the one read, and an empty list - which every successful
/// response there carries - is no refusal. All three are read here rather than at the call site,
/// which does not know and has no reason to learn which one answered.
///
/// note: the label is kept wherever there is one, and it is the part that does not get reworded.
/// A `code` is a number at one service and a string at the other, so both are read; what is never
/// invented is a label where the envelope carried none.
fn complaint(parsed: &Value) -> Option<String> {
    let envelope = match (parsed.get("detail"), parsed["errors"].get(0)) {
        (Some(detail), _) => detail,
        (None, Some(first)) => first,
        (None, None) => &parsed["error"],
    };
    // or the envelope is the sentence itself: `{"detail": "..."}`, `{"error": "..."}`
    let said = envelope["message"].as_str().or_else(|| envelope.as_str())?;

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
impl Endpoint for Client {
    fn endpoint(&self) -> String {
        self.endpoint()
    }

    fn model(&self) -> String {
        self.model()
    }

    /// What the endpoint serves: the System One models on OpenRouter's list, or whatever a
    /// self-hosted engine lists.
    ///
    /// note: two shapes, because the two kinds of address publish different ones. OpenRouter's is
    /// its usual `data[].id`, asked for the decision models alone; one written against the
    /// TypeSafe SDKs answers `models[].name`. An address that answers neither says nothing, which
    /// `say_if_the_model_is_not_there` reads as nothing having been said rather than as every
    /// model missing.
    ///
    /// note: nothing is asked of Workers AI, which has no listing under the address a question
    /// goes to - its search is elsewhere on the account, paged, and lists every task's models -
    /// so the answer there is the same nothing an address with no listing gives, without the
    /// request that would have found that out.
    async fn models(&self) -> Vec<String> {
        let base = self.endpoint();
        if is_workers_ai(&base) {
            return Vec::new();
        }
        let Ok(response) = self
            .client
            .get(format!("{base}/{LISTING}"))
            .bearer(&self.api_key)
            .send()
            .await
        else {
            return Vec::new();
        };
        let Ok(body) = response.json::<Value>().await else {
            return Vec::new();
        };

        let (listed, named) = match (body["data"].as_array(), body["models"].as_array()) {
            (Some(listed), _) => (listed, "id"),
            (None, Some(listed)) => (listed, "name"),
            (None, None) => return Vec::new(),
        };

        listed
            .iter()
            .filter_map(|model| model[named].as_str())
            .map(str::to_owned)
            .collect()
    }

    async fn set_model(&self, model: String) {
        *self.model.lock() = model;
        self.say_if_the_model_is_not_there().await;
    }

    async fn set_endpoint(&self, url: String, model: Option<String>) {
        *self.base_url.lock() = crate::address(url);
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
        let engine = Client::new("vendor/decider", DEFAULT_BASE_URL, "k");
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

        let body = engine.render(&"rm -rf /".into(), &asked);
        assert_eq!(body["model"], "vendor/decider");
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

    /// An answer that carried no confidence says it has none, rather than handing out `NaN`.
    #[test]
    fn a_missing_confidence_is_none() {
        let read = Answers::read(serde_json::json!({"model": "jev", "answers": {
            "verdict": {"type": "choice", "choice": "deny", "probabilities": {"deny": 1.0}},
            "risk": {"type": "score", "score": 1.0},
        }}));

        assert_eq!(read.choice("verdict"), Some("deny"));
        assert_eq!(read.confidence("verdict"), None);
        assert_eq!(read.score("risk"), Some(1.0));
        assert_eq!(read.confidence("risk"), None);
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

        let read = Answers::read(raw);

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

    /// Every question goes to `/systemone` under whatever address the client was given.
    ///
    /// note: OpenRouter's own path for these under its ordinary `/api/v1`, and the one a
    /// self-hosted engine keeps. A wrong path is a 404 from a service that does serve the model,
    /// which reads as an outage and is not one.
    #[test]
    fn every_question_goes_to_one_route_wherever_it_goes() {
        let url = |base| Client::new("vendor/decider", base, "k").url();

        assert_eq!(
            url(DEFAULT_BASE_URL),
            "https://openrouter.ai/api/v1/systemone"
        );
        assert_eq!(
            url("http://127.0.0.1:8080/v1/"),
            "http://127.0.0.1:8080/v1/systemone"
        );
    }

    /// The two envelopes a refusal arrives in: `detail`, as the TypeSafe SDKs have it, and the
    /// `error` OpenRouter uses.
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
        // know which engine answered
        let router: Value = serde_json::from_str(
            r#"{"error":{"code":401,"message":"No auth credentials found"},"user_id":null}"#,
        )
        .expect("it parses");
        assert_eq!(
            complaint(&router).as_deref(),
            Some("401: No auth credentials found")
        );

        // a code is a number at one engine and a string at another, and a refusal that carried
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

        // and an envelope that is the sentence itself, as a FastAPI engine's `detail` usually is
        for envelope in [
            r#"{"detail":"Model overloaded, try later"}"#,
            r#"{"error":"Model overloaded, try later"}"#,
        ] {
            let said: Value = serde_json::from_str(envelope).expect("it parses");
            assert_eq!(
                complaint(&said).as_deref(),
                Some("Model overloaded, try later"),
                "{envelope}"
            );
        }

        // an ordinary answer is not a complaint, which is what lets a 200 be checked for one
        let fine: Value =
            serde_json::from_str(r#"{"model":"jev-1.13.0","answers":{}}"#).expect("it parses");
        assert_eq!(complaint(&fine), None);
    }

    /// Workers AI is told apart by its authority and its `/ai/run`, and nothing else is.
    #[test]
    fn workers_ai_is_the_account_s_ai_run_on_cloudflare_s_api() {
        for theirs in [
            "https://api.cloudflare.com/client/v4/accounts/abc/ai/run",
            "https://api.cloudflare.com/client/v4/accounts/abc/ai/run/@cf/cloudflare/clef",
            "http://api.cloudflare.com:8080/client/v4/accounts/abc/ai/run",
        ] {
            assert!(is_workers_ai(theirs), "{theirs}");
        }

        for not in [
            DEFAULT_BASE_URL,
            "http://127.0.0.1:8000/v1",
            // the rest of Cloudflare's API, which no question goes to
            "https://api.cloudflare.com/client/v4/zones",
            // and a host that only borrows the name
            "https://api.cloudflare.com.example.com/client/v4/accounts/abc/ai/run",
            "https://example.com/api.cloudflare.com/ai/run",
            // or puts it before an `@`, which is who the request is sent as and not to
            "https://api.cloudflare.com:x@example.com/client/v4/accounts/abc/ai/run",
        ] {
            assert!(!is_workers_ai(not), "{not}");
        }
    }

    /// On Workers AI the model is the route, the address is taken either way it is copied, and the
    /// body names the model the short way its schema asks for.
    #[test]
    fn workers_ai_is_asked_under_the_model_s_own_name() {
        let account = "https://api.cloudflare.com/client/v4/accounts/abc/ai/run";
        let whole = format!("{account}/@cf/cloudflare/clef");
        let asked = [("q".to_owned(), Question::noul("Is this fine?"))];

        for (base, model) in [
            (account.to_owned(), "@cf/cloudflare/clef"),
            (format!("{account}/"), "@cf/cloudflare/clef"),
            (whole.clone(), "clef"),
            (whole.clone(), "@cf/cloudflare/clef"),
            (format!("{account}/@cf/cloudflare"), "clef"),
        ] {
            let engine = Client::new(model, &base, "k");
            assert_eq!(engine.url(), whole, "{base} asking {model}");
            assert_eq!(
                engine.render(&"anything".into(), &asked)["model"],
                "clef",
                "{base} asking {model}"
            );
        }

        // and nowhere else is the name shortened: OpenRouter's identifiers carry their vendor
        let elsewhere = Client::new("vendor/decider", DEFAULT_BASE_URL, "k");
        assert_eq!(
            elsewhere.render(&"anything".into(), &asked)["model"],
            "vendor/decider"
        );
    }

    /// An answer inside Workers AI's `result` is read out of it, and the wrapper is kept as it came.
    ///
    /// note: the answers are in the shapes Clef's reference implementation builds them in - its
    /// `systemone_answer`, published with the weights on Hugging Face - and the wrapper is the one
    /// Workers AI's REST API puts every response in. Neither was recorded from a live account.
    #[test]
    fn an_answer_wrapped_in_a_result_is_read_out_of_it() {
        let raw = json!({
            "result": {
                "model": "clef",
                "answers": {
                    "urgent": {"type": "noul", "noul": 0.9731},
                    "team": {"type": "choice", "choice": "technical", "confidence": 0.9512,
                             "probabilities": {"billing": 0.0311, "sales": 0.0177, "technical": 0.9512}},
                    "severity": {"type": "score", "score": 2.6104, "confidence": 0.6612,
                                 "legend": {"0": "No impact", "1": "Minor", "2": "Major", "3": "Critical"},
                                 "probabilities": {"0": 0.0021, "1": 0.0062, "2": 0.3305, "3": 0.6612}}
                },
                "usage": {"input_tokens": 312, "output_tokens": 0}
            },
            "success": true,
            "errors": [],
            "messages": []
        });

        // a success carries an empty `errors`, which is not a refusal
        assert_eq!(complaint(&raw), None);

        let read = Answers::read(raw.clone());
        assert_eq!(read.model, "clef");
        assert_eq!(read.noul("urgent"), Some(0.9731));
        assert_eq!(read.choice("team"), Some("technical"));
        assert_eq!(read.confidence("team"), Some(0.9512));
        assert_eq!(read.score("severity"), Some(2.6104));
        assert_eq!(read.confidence("severity"), Some(0.6612));
        // nothing is generated, and an honest zero is a zero rather than an unreported figure
        assert_eq!(
            read.usage,
            Some(Usage {
                input_tokens: Some(312),
                output_tokens: Some(0),
                ..Usage::default()
            })
        );
        assert_eq!(read.raw, raw, "the response as it arrived, wrapper and all");

        // and an engine that answers plainly beside a `result` of its own is read at the top level
        let plain = Answers::read(json!({
            "model": "jev-1.13.0",
            "answers": {"q": {"type": "noul", "noul": 0.1}},
            "result": {"answers": {"q": {"type": "noul", "noul": 0.9}}}
        }));
        assert_eq!(plain.noul("q"), Some(0.1));
    }

    /// A refusal in Cloudflare's envelope is read out of its first `errors` entry, code and all.
    #[test]
    fn a_refusal_in_cloudflare_s_envelope_names_its_code() {
        let refused = json!({
            "result": null,
            "success": false,
            "errors": [{"code": 10000, "message": "Authentication error"}],
            "messages": []
        });

        assert_eq!(
            complaint(&refused).as_deref(),
            Some("10000: Authentication error")
        );
    }

    /// An engine that does not override [`SystemOne::notice`] has nothing to say.
    ///
    /// note: the default is what a stand-in is held to, and a client polls it on every tick:
    /// anything but `None` from it is a status line that never clears.
    #[test]
    fn an_engine_with_no_notice_of_its_own_says_nothing() {
        struct Quiet;

        #[async_trait]
        impl SystemOne for Quiet {
            async fn ask(
                &self,
                _state: Value,
                _questions: Vec<(String, Question)>,
            ) -> Result<Answers, BoxError> {
                Err("never asked".into())
            }

            fn named(&self) -> String {
                "quiet".to_owned()
            }
        }

        let quiet: &dyn SystemOne = &Quiet;
        assert_eq!(quiet.notice(), None);
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

    /// An engine is called by the host it asks, on a screen and in a line saying what advice cost.
    #[test]
    fn the_engine_is_named_for_the_host_it_asks() {
        let named = |base| SystemOne::named(&Client::new("vendor/decider", base, "k"));

        assert_eq!(named(DEFAULT_BASE_URL), "openrouter.ai");
        assert_eq!(named("http://127.0.0.1:8080/v1"), "127.0.0.1:8080");
    }

    /// Asking nothing is a caller's mistake, and is refused before a request is made.
    #[tokio::test]
    async fn a_request_with_no_questions_in_it_is_not_sent() {
        let engine = Client::new("vendor/decider", "http://127.0.0.1:1", "k");
        let empty: Vec<(String, Question)> = Vec::new();
        assert!(engine.ask("anything", empty).await.is_err());
        assert_eq!(engine.attempts(), 0, "nothing should have gone out");
    }

    /// An address that answers every request with the same bytes, whatever was asked.
    async fn answering(raw: &'static [u8]) -> std::net::SocketAddr {
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
                    let _ = socket.write_all(raw).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        at
    }

    /// A server that stays busy is asked as many times as a dialect would ask it, and no more,
    /// with the wait doubling between, and the caller is told about the last wait once.
    ///
    /// note: `RETRIES` counts the first send, and this client once counted only the
    /// retries against the same number. It waits out the real backoff, so it takes a few seconds.
    ///
    /// note: the notice is read through `SystemOne`, which is where a caller holding the engine
    /// reads it, and read twice, because a notice that is not taken is a status line that never
    /// clears.
    #[tokio::test]
    async fn a_busy_server_is_asked_as_often_as_a_dialect_would_ask_it() {
        let at = answering(
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\
              Connection: close\r\n\r\n",
        )
        .await;

        let engine = Client::new("vendor/decider", format!("http://{at}"), "k");
        assert_eq!(SystemOne::notice(&engine), None, "nothing has happened yet");
        assert!(
            engine
                .ask("anything", [("q", Question::noul("Is this fine?"))])
                .await
                .is_err()
        );
        assert_eq!(engine.attempts(), RETRIES);

        let longest = BACKOFF * 2u32.pow(RETRIES as u32 - 2);
        assert_eq!(
            SystemOne::notice(&engine),
            Some(busy("vendor/decider", "429", longest))
        );
        assert_eq!(SystemOne::notice(&engine), None, "a notice is said once");
    }

    /// A 502 or a 503 is asked once more and no more, and any other 5xx is not asked again.
    ///
    /// note: once, because a proxy's blip is over by then and a service that is down is not, and
    /// the person at the prompt is better off with a quick failure than with the backoff a
    /// documented 429 gets.
    #[tokio::test]
    async fn a_gateway_blip_is_asked_once_more() {
        for (raw, attempts) in [
            (&b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..], 2),
            (&b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..], 2),
            (&b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..], 1),
        ] {
            let at = answering(raw).await;
            let engine = Client::new("vendor/decider", format!("http://{at}"), "k");
            assert!(
                engine.ask("anything", [("q", Question::noul("Is this fine?"))])
                    .await
                    .is_err()
            );
            assert_eq!(
                engine.attempts(),
                attempts,
                "{}",
                String::from_utf8_lossy(raw).lines().next().unwrap_or_default()
            );
        }
    }

    /// A server that takes the request and never answers is asked again, as often as a busy one
    /// and with the same doubling wait between.
    ///
    /// note: a timeout is the one transport failure `send` repeats. On a paused clock, so that
    /// `PATIENCE` is waited out on every send without its real thirty seconds. The listener is
    /// bound and never accepts: the connection is made, and the request sits there unread.
    #[tokio::test(start_paused = true)]
    async fn a_server_that_never_answers_is_asked_again_after_a_growing_wait() {
        let deaf = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = deaf.local_addr().expect("its address");

        let engine = Client::new("vendor/decider", format!("http://{at}"), "k");
        assert!(
            engine
                .ask("anything", [("q", Question::noul("Is this fine?"))])
                .await
                .is_err()
        );
        assert_eq!(engine.attempts(), RETRIES);

        let longest = BACKOFF * 2u32.pow(RETRIES as u32 - 2);
        let said = engine
            .take_notice()
            .expect("the last wait is on the notice");
        assert_eq!(said, busy("vendor/decider", "timed out", longest));

        // and it is a sentence a person can act on: which model, what happened, how long the wait
        for part in [
            "vendor/decider",
            "timed out",
            &format!("{}ms", longest.as_millis()),
        ] {
            assert!(said.contains(part), "{said} should say {part}");
        }
    }

    /// A question that could not be sent says why, and not only where it was going.
    #[tokio::test]
    async fn a_question_nobody_could_take_says_why() {
        install_crypto();
        let engine = Client::new("vendor/decider", "http://127.0.0.1:1", "k");
        let asked = vec![("q".to_owned(), Question::noul("Is this fine?"))];

        let said = engine
            .ask(json!({"tool": "shell"}), asked)
            .await
            .expect_err("nothing listens there")
            .to_string();

        assert!(said.to_lowercase().contains("refused"), "{said}");
    }

    /// A model named through [`Endpoint::set_model`] is the one asked from then on.
    ///
    /// note: the listing it is checked against is at an address nothing answers, which says
    /// nothing, so what is left to see is the name itself.
    #[tokio::test]
    async fn a_model_named_through_the_endpoint_is_the_one_asked() {
        let engine = Client::new("vendor/decider", "http://127.0.0.1:1", "k");

        engine.set_model("jev-1.13.0".to_owned()).await;

        assert_eq!(engine.model(), "jev-1.13.0");
        let asked = [("q".to_owned(), Question::noul("Is this fine?"))];
        assert_eq!(
            engine.render(&"anything".into(), &asked)["model"],
            "jev-1.13.0"
        );
    }

    /// A probe says so when the address does not list the model being asked for, and only then -
    /// in either shape a listing comes in.
    ///
    /// note: a name that is not listed comes back a 400 on the next question, which is a worse
    /// place to find out; a name that is listed buys silence, or the notice means nothing.
    #[tokio::test]
    async fn a_probe_says_when_the_address_does_not_list_the_model() {
        for listing in [
            // OpenRouter's
            r#"{"data":[{"id":"liquid/d1"},{"id":"vendor/decider"}]}"#,
            // and one written against the TypeSafe SDKs
            r#"{"models":[{"name":"liquid/d1"},{"name":"vendor/decider"}]}"#,
        ] {
            let raw = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n\
                 {listing}"
            );
            let at = answering(raw.leak().as_bytes()).await;

            let stranger = Client::new("vendor/nope", format!("http://{at}"), "k");
            stranger.probe().await;
            let said = stranger
                .take_notice()
                .expect("an unlisted model is worth saying");
            assert!(said.contains("vendor/nope"), "the model asked for: {said}");
            assert!(said.contains("liquid/d1"), "what is served: {said}");

            let resident = Client::new("vendor/decider", format!("http://{at}"), "k");
            resident.probe().await;
            assert_eq!(resident.take_notice(), None, "{listing}");
        }
    }

    /// The listing is asked for the decision models alone.
    ///
    /// note: OpenRouter's `/models` without the filter lists every chat model and none of these, so
    /// every model this client could be asking would be reported as not served.
    #[tokio::test]
    async fn the_listing_is_asked_for_decision_models() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        let heard = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("a request");
            let mut asked = [0u8; 1024];
            let read = socket.read(&mut asked).await.unwrap_or(0);
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await;
            String::from_utf8_lossy(&asked[..read]).into_owned()
        });

        Client::new("vendor/decider", format!("http://{at}/api/v1"), "k")
            .models()
            .await;
        let heard = heard.await.expect("the listener");
        assert!(
            heard.starts_with("GET /api/v1/models?output_modalities=decisions "),
            "{heard}"
        );
    }

    /// A probe names three of what an address serves, and how many, rather than all of them.
    #[tokio::test]
    async fn a_long_listing_is_named_in_part() {
        let at = answering(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n\
              {\"models\":[{\"name\":\"a\"},{\"name\":\"b\"},{\"name\":\"c\"},{\"name\":\"d\"},\
              {\"name\":\"e\"}]}",
        )
        .await;

        let stranger = Client::new("vendor/nope", format!("http://{at}"), "k");
        stranger.probe().await;
        let said = stranger.take_notice().expect("an unlisted model");
        assert!(said.contains("5 models (a, b, c, …)"), "{said}");
        assert!(
            !said.contains(", d"),
            "the rest are counted, not named: {said}"
        );
    }

    /// Answers one request with `reply`, and hands back what it was sent - head and body.
    async fn overheard(engine: Client, reply: &'static str) -> (String, Result<Answers, BoxError>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port");
        let at = listener.local_addr().expect("its address");
        let heard = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("a request");
            let mut seen = Vec::new();
            let mut buffer = [0u8; 4096];
            // the head and then the body the head promised, which is all one question sends
            loop {
                let text = String::from_utf8_lossy(&seen).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .and_then(|it| it.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen.extend_from_slice(&buffer[..n]),
                }
            }
            let _ = socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = socket.shutdown().await;
            String::from_utf8_lossy(&seen).into_owned()
        });

        // whatever name the engine was given, the socket is the listener's
        let mut engine = engine;
        let host = Endpoint::host(&engine);
        let host = host.split(':').next().unwrap_or_default().to_owned();
        engine.client = reqwest::Client::builder()
            .resolve(&host, at)
            .build()
            .expect("a client that resolves one name itself");
        let address = engine
            .endpoint()
            .replace(&format!("{host}:1"), &format!("{host}:{}", at.port()));
        *engine.base_url.lock() = address;

        let answered = engine
            .ask("anything", [("q", Question::noul("Is this fine?"))])
            .await;

        (heard.await.expect("the listener").to_lowercase(), answered)
    }

    /// The advice is attributed to the app at OpenRouter, the way a conversation is, and nowhere
    /// else.
    #[tokio::test]
    async fn the_advice_names_the_app_to_openrouter_and_nowhere_else() {
        const ANSWER: &str = r#"{"model":"m","answers":{"q":{"type":"noul","noul":0.5}}}"#;
        let app = || {
            Attribution::new("https://example.invalid/app", "kamchatka")
                .filed_under(["cli-agent", "programming-app"])
        };

        let (seen, answered) = overheard(
            Client::new("m", "http://openrouter.ai:1/api/v1", "k").attributed_to(app()),
            ANSWER,
        )
        .await;
        assert!(answered.is_ok(), "{answered:?}");
        assert!(seen.starts_with("post /api/v1/systemone "), "{seen}");
        assert!(
            seen.contains("referer: https://example.invalid/app"),
            "{seen}"
        );
        assert!(seen.contains("x-openrouter-title: kamchatka"), "{seen}");
        assert!(
            seen.contains("x-openrouter-categories: cli-agent,programming-app"),
            "{seen}"
        );

        // an engine of one's own keeps no ranking, and is told nothing about the app
        let (elsewhere, _) = overheard(
            Client::new("m", "http://127.0.0.1:1/v1", "k").attributed_to(app()),
            ANSWER,
        )
        .await;
        assert!(
            !elsewhere.contains("referer") && !elsewhere.contains("x-openrouter-"),
            "{elsewhere}"
        );

        // and a client with no attribution sends none wherever it is pointed
        let (silent, _) = overheard(
            Client::new("m", "http://openrouter.ai:1/api/v1", "k"),
            ANSWER,
        )
        .await;
        assert!(!silent.contains("referer"), "{silent}");
    }

    /// A question to Workers AI goes under the model's name, with the short name in the body, and
    /// what comes back inside `result` is read.
    #[tokio::test]
    async fn a_question_to_workers_ai_is_asked_and_answered_through_its_wrapper() {
        let (seen, answered) = overheard(
            Client::new(
                "@cf/cloudflare/clef",
                "http://api.cloudflare.com:1/client/v4/accounts/abc/ai/run",
                "cf-token",
            ),
            r#"{"result":{"model":"clef","answers":{"q":{"type":"noul","noul":0.25}},
                "usage":{"input_tokens":40,"output_tokens":0}},
                "success":true,"errors":[],"messages":[]}"#,
        )
        .await;

        assert!(
            seen.starts_with("post /client/v4/accounts/abc/ai/run/@cf/cloudflare/clef "),
            "{seen}"
        );
        assert!(seen.contains("authorization: bearer cf-token"), "{seen}");
        assert!(seen.contains(r#""model":"clef""#), "{seen}");

        let answered = answered.expect("an answer");
        assert_eq!(answered.model, "clef");
        assert_eq!(answered.noul("q"), Some(0.25));
    }

    /// A 200 that carries no answer is an error, not a response with every question unanswered.
    #[tokio::test]
    async fn a_success_that_is_not_an_answer_is_an_error() {
        let page = answering(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 32\r\n\
              Connection: close\r\n\r\n<html><p>Welcome back</p></html>",
        )
        .await;
        let cut = answering(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n\
              {\"model\": \"jev-1",
        )
        .await;

        for at in [page, cut] {
            let engine = Client::new("vendor/decider", format!("http://{at}"), "k");
            let asked = engine
                .ask("anything", [("q", Question::noul("Is this fine?"))])
                .await;
            assert!(asked.is_err(), "{at} answered nothing, and got {asked:?}");
            assert_eq!(engine.attempts(), 1, "a 200 is not worth asking again");
        }
    }

    /// A body that breaks off is judged by the status that came before it: a refusal is still
    /// the refusal, and a success is the transport's failure rather than a claim about the answer.
    ///
    /// note: both replies promise a `Content-Length` and hang up short of it, which is what a
    /// proxy giving up or a connection reset looks like. The status is the one part that arrived
    /// whole, and reporting the transport instead would name a broken network where a key was
    /// refused, or a malformed answer where a connection broke.
    #[tokio::test]
    async fn a_body_that_breaks_off_is_judged_by_the_status_before_it() {
        let refused = answering(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 100\r\nConnection: close\r\n\r\n\
              {\"detail\":",
        )
        .await;
        let cut = answering(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n\
              {\"model\": \"jev-1",
        )
        .await;
        let ask = |at| async move {
            Client::new("vendor/decider", format!("http://{at}"), "k")
                .ask("anything", [("q", Question::noul("Is this fine?"))])
                .await
                .expect_err("nothing whole came back")
        };

        let said = ask(refused).await.to_string();
        assert!(said.starts_with("401"), "{said}");

        // the transport's failure, with the cause under it that says the connection broke
        let failed = ask(cut).await.to_string();
        assert!(
            failed.starts_with("error decoding response body")
                && failed.contains("error reading a body from connection"),
            "{failed}"
        );
    }
}
