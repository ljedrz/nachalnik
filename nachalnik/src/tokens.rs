//! Counting what has not been sent yet.
//!
//! note: no tokenizer goes in this crate, because it would be a model-specific assumption and this
//! crate holds none, so every figure the kernel reports is an estimate and the types here mark it
//! as one. [`Calibrating`] lets a provider report what a request really cost, and the correction
//! applies from then on.

use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(doc)]
use crate::{Budget, Kernel, Usage};
use crate::{
    context::{ContextItem, ContextKind},
    model::{Content, Message, ToolCall},
};

/// Returns how many pieces of a turn's tool calls `counter` would not count: the parts
/// [`TokenCounter::count_item`] and [`TokenCounter::count_message`] charge for in each.
fn uncounted_calls<C: TokenCounter + ?Sized>(counter: &C, calls: &[ToolCall]) -> usize {
    calls
        .iter()
        .map(|call| {
            counter.uncounted(&Content::text(&*call.tool))
                + counter.uncounted(&Content::Json(call.args.clone()))
                + match call.extra.is_null() {
                    true => 0,
                    false => counter.uncounted(&Content::Json(call.extra.clone())),
                }
        })
        .sum()
}

/// Turns content into a token count.
///
/// note: every token figure the kernel reports comes from this trait, and the default is an
/// admitted estimate: the kernel never claims to know a provider's tokenizer. Install a real one
/// with [`Kernel::set_counter`](crate::Kernel::set_counter) when the numbers must be exact.
///
/// note: the counting methods are called with the context locked - counting an item is part of
/// adding it - so they must not call back into the [`Kernel`] the counter is installed in.
pub trait TokenCounter: Send + Sync {
    /// Returns the number of tokens `content` is expected to occupy.
    fn count(&self, content: &Content) -> usize;

    /// The counter's name, for a client that wants to say which one is installed.
    ///
    /// note: the default is the implementing type's path; override it to say something friendlier.
    /// It is for showing a person, not for matching on: `type_name` makes no stability promise.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Returns the number of tokens a tool definition is expected to occupy.
    ///
    /// note: The default counts the schema as content and shares it, not copying. This runs for
    /// every tool on every request.
    fn count_schema(&self, schema: &Arc<Value>) -> usize {
        self.count(&Content::Json(schema.clone()))
    }

    /// Returns the number of tokens a whole context item is expected to occupy.
    ///
    /// note: the default counts the content plus an assistant turn's tool calls and reasoning,
    /// since a turn with empty text still costs the calls it makes.
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

    /// Returns the number of tokens a projected message is expected to occupy.
    ///
    /// note: the counterpart of [`TokenCounter::count_item`], and the budget is built from it. An
    /// item and its message can differ in size (a reference gains its label, an elided item goes as
    /// a marker), and a request costs what was projected.
    fn count_message(&self, message: &Message) -> usize {
        let mut tokens = message.content.as_ref().map_or(0, |c| self.count(c));

        for call in &message.tool_calls {
            tokens += self.count(&Content::text(&*call.tool))
                + self.count(&Content::Json(call.args.clone()));
            if !call.extra.is_null() {
                tokens += self.count(&Content::Json(call.extra.clone()));
            }
        }
        if let Some(reasoning) = &message.reasoning {
            tokens += self.count(reasoning);
        }

        tokens
    }

    /// Returns how many pieces of content inside `content` this counter would not count.
    ///
    /// note: this is how a count says it did not measure something: a `0` from
    /// [`TokenCounter::count`] looks like content measured and found free. An estimate low by a
    /// known amount is a floor a client can act on; one low by an unknown amount is not. The figure
    /// feeds [`Budget::uncounted`](crate::Budget::uncounted) and
    /// [`ContextItem::uncounted`](crate::ContextItem::uncounted), so a client can say the context
    /// is larger than its token count.
    ///
    /// note: it counts pieces, not bytes or tokens, because a counter that could count those would
    /// not be abstaining.
    ///
    /// note: the default is `0`, which is right for a real tokenizer.
    fn uncounted(&self, content: &Content) -> usize {
        let _ = content;

        0
    }

