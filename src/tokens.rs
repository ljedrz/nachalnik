use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;

#[cfg(doc)]
use crate::{Budget, Kernel, Usage};
use crate::{
    context::{ContextItem, ContextKind},
    model::Content,
};

/// Turns content into a token count.
///
/// note: The kernel never claims to know a provider's tokenizer. Every token figure it reports
/// comes from this trait, and the default implementation is an admitted estimate; swap in a
/// real tokenizer with [`Kernel::set_counter`](crate::Kernel::set_counter) when the numbers
/// need to be exact.
pub trait TokenCounter: Send + Sync {
    /// Returns the number of tokens `content` is expected to occupy.
    fn count(&self, content: &Content) -> usize;

    /// Returns the number of tokens a tool definition is expected to occupy.
    ///
    /// note: The default implementation counts the JSON schema as if it were content, sharing it
    /// rather than copying it - measuring something is not a reason to duplicate it, and this
    /// runs for every tool on every request.
    fn count_schema(&self, schema: &Arc<Value>) -> usize {
        self.count(&Content::Json(schema.clone()))
    }

    /// Returns the number of tokens a whole context item is expected to occupy.
    ///
    /// note: The default implementation counts the content, plus the tool calls and the
    /// reasoning an assistant turn carries - a turn whose text is empty still costs whatever the
    /// model wrote into the call it is requesting, and a report that said `0` there would be
    /// lying.
    fn count_item(&self, item: &ContextItem) -> usize {
        let mut tokens = self.count(&item.content);

        if let ContextKind::AssistantMessage {
            tool_calls,
            reasoning,
        } = &item.kind
        {
            for call in tool_calls {
                tokens += self.count(&Content::text(&*call.tool))
                    + self.count(&Content::Json(call.args.clone()));
                if !call.extra.is_null() {
                    tokens += self.count(&Content::Json(call.extra.clone()));
                }
            }
            if let Some(reasoning) = reasoning {
                tokens += self.count(reasoning);
            }
        }

        tokens
    }

    /// Reports what a provider charged for a request this counter had estimated.
    ///
    /// note: The kernel calls this after every response that carries
    /// [`Usage::input_tokens`], with its own estimate of the whole request - context and tool
    /// definitions - beside the provider's figure for the same bytes. It is the only feedback
    /// there is: the kernel does not have the model's tokenizer, but it does get told, once per
    /// request, exactly how wrong it was.
    ///
    /// note: The default does nothing, because deciding what to make of that is not the kernel's
    /// business. [`Calibrating`] is the implementation that acts on it.
    fn observe(&self, estimated: usize, reported: usize) {
        let _ = (estimated, reported);
    }
}

/// A [`TokenCounter`] that divides the byte length of the content by a fixed number.
///
/// note: This is an estimate, not a tokenizer. It is the default only because the alternative
/// would be to embed a tokenizer (and thus a model-specific assumption) in the kernel.
///
/// note: It is usually a *low* estimate, and how low depends on what is being sent. Measured
/// against a real API: about a third low on a short chat carrying four tool definitions, where
/// the schemas and the per-message framing are most of the request, and a steady 7% low once the
/// conversation is a few thousand tokens and the prose dominates. It cannot see framing, and it
/// never sees the tokens a reasoning model spends thinking. Treat its figures as a floor, compare
/// them against [`Budget::reported`](crate::Budget::reported), and put a real tokenizer in its
/// place before making decisions that have to be right - or wrap it in [`Calibrating`].
#[derive(Debug, Clone, Copy)]
pub struct BytesPerToken {
    /// The assumed average number of bytes per token; the count is rounded up.
    pub bytes_per_token: usize,
}

impl Default for BytesPerToken {
    fn default() -> Self {
        Self { bytes_per_token: 4 }
    }
}

impl TokenCounter for BytesPerToken {
    fn count(&self, content: &Content) -> usize {
        let divisor = self.bytes_per_token.max(1);
        content.byte_len().div_ceil(divisor)
    }
}

/// What a [`Calibrating`] counter has learned, and what it is derived from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Calibration {
    /// The factor applied to the underlying counter's figures; `1.0` until something has been
    /// observed.
    pub scale: f64,
    /// How many requests it has been told about.
    pub observations: usize,
    /// The tokens it estimated for those requests, in total.
    pub estimated: u64,
    /// The tokens the providers charged for them, in total.
    pub reported: u64,
}

