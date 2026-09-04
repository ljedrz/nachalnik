//! The model under test, and what asking it something costs.

use std::ops::AddAssign;

use nachalnik::{
    Config, ContextId, ContextItem, Event, Kernel, ModelInfo, State, StopReason, Usage,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, Result},
    probe::{Answer, Probe},
};

/// The model under test, and the session it is being tested in.
///
/// note: A thin thing on purpose. It owns a [`Kernel`] and adds one operation the kernel has no
/// reason to have - put a question, wait for the turn to end, hand back what was said and what it
/// cost - and everything else an experiment wants from a subject it gets from
/// [`Subject::kernel`]. There is nothing about introspection in here.
///
/// note: One subject per experiment. A session that has already been asked about itself has
/// learnt that it is being measured, which is a difference between the first experiment and the
/// second one that nothing in a score would show.
pub struct Subject {
    kernel: Kernel,
    rounds: usize,
}

impl Subject {
    /// Takes a kernel that already has its provider, its tools and its policy.
    pub fn new(kernel: Kernel) -> Self {
        Self { kernel, rounds: 3 }
    }

    /// How many times a question may exhaust `Config::max_requests_per_turn` before the harness
    /// gives up on it; three by default.
    ///
    /// note: a turn that ends because the request budget ran out is not an answer, and calling
    /// `Kernel::turn` again resumes it. This is how many times that is worth doing before
    /// [`Error::Exhausted`]: a subject that spends nine requests' worth of tool calls and still
    /// has not answered is not going to.
    #[must_use]
    pub fn rounds(mut self, rounds: usize) -> Self {
        self.rounds = rounds.max(1);
        self
    }

    /// The kernel under test.
    pub fn kernel(&self) -> &Kernel {
        &self.kernel
    }

    /// What the provider says it is, where there is one.
    pub fn model(&self) -> Option<ModelInfo> {
        self.kernel.model_info()
    }

    /// A second subject on the same provider and the same parameters, with an empty context.
    ///
    /// note: What a ladder needs and a single session cannot supply. A stage that asks a subject
    /// which has already committed to an answer measures deference to evidence; a stage that asks
    /// one which never guessed measures instrumented accuracy with nothing to defend. The two are
    /// different questions and neither is optional, so an experiment has to be able to raise a
    /// sibling - on the same model, at the same temperature, or the comparison is between two
    /// models rather than two conditions.
    ///
    /// note: tools and policy are deliberately *not* carried across. A sibling starts with the
    /// context it is given and the handles the experiment chooses to grant it, which is the point
    /// of having one.
    pub fn sibling(&self, tag: &str) -> Result<Self> {
        let provider = self
            .kernel
            .provider()
            .ok_or_else(|| Error::Setup("there is no provider to raise a sibling on".into()))?;
        let kernel = Kernel::new(Config {
            session_name: Some(format!("{}#{tag}", self.kernel.session_name())),
            ..Config::default()
        });
        kernel.set_provider(provider);
        kernel.set_params(self.kernel.params());

        Ok(Self::new(kernel))
    }

    /// Puts a question, drives the loop until the model ends its turn, and hands back what it
    /// said.
    pub async fn ask(&self, question: &str) -> Result<Said> {
        let from = self.kernel.last_seq();
        let asked = self
            .kernel
            .push(ContextItem::user(question).because("put by the evaluator"));

        let mut rounds = 0;
        let (item, stop) = loop {
            match self.kernel.turn().await? {
                State::Finished { item, stop } => break (item, stop),
                // an evaluation has no user to put a permission question to, and answering it on
                // their behalf is the one thing this workspace exists not to do
                State::Deciding { .. } => return Err(Error::Undecided),
                _ => {
                    rounds += 1;
                    if rounds >= self.rounds {
                        return Err(Error::Exhausted);
                    }
                }
            }
        };

        let said = self.kernel.item(item);

        Ok(Said {
            // what the *context* holds, rather than what the response object did: an item that
            // was recorded as ordered blocks keeps its calls and its thinking inside its content,
            // and `to_text` is the part of it the model uttered
            text: said
                .map(|item| item.content.to_text().into_owned())
                .unwrap_or_default(),
            reasoning: self.kernel.last_response().and_then(|response| {
                response
                    .reasoning
                    .as_ref()
                    .map(|r| r.to_text().into_owned())
            }),
            asked,
            item,
            stop,
            spend: Spend::since(&self.kernel, from),
        })
    }