    /// Returns how many pieces of a whole context item this counter would not count.
    ///
    /// note: the counterpart of [`TokenCounter::count_item`], and it counts the same parts: the
    /// content, plus the tool calls and reasoning an assistant turn carries.
    fn uncounted_item(&self, item: &ContextItem) -> usize {
        let mut uncounted = self.uncounted(&item.content);

        if let ContextKind::AssistantMessage {
            tool_calls,
            reasoning,
        } = &item.kind
        {
            uncounted += uncounted_calls(self, tool_calls);
            if let Some(reasoning) = reasoning {
                uncounted += self.uncounted(reasoning);
            }
        }

        uncounted
    }

    /// Returns how many pieces of a projected message this counter would not count.
    ///
    /// note: the counterpart of [`TokenCounter::count_message`], and
    /// [`Budget::uncounted`](crate::Budget::uncounted) is built from it, over messages rather than
    /// items for the same reason: an elided picture is a marker with no picture in it.
    fn uncounted_message(&self, message: &Message) -> usize {
        let content = message.content.as_ref().map_or(0, |c| self.uncounted(c));
        let reasoning = message.reasoning.as_ref().map_or(0, |r| self.uncounted(r));

        content + uncounted_calls(self, &message.tool_calls) + reasoning
    }

    /// Reports what a provider charged for a request this counter had estimated.
    ///
    /// note: the kernel calls this after every response carrying [`Usage::input_tokens`] and every
    /// refusal carrying [`TooLong`](crate::TooLong), with its own estimate for the whole request
    /// (context and tool definitions) beside the provider's figure for the same bytes. It is the
    /// kernel's only feedback on its estimates. A request containing anything this counter could
    /// not price is not reported, because the two figures would not cover the same bytes.
    ///
    /// note: the default does nothing; what to make of the feedback is not the kernel's business.
    /// [`Calibrating`] acts on it.
    fn observe(&self, estimated: usize, reported: usize) {
        let _ = (estimated, reported);
    }

    /// Returns what this counter has learned from [`TokenCounter::observe`], if it learns at all.
    ///
    /// note: the other half of `observe`. A [`Snapshot`](crate::Snapshot) without it would resume a
    /// long session with the counter back at `1.0`, relearning what it had already been told. The
    /// calibration holds only these four numbers; a counter whose learning does not fit them
    /// returns `None` and persists its own state.
    ///
    /// note: the default is `None`, which is right for a counter that never changes its mind.
    fn calibration(&self) -> Option<Calibration> {
        None
    }

    /// Puts back what [`TokenCounter::calibration`] handed out when a session resumes.
    ///
    /// note: the default does nothing. A counter that does not implement
    /// [`TokenCounter::calibration`] never sees this called; the kernel only offers back what a
    /// counter gave it.
    ///
    /// note: it changes what is counted from here on, not what has already been counted, as
    /// [`TokenCounter::observe`] does. A caller must apply it before items are counted, as
    /// [`Kernel::resume`] does, or recount afterwards. [`Kernel::recalibrate`] does both.
    fn recalibrate(&self, calibration: Calibration) {
        let _ = calibration;
    }
}

/// A [`TokenCounter`] that divides the byte length of the content by a fixed number.
///
/// note: an estimate, not a tokenizer. It is the default because the alternative is a
/// model-specific tokenizer in the kernel.
///
/// note: it is usually low: most on a short request, where tool schemas and per-message framing are
/// most of it, least once the conversation's prose dominates. It cannot see framing or the tokens a
/// reasoning model spends thinking. Treat its figures as a floor, compare them with
/// [`Budget::reported`](crate::Budget::reported), and wrap it in [`Calibrating`] or replace it with
/// a real tokenizer before decisions that must be right.
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
    /// note: every [`Blob`](crate::Blob) is subtracted before the division, so a picture counts as
    /// nothing wherever it appears. That is wrong, and the least wrong option here: base64 over
    /// four describes an encoding, not a model, and would make one screenshot a quarter of a
    /// million tokens and send a compactor after a context nowhere near full. What an image really
    /// costs is a vendor-specific formula over its dimensions, which no byte length reaches;
    /// [`Blob::meta`](crate::Blob::meta) carries the inputs for a counter that has one.
    ///
    /// note: wherever it appears includes inside a [`Content::Blocks`] turn, the shape a sentence
    /// and a screenshot arrive in from both dialects. [`Content::byte_len`] sums the blobs nested
    /// there, so [`Content::blobs`] is used to reach them; matching on [`Content::Blob`] alone
    /// would count a nested picture as its base64 over four.
    ///
    /// note: it under-reports, and [`BytesPerToken::uncounted`] says so: the budget states in
    /// pieces what it could not price. A real tokenizer installed with
    /// [`Kernel::set_counter`](crate::Kernel::set_counter) is the answer when the number itself
    /// must be right.
    fn count(&self, content: &Content) -> usize {
        let divisor = self.bytes_per_token.max(1);
        let blobs: usize = content.blobs().iter().map(|blob| blob.wire_len()).sum();

        content.byte_len().saturating_sub(blobs).div_ceil(divisor)
    }

    /// note: one for every blob, at any depth. Blobs are all this counter declines to measure, so
    /// the figure is exactly how many pictures nothing has priced.
    fn uncounted(&self, content: &Content) -> usize {
        content.blobs().len()
    }
}