/// A [`TokenCounter`] that corrects another one against what providers actually charge.
///
/// The problem it solves is the one [`BytesPerToken`] admits to: an estimate made without the
/// model's tokenizer is wrong by an amount nobody can know in advance, and the amount depends on
/// the shape of what is being sent. The answer here is not to guess better but to *stop guessing
/// after the first response*. The kernel knows what it estimated for a request and the provider
/// says what the same bytes cost, so the ratio between them is a fact, and this counter
/// multiplies by it from then on.
///
/// ```
/// use std::sync::Arc;
/// use nachalnik::{BytesPerToken, Calibrating, Config, Kernel};
///
/// let kernel = Kernel::new(Config::default());
/// let counter = Arc::new(Calibrating::new(BytesPerToken::default()));
/// kernel.set_counter(counter.clone());
///
/// // ... after a turn or two, the correction is a number you can look at
/// let learned = counter.calibration();
/// assert_eq!(learned.scale, 1.0, "nothing has been observed yet");
/// assert_eq!(learned.observations, 0);
///
/// // items counted before it learned still carry their old figures; this brings them into line
/// kernel.recount();
/// ```
///
/// note: The correction is a single multiplier, so it spreads what is really a per-message
/// overhead across the bytes. That is an approximation - but it is one calibrated against the
/// truth, rather than a constant picked in advance. Measured against a real API over a growing
/// conversation, the underlying estimate ran a steady 7% low once the context was a few thousand
/// tokens, and this brought it to within 1%.
///
/// note: It learns only from requests big enough to have a systematic error in them. On a
/// request of a few dozen tokens the percentage error is noise - a token either way - and a
/// counter that chased it would arrive at a scale that is wrong for every request that matters.
///
/// note: It corrects what is counted *from then on*. The figures already stored on context items
/// do not change by themselves, because silently rewriting recorded numbers is exactly the sort
/// of thing this crate does not do; [`Kernel::recount`] rewrites them when you ask, and says so
/// on the event stream. [`Budget::reported`] remains the provider's own last word either way.
///
/// note: The ratio is cumulative over every observation, so it settles rather than chasing the
/// last request. Swapping the model invalidates it - the new one tokenizes differently - which is
/// what [`Calibrating::reset`] is for.
#[derive(Debug)]
pub struct Calibrating<C> {
    inner: C,
    learned: RwLock<Calibration>,
}

/// How far from `1.0` a single multiplier is allowed to stray, so that one nonsensical pair of
/// numbers cannot turn a budget into a fiction.
const BOUNDS: (f64, f64) = (0.1, 10.0);

/// How large a request has to be before it is worth learning anything from.
///
/// note: Measured against a real API, the underlying estimate is out by about 7% on a request of
/// a few thousand tokens - a systematic error worth correcting - and by anything between -20% and
/// +17% on requests of a few dozen, where the absolute error is a handful of tokens and the
/// percentage is noise. Learning from the second kind makes the first kind worse, so it is
/// ignored.
const WORTH_LEARNING_FROM: usize = 256;

impl<C> Calibrating<C> {
    /// Wraps a counter, correcting nothing until it has been told something.
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            learned: RwLock::new(Calibration {
                scale: 1.0,
                observations: 0,
                estimated: 0,
                reported: 0,
            }),
        }
    }

    /// Returns what it has learned so far.
    pub fn calibration(&self) -> Calibration {
        *self.learned.read()
    }

    /// Forgets it, which is what a change of model calls for.
    pub fn reset(&self) {
        *self.learned.write() = Calibration {
            scale: 1.0,
            observations: 0,
            estimated: 0,
            reported: 0,
        };
    }

    /// Returns the counter being corrected.
    pub fn inner(&self) -> &C {
        &self.inner
    }

    /// Applies the correction to one figure.
    fn corrected(&self, tokens: usize) -> usize {
        let scale = self.learned.read().scale;

        (tokens as f64 * scale).round() as usize
    }
}

impl<C: TokenCounter> TokenCounter for Calibrating<C> {
    fn count(&self, content: &Content) -> usize {
        self.corrected(self.inner.count(content))
    }

    // both of these are delegated rather than left to the default implementations, so that a
    // wrapped counter with opinions of its own keeps them and only has them scaled
    fn count_schema(&self, schema: &Arc<Value>) -> usize {
        self.corrected(self.inner.count_schema(schema))
    }

    fn count_item(&self, item: &ContextItem) -> usize {
        self.corrected(self.inner.count_item(item))
    }

    fn observe(&self, estimated: usize, reported: usize) {
        // a request too small to have a systematic error in it says nothing about the ratio, and
        // one estimated at nothing says less than that
        if estimated == 0 || reported < WORTH_LEARNING_FROM {
            return;
        }

        let mut learned = self.learned.write();
        learned.observations += 1;
        // what was estimated is what this counter had *already* corrected, so the totals are kept
        // in the underlying counter's own units to keep the ratio from compounding
        learned.estimated += (estimated as f64 / learned.scale).round() as u64;
        learned.reported += reported as u64;
        learned.scale =
            (learned.reported as f64 / learned.estimated.max(1) as f64).clamp(BOUNDS.0, BOUNDS.1);
    }
}