    /// Puts a probe, and reads the answer.
    pub async fn probe(&self, probe: &Probe) -> Result<(Said, Answer)> {
        let said = self.ask(&probe.asked()).await?;
        // note: an empty turn that stopped because it ran out of room is not a subject declining
        // to commit - it is a question that never got an answer, and scoring it as a wrong claim
        // would charge a model for the harness's token ceiling. A model that stops at the limit
        // *having said something* is read normally: the reading takes the last tagged line, and
        // whether one arrived is the subject's business
        let answer = match said.stop == StopReason::Length && said.text.trim().is_empty() {
            true => Answer::Cut,
            false => probe.read(&said.text),
        };

        Ok((said, answer))
    }

    /// Everything the session has spent so far.
    pub fn spend(&self) -> Spend {
        Spend::since(&self.kernel, 0)
    }
}

/// What a subject said, and what saying it cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Said {
    /// The words, as the context recorded them.
    pub text: String,
    /// What it was thinking, where the provider reports that at all.
    pub reasoning: Option<String>,
    /// The context item the question was pushed as.
    ///
    /// note: worth having beside the answer, because an experiment that wants copies to answer a
    /// question *fresh* has to leave the whole exchange out of them, and that is two items rather
    /// than one. See [`Ablation::blind_to`](crate::Ablation::blind_to).
    pub asked: ContextId,
    /// The context item the turn was recorded as.
    pub item: ContextId,
    /// Why it stopped.
    ///
    /// note: worth keeping beside the answer, because a turn that ran out of output tokens
    /// (`StopReason::Length`) is a different thing from an unreadable answer, and the two look
    /// identical once the text has been parsed.
    pub stop: StopReason,
    /// What the exchange cost, as the provider counted it.
    pub spend: Spend,
}

/// What a stretch of a session cost, as the provider counted it.
///
/// note: The provider's figures, not the kernel's estimate. A `TokenCounter` is honest about
/// being an estimate and the default one comes out low; a benchmark reporting what it cost to run
/// should report what was actually charged, and that number only exists once a response has come
/// back. Where a provider reports nothing, these stay at zero and [`Spend::requests`] still
/// counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    /// How many requests were answered.
    pub requests: usize,
    /// Input tokens, as the provider counted them.
    pub input: u64,
    /// Output tokens, as the provider counted them.
    pub output: u64,
    /// Reasoning tokens, where the provider distinguishes them.
    pub reasoning: u64,
}

impl Spend {
    /// Adds up everything a kernel has spent since record `seq`.
    ///
    /// note: read out of the session log rather than accumulated on the side, so that the figure
    /// is the same one an auditor reading the log would arrive at. `0` is every record there is.
    pub fn since(kernel: &Kernel, seq: u64) -> Self {
        let mut spend = Self::default();
        kernel.with_history(|session| {
            for record in session.since(seq) {
                if let Event::ModelFinished { usage, .. } = &record.event {
                    spend.requests += 1;
                    spend.count(*usage);
                }
            }
        });

        spend
    }

    /// Everything the provider reported for one response.
    fn count(&mut self, usage: Option<Usage>) {
        let Some(usage) = usage else { return };
        self.input += usage.input_tokens.unwrap_or_default();
        self.output += usage.output_tokens.unwrap_or_default();
        self.reasoning += usage.reasoning_tokens.unwrap_or_default();
    }

    /// The tokens in and out together.
    pub fn tokens(&self) -> u64 {
        self.input + self.output
    }
}

impl AddAssign for Spend {
    fn add_assign(&mut self, other: Self) {
        self.requests += other.requests;
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
    }
}

impl std::iter::Sum for Spend {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), |mut total, one| {
            total += one;
            total
        })
    }
}