/// What a [`Calibrating`] counter has learned, and what it is derived from.
///
/// note: `serde`, because this is the one thing a [`Snapshot`](crate::Snapshot) carries on a
/// counter's behalf - [`Snapshot::calibration`](crate::Snapshot::calibration) says why.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

impl Default for Calibration {
    /// Nothing learned: a scale of `1.0`, drawn from no observations.
    ///
    /// note: hand-written because `f64::default()` is `0.0`, and a counter multiplying by zero
    /// would report every request as costing nothing.
    fn default() -> Self {
        Self {
            scale: 1.0,
            observations: 0,
            estimated: 0,
            reported: 0,
        }
    }
}

/// A [`TokenCounter`] that corrects another one against what providers actually charge.
///
/// An estimate made without the model's tokenizer is wrong by an amount that depends on what is
/// sent and cannot be known in advance. Rather than guess better, this stops guessing after the
/// first response: the kernel knows what it estimated for a request and the provider says what
/// those bytes cost, and this counter multiplies by the ratio from then on.
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
/// note: the correction is a single multiplier, so it spreads what is really a per-message overhead
/// across the bytes: an approximation, but calibrated against the provider rather than fixed in
/// advance.
///
/// note: it learns only from requests large enough to carry a systematic error. On a few dozen
/// tokens the percentage error is noise, and chasing it would give a scale that is wrong for every
/// request that matters.
///
/// note: it corrects what is counted from then on. Figures already stored on context items change
/// only when [`Kernel::recount`] rewrites them, which it announces. [`Budget::reported`] stays the
/// provider's own last word.
///
/// note: the ratio is cumulative over every observation, so it settles instead of chasing the last
/// request. A different model invalidates it; see [`Calibrating::reset`].
#[derive(Debug)]
pub struct Calibrating<C> {
    inner: C,
    learned: RwLock<Calibration>,
}

/// How far from `1.0` a single multiplier is allowed to stray, so one bad pair of numbers cannot
/// corrupt a budget.
const BOUNDS: (f64, f64) = (0.1, 10.0);

/// How large a request has to be, on both sides of the ratio, before it is worth learning from.
///
/// note: on a request of a few thousand tokens the underlying estimate's error is systematic and
/// worth correcting; on a few dozen it is a handful of tokens either way, and learning from it
/// would make the large requests worse.
///
/// note: both sides, because an endpoint charges for framing nobody counted (its preamble, the
/// scaffolding round a tool call), a fixed cost that is the whole difference on a small request. A
/// request estimated at a few dozen tokens can be charged many times that, [`BOUNDS`] still lets
/// the ratio reach 10, and every later figure would then read ten times too high. A provider
/// calling a request large does not mean it has enough content for the framing to be a small
/// fraction of it.
const WORTH_LEARNING_FROM: usize = 256;

/// The scale a correction is actually applied at: inside [`BOUNDS`], and a number arithmetic can be
/// done with.
///
/// note: one function rather than a `clamp` where the ratio is worked out, because two paths write
/// the scale and both must hold the bound. [`TokenCounter::observe`] derives a ratio;
/// [`TokenCounter::recalibrate`] is handed one from a [`Snapshot`](crate::Snapshot), and
/// `Calibration` is `serde` with public fields, so that one was derived by nobody. A `0.0` arriving
/// there, the value [`Calibration::default`] is hand-written to avoid, would report every figure as
/// nothing, and the next `observe` would divide by it into an infinity that saturates to `u64::MAX`
/// in a running total.
///
/// note: the finite test is not redundant with the clamp: `f64::clamp` returns NaN for NaN, and a
/// NaN scale makes every count `0` (a float-to-integer `as` saturates it to zero) for the rest of
/// the session, which `observe` cannot correct because its own division returns the same NaN. `1.0`
/// corrects nothing and lets the next response teach it.
fn applicable(scale: f64) -> f64 {
    match scale.is_finite() {
        true => scale.clamp(BOUNDS.0, BOUNDS.1),
        false => 1.0,
    }
}

impl<C> Calibrating<C> {
    /// Wraps a counter, correcting nothing until it has been told something.
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            learned: RwLock::new(Calibration::default()),
        }
    }

    /// Returns what it has learned so far.
    pub fn calibration(&self) -> Calibration {
        *self.learned.read()
    }

    /// Forgets it, which is what a change of model calls for.
    pub fn reset(&self) {
        *self.learned.write() = Calibration::default();
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

    // all three are delegated, so a wrapped counter keeps its own opinions and only has them
    // scaled. `count_message` matters most: the budget is counted over it, and a real tokenizer
    // overrides it to charge for the per-message framing an estimate cannot see
    fn count_schema(&self, schema: &Arc<Value>) -> usize {
        self.corrected(self.inner.count_schema(schema))
    }

    fn count_item(&self, item: &ContextItem) -> usize {
        self.corrected(self.inner.count_item(item))
    }

    fn count_message(&self, message: &Message) -> usize {
        self.corrected(self.inner.count_message(message))
    }

    // not scaled, because a scale applies to tokens and these are pieces, but delegated, so a
    // wrapped counter that declines something keeps declining it; answering `0` here would tell a
    // client the budget is whole when it is not
    fn uncounted(&self, content: &Content) -> usize {
        self.inner.uncounted(content)
    }

    fn uncounted_item(&self, item: &ContextItem) -> usize {
        self.inner.uncounted_item(item)
    }

    fn uncounted_message(&self, message: &Message) -> usize {
        self.inner.uncounted_message(message)
    }

    fn calibration(&self) -> Option<Calibration> {
        Some(*self.learned.read())
    }

    fn recalibrate(&self, calibration: Calibration) {
        // through the same gate as the ratio this counter works out itself; see `applicable` for
        // why.
        //
        // note: and recomputed from the two totals where there are any, as `observe` does, rather
        // than taken as written: a float read back from JSON is not always the one written, so a
        // resumed session was counted on a scale one digit off and a snapshot did not equal itself
        // after a round trip
        let scale = match calibration.estimated {
            0 => calibration.scale,
            estimated => calibration.reported as f64 / estimated as f64,
        };
        *self.learned.write() = Calibration {
            scale: applicable(scale),
            ..calibration
        };
    }

    fn observe(&self, estimated: usize, reported: usize) {
        // one estimated at nothing says nothing about the ratio
        if estimated == 0 {
            return;
        }

        let mut learned = self.learned.write();
        // the estimated total is already corrected by this counter, so totals stay in the
        // underlying counter's own units to stop the ratio compounding
        let own = (estimated as f64 / learned.scale).round() as u64;
        // a request too small to have a systematic error says nothing about the ratio, and it is
        // too small when either side is: see `WORTH_LEARNING_FROM`
        if own < WORTH_LEARNING_FROM as u64 || reported < WORTH_LEARNING_FROM {
            return;
        }

        // note: a request whose own ratio is outside the bounds is not believed at all, rather than
        // believed and clamped. A pair needing more correction than `BOUNDS` allows is not a bias
        // in the estimate but two figures about different things, or nonsense. The ratio is
        // cumulative, so one request at a thousand times its estimate would dominate every ordinary
        // one: the clamp would settle on `BOUNDS.1`, every later request would read at that
        // multiple, the next would be refused against a limit the context is nowhere near, and a
        // snapshot would carry that into a resumed session.
        //
        // note: checked against this request's estimate rather than the running ratio, so only this
        // request is judged; letting the running total into the test would let the first absurd
        // report through every later one.
        let ratio = reported as f64 / own as f64;
        if !ratio.is_finite() || !(BOUNDS.0..=BOUNDS.1).contains(&ratio) {
            return;
        }

        // saturating, because these fields may have come from a snapshot. See `applicable` for the
        // same caution about the scale
        learned.observations = learned.observations.saturating_add(1);
        learned.estimated = learned.estimated.saturating_add(own);
        learned.reported = learned.reported.saturating_add(reported as u64);
        learned.scale = applicable(learned.reported as f64 / learned.estimated.max(1) as f64);
    }
}
