//! The vocabulary both halves agree on: what a request carries, what a response brings back, and
//! the [`Provider`] that turns one into the other.
//!
//! note: nothing in here knows about HTTP, JSON schemas or any particular vendor. It is the shape
//! the kernel builds and a provider renders, which is what lets a dialect whose assistant turn is
//! an ordered list of parts and one whose turn is a content slot beside a list of calls sit behind
//! the same trait.

use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[cfg(doc)]
use crate::{Context, Kernel, Projector, TokenCounter};
use crate::{error::BoxError, event::DeltaSink, tool::ToolSpec};

mod content;

pub use content::{Blob, Block, Content, Part};

/// The role a [`Message`] is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    /// Instructions to the model.
    System,
    /// Input from the user (or from the harness on the user's behalf).
    User,
    /// Output from the model.
    Assistant,
    /// The result of a tool call.
    Tool,
}

impl Role {
    /// Returns the name the role conventionally goes by on the wire.
    ///
    /// note: This exists so that a [`Provider`] does not have to match on a `#[non_exhaustive]`
    /// enum whose only sensible fallback would be to guess. A provider whose format disagrees
    /// with the convention is free to ignore it and map the roles itself.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single message in a [`ModelRequest`].
///
/// note: Messages are not stored anywhere; they are produced from the context by a
/// [`Projector`] every time a request is built. The context items are the state - see
/// [`Context`].
///
/// note: a turn is recorded one of two ways, and never both. The conventional one is these three
/// slots - a content, an optional reasoning, a flat list of calls - which is the dialect most
/// APIs speak. The other is [`Content::Blocks`] in the content slot, an ordered sequence of
/// thinking, text and calls, for a dialect where the order is itself the information: thinking,
/// a sentence, a call, more thinking before the next one. A turn recorded that way leaves
/// [`Message::reasoning`] and [`Message::tool_calls`] empty, so that there is never a second
/// account of it to disagree with the first, and [`Message::calls`] is what reads either.
///
/// note: which of the two a request carries is [`LinearProjector::send_blocks`], and it is a
/// [`Projector`]'s decision. What a projector cannot do is invent an order that was never
/// recorded: a turn that arrived through a provider speaking the three-slot dialect has none to
/// carry, and flattening one that does is lossy and says so in [`Projection::repairs`].
///
/// [`LinearProjector::send_blocks`]: crate::LinearProjector::send_blocks
/// [`Projection::repairs`]: crate::Projection::repairs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who the message is attributed to.
    pub role: Role,
    /// The message's content, if it has any.
    pub content: Option<Content>,
    /// The model's reasoning for an assistant message, where the provider exposes it and the
    /// [`Projector`] carries it back.
    ///
    /// note: The kernel never looks inside this. It is here because some APIs require a
    /// reasoning model's own thinking to be echoed back verbatim in later requests - a signed
    /// block, an encrypted item - and a runtime that dropped it on the floor would simply not be
    /// able to talk to them. Whether it is sent is [`LinearProjector::send_reasoning`], and what
    /// it means on the wire is the provider's business.
    ///
    /// [`LinearProjector::send_reasoning`]: crate::LinearProjector::send_reasoning
    pub reasoning: Option<Content>,
    /// Tool calls carried by an assistant message.
    pub tool_calls: Vec<ToolCall>,
    /// The identifier of the tool call a [`Role::Tool`] message answers.
    pub tool_call_id: Option<ToolCallId>,
    /// The name of the tool a [`Role::Tool`] message answers.
    pub name: Option<String>,
}

impl Message {
    /// Creates a message with the given role and content.
    pub fn new(role: Role, content: impl Into<Content>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
        }
    }

    /// Attaches the model's reasoning to the message.
    pub fn with_reasoning(mut self, reasoning: Option<Content>) -> Self {
        self.reasoning = reasoning;
        self
    }

    /// Creates a [`Role::System`] message.
    pub fn system(content: impl Into<Content>) -> Self {
        Self::new(Role::System, content)
    }

    /// Creates a [`Role::User`] message.
    pub fn user(content: impl Into<Content>) -> Self {
        Self::new(Role::User, content)
    }

    /// Creates a [`Role::Assistant`] message, possibly carrying tool calls.
    pub fn assistant(content: Option<Content>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content,
            reasoning: None,
            tool_calls,
            tool_call_id: None,
            name: None,
        }
    }

    /// Returns the tool calls this message carries, wherever they are recorded.
    ///
    /// note: this, rather than the [`Message::tool_calls`] field, is what a [`Provider`] should
    /// read. A turn projected as [`Content::Blocks`] keeps its calls in the blocks, in the order
    /// the model asked for them, and leaves that field empty; a provider reading the field
    /// directly would send a request with the text of a turn and none of the calls in it, which
    /// most APIs reject and which is very hard to see afterwards. Blocks win where there are
    /// any - there is never both.
    pub fn calls(&self) -> impl Iterator<Item = &ToolCall> {
        let blocks = self.blocks();
        let flat = match blocks {
            Some(_) => None,
            None => Some(&self.tool_calls[..]),
        };

        flat.into_iter()
            .flatten()
            .chain(blocks.into_iter().flatten().filter_map(Block::call))
    }

    /// Returns the ordered blocks of this message, if it is carrying any.
    pub fn blocks(&self) -> Option<&[Block]> {
        self.content.as_ref().and_then(Content::as_blocks)
    }

    /// Creates a [`Role::Tool`] message answering the given call.
    pub fn tool_result(
        call: ToolCallId,
        tool: impl Into<String>,
        content: impl Into<Content>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(call),
            name: Some(tool.into()),
        }
    }
}

/// The identifier a provider assigns to a tool call, used to match a result to its call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ToolCallId(pub String);

impl From<String> for ToolCallId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ToolCallId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The bytes a JSON value serializes to.
///
/// note: it is walked to be measured, but it does not have to be *built*: this counts the bytes
/// as they are written and throws them away. Token counting runs over every tool schema on every
/// request, and rendering each one into a string that is immediately dropped is a cost worth not
/// paying.
fn json_len(value: &Value) -> usize {
    let mut counted = Counting(0);
    match serde_json::to_writer(&mut counted, value) {
        Ok(()) => counted.0,
        // a `Value` that will not serialize is not a thing that exists, but guessing is better
        // than panicking in a size estimate
        Err(_) => 0,
    }
}

/// A sink that measures what is written to it and keeps none of it.
struct Counting(usize);

impl std::io::Write for Counting {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();

        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A shared JSON null, for a call the provider attached nothing to.
fn null() -> Arc<Value> {
    Arc::new(Value::Null)
}

fn is_null(value: &Arc<Value>) -> bool {
    value.is_null()
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider-assigned identifier of the call.
    pub id: ToolCallId,
    /// The [`ToolSpec::id`] of the tool the model wants to invoke.
    pub tool: String,
    /// The arguments, as provided by the model.
    ///
    /// note: The kernel passes these through unvalidated; a [`Tool`](crate::Tool) is
    /// responsible for rejecting arguments that do not match its schema.
    ///
    /// note: Shared, for the same reason [`Content`] is: a call is copied into every request
    /// that follows it, and a `write_file` argument is as big as the file.
    pub args: Arc<Value>,
    /// Whatever the provider attached to this call and expects to see again, carried verbatim.
    ///
    /// note: This is [`Params`] in the other direction, and it exists for the same reason: some
    /// APIs hand back a piece of opaque state per call - Gemini's `thoughtSignature`, an
    /// encrypted reasoning item - and *reject the next request* if it does not come back
    /// attached to the call it belongs to. The kernel never looks inside it, never separates it
    /// from its call, and a provider that ignores it loses nothing.
    #[serde(default = "null", skip_serializing_if = "is_null")]
    pub extra: Arc<Value>,
}

impl ToolCall {
    /// Creates a call with nothing attached to it.
    pub fn new(
        id: impl Into<ToolCallId>,
        tool: impl Into<String>,
        args: impl Into<Arc<Value>>,
    ) -> Self {
        Self {
            id: id.into(),
            tool: tool.into(),
            args: args.into(),
            extra: Arc::new(Value::Null),
        }
    }

    /// Attaches the provider's own opaque state to the call.
    pub fn with_extra(mut self, extra: impl Into<Arc<Value>>) -> Self {
        self.extra = extra.into();
        self
    }

    /// Returns the size of the call in bytes: the tool's name, its arguments, and whatever the
    /// provider attached to it.
    ///
    /// note: a call is not free and a turn whose text is empty is not a turn that costs nothing -
    /// the model wrote the arguments, and they go out on every request that follows. These are
    /// the three things [`TokenCounter::count_item`](crate::TokenCounter::count_item) adds on top
    /// of the content, said once so that a [`Block::Call`] can be measured the same way.
    pub fn byte_len(&self) -> usize {
        let extra = match self.extra.is_null() {
            true => 0,
            false => json_len(&self.extra),
        };

        self.tool.len() + json_len(&self.args) + extra
    }
}

/// The knobs sent to the provider alongside the messages, in whatever shape that provider
/// understands.
///
/// note: The kernel has no idea what a temperature is. It carries this map to the [`Provider`]
/// verbatim, which is the only way for `thinking`, `safety_settings`, `reasoning_effort` and
/// every future vendor invention to be as first-class as `temperature` - and for the kernel to
/// be unable to send anything the user did not ask for.
pub type Params = Map<String, Value>;

/// A request to a model: exactly what will be sent, and nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// The messages, in the order they will be sent.
    pub messages: Vec<Message>,
    /// The tool definitions the model may call.
    pub tools: Vec<ToolSpec>,
    /// The model parameters, verbatim.
    pub params: Params,
}

/// Why the model stopped producing output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    /// The model finished its turn.
    EndTurn,
    /// The model wants one or more tools to be invoked.
    ToolUse,
    /// The output length limit was reached.
    Length,
    /// The model declined to answer.
    Refusal,
    /// Anything else the provider reported, verbatim.
    Other(String),
}

/// The token counts a provider reported for a request.
///
/// note: These are the provider's numbers, as opposed to the kernel's estimates, and the two
/// are kept separate on purpose: [`Kernel::budget`] is what the kernel thinks, this is what the
/// provider says, and a client showing both is telling the truth twice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens in the request.
    pub input_tokens: Option<u64>,
    /// Everything the model generated and is charged for, reasoning included.
    ///
    /// note: *including* the reasoning, which is the half a provider has to be careful about,
    /// because the dialects do not agree and the field cannot mean two things. OpenAI's
    /// `completion_tokens` already contains it; Google reports `candidatesTokenCount` and
    /// `thoughtsTokenCount` side by side and defines its own total as the two plus the prompt, so
    /// a provider speaking that dialect adds them. The rule is what makes `input_tokens +
    /// output_tokens` the whole bill for a request whichever endpoint answered it - and a figure
    /// that means one thing per provider is not a figure anybody can put beside another.
    pub output_tokens: Option<u64>,
    /// How much of [`Self::output_tokens`] was reasoning, where the provider says.
    ///
    /// note: a part of that number rather than a second one beside it, so it is never added to
    /// anything - it is subtracted from it to find what the answer itself cost. `None` means the
    /// provider did not say, which is not the same as zero and must not be shown as it.
    pub reasoning_tokens: Option<u64>,
    /// Request tokens that were served from the provider's cache.
    ///
    /// note: a part of [`Self::input_tokens`] rather than a second number beside it. That is how
    /// OpenAI's dialect defines it, and every endpoint speaking that dialect is read as meaning
    /// it; [`Usage::settled`] is what happens when one does not.
    pub cached_input_tokens: Option<u64>,
}

impl Usage {
    /// Reads the counts as the dialect defines them, where an endpoint plainly did not.
    ///
    /// note: one repair, and only the one that is *certain*. A cached prefix is inside the prompt
    /// figure, so `cached_input_tokens > input_tokens` cannot be true of the dialect this crate
    /// reads, and an endpoint reporting it is reporting the cache miss under the name of the
    /// whole. The two are added, since that is what the field is going to be read as either way.
    ///
    /// note: this must not guess, from the kernel's estimate, which convention an endpoint is
    /// speaking - taking whichever of `input` and `input + cached` is nearer what was
    /// estimated. The estimate is what the reported figure calibrates, so that reasoning is
    /// circular, and it resolves in favour of whichever error the counter has already been
    /// dragged towards. A usage figure disagreeing with an estimate is not settled here; it is
    /// settled by [`Overrun`], which is the only measurement in the units the limit uses.
    #[must_use]
    pub fn settled(self) -> Self {
        let Some((input, cached)) = self.input_tokens.zip(self.cached_input_tokens) else {
            return self;
        };

        match cached > input {
            // saturating, because both figures are whatever a provider reported
            true => Self {
                input_tokens: Some(input.saturating_add(cached)),
                ..self
            },
            false => self,
        }
    }
}

/// How long a request was, against the length the model would take.
///
/// note: the only figure in a session that is measured in the units the *limit* is enforced in.
/// [`Usage::input_tokens`] is what the endpoint charged for, and an aggregator in front of a
/// model is free to normalise that to some other tokenizer's idea of the same bytes - it is a
/// bill, and bills are quoted in one currency. An overrun a provider reports comes from the model
/// refusing to read the request, so it is the model's own count of it, and where the two disagree
/// this is the one a budget has to be kept in. One the kernel builds before sending, in
/// [`Error::TooLong`](crate::Error::TooLong), carries the kernel's own estimate instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overrun {
    /// How long the request was: what the provider said, or the kernel's estimate where the
    /// kernel refused it first.
    pub tokens: u64,
    /// What the model holds, where the provider knows it.
    ///
    /// note: [`ModelInfo::context_limit`] rather than a number out of the refusal, which is a
    /// sentence and names more numbers than it looks like it does. The difference between the two
    /// is what somebody is told to prune, so it is worth taking from the side that measured it.
    pub limit: Option<u64>,
}

/// A request the model refused to read, because it was longer than the model can take.
///
/// note: a [`Provider`] returns this in place of a plain message where it recognises the refusal,
/// which is a dialect's job and not this crate's: the sentence is a vendor's wording and nothing
/// here reads one. The kernel does not parse an error either - [`BoxError`] is carried
/// uninterpreted - but it will look for *this type* in what it was handed, because the number
/// inside is the one thing a session that has run out of room cannot find out any other way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLong {
    /// The two numbers.
    pub overrun: Overrun,
    /// What the provider said, in full, which is what a user is shown.
    pub said: String,
}

impl TooLong {
    /// Finds one of these in whatever a provider failed with, however deeply it is wrapped.
    #[must_use]
    pub fn of<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        let mut looking = Some(error);
        while let Some(error) = looking {
            if let Some(found) = error.downcast_ref::<Self>() {
                return Some(found);
            }
            looking = error.source();
        }

        None
    }
}

impl fmt::Display for TooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.said)
    }
}

impl std::error::Error for TooLong {}

/// A model's answer to a [`ModelRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelResponse {
    /// The text (or structured content) the model produced, if any.
    pub content: Option<Content>,
    /// The model's reasoning output, where the provider exposes it.
    ///
    /// note: The kernel records this on the assistant turn it belongs to, so it is as visible,
    /// countable and prunable as everything else - and so that a provider whose API requires
    /// reasoning to be echoed back verbatim can get it back out of [`Message::reasoning`]. It is
    /// never separated from the turn that produced it, because for a signed or encrypted
    /// reasoning block that would be worse than dropping it.
    pub reasoning: Option<Content>,
    /// The tools the model wants invoked.
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped.
    pub stop: StopReason,
    /// The token counts the provider reported.
    pub usage: Option<Usage>,
    /// The provider's own response payload.
    ///
    /// note: Providers are encouraged to fill this in: it is the only way for a user to check
    /// what the model *actually* said against what the provider mapped it to.
    pub raw: Option<Value>,
}

impl ModelResponse {
    /// Creates a response consisting of nothing but text.
    pub fn text(content: impl Into<Content>) -> Self {
        Self {
            content: Some(content.into()),
            reasoning: None,
            tool_calls: Vec::new(),
            stop: StopReason::EndTurn,
            usage: None,
            raw: None,
        }
    }

    /// Creates a response consisting of nothing but tool calls.
    pub fn tool_calls(calls: Vec<ToolCall>) -> Self {
        Self {
            content: None,
            reasoning: None,
            tool_calls: calls,
            stop: StopReason::ToolUse,
            usage: None,
            raw: None,
        }
    }

    /// Creates a response whose turn is an ordered sequence of blocks.
    ///
    /// note: the [`StopReason`] is derived rather than asked for, because with the blocks in hand
    /// there is nothing to ask: a turn containing a call is a turn the model expects to be
    /// answered. Override it afterwards where the provider said something else.
    ///
    /// note: [`ModelResponse::reasoning`] and [`ModelResponse::tool_calls`] are left empty, and
    /// have to be: they are the other way of recording the same turn, and a response carrying
    /// both would be two accounts of it. [`ModelResponse::calls`] reads whichever is in use.
    pub fn blocks(blocks: impl IntoIterator<Item = Block>) -> Self {
        let content = Content::blocks(blocks);
        let stop = match content
            .as_blocks()
            .is_some_and(|b| b.iter().any(|b| b.call().is_some()))
        {
            true => StopReason::ToolUse,
            false => StopReason::EndTurn,
        };

        Self {
            content: Some(content),
            reasoning: None,
            tool_calls: Vec::new(),
            stop,
            usage: None,
            raw: None,
        }
    }

    /// Returns the tools the model asked for, wherever they are recorded.
    ///
    /// note: the kernel reads this rather than the [`ModelResponse::tool_calls`] field, so that a
    /// provider which reports an ordered turn gets its calls gated, run and recorded like any
    /// other. See [`Message::calls`].
    pub fn calls(&self) -> impl Iterator<Item = &ToolCall> {
        let blocks = self.content.as_ref().and_then(Content::as_blocks);
        let flat = match blocks {
            Some(_) => None,
            None => Some(&self.tool_calls[..]),
        };

        flat.into_iter()
            .flatten()
            .chain(blocks.into_iter().flatten().filter_map(Block::call))
    }

    /// Returns what the model thought, wherever it is recorded.
    ///
    /// note: the same accessor as [`ContextItem::thinking`](crate::ContextItem::thinking), on the
    /// type a turn arrives as rather than the one it is kept as. A turn is recorded one of two
    /// ways - the [`ModelResponse::reasoning`] field, or a [`Block::Reasoning`] among ordered
    /// blocks - and which one a caller gets depends on the provider it is talking to. Reading the
    /// field directly is right on one dialect only; this reads both, as [`ModelResponse::calls`]
    /// does.
    pub fn thinking(&self) -> impl Iterator<Item = &Content> {
        let blocks = self.content.as_ref().and_then(Content::as_blocks);
        let flat = match blocks {
            Some(_) => None,
            None => self.reasoning.as_ref(),
        };

        flat.into_iter().chain(
            blocks
                .into_iter()
                .flatten()
                .filter_map(Block::thought)
                .map(|part| &part.content),
        )
    }
}

/// The identity and capabilities of the model behind a [`Provider`], as reported by it.
///
/// note: `#[non_exhaustive]`, so that what a provider can say about itself can grow without
/// breaking every implementation again. Build one with [`ModelInfo::new`] and the `with_`
/// methods; the fields stay public to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ModelInfo {
    /// The provider's name, e.g. `openrouter`.
    pub provider: String,
    /// The model's identifier, e.g. `qwen/qwen3-coder`.
    pub model: String,
    /// The model's context limit in tokens, if known.
    pub context_limit: Option<usize>,
    /// The maximum number of output tokens, if known.
    pub max_output_tokens: Option<usize>,
    /// Whether the model can call tools.
    pub tool_calling: bool,
    /// Whether the model exposes reasoning.
    pub reasoning: bool,
    /// The names of the [`Params`] this model accepts, where the provider publishes them.
    ///
    /// note: empty means "not published", never "takes none" - an endpoint that says nothing
    /// about its parameters is the common case, and a caller that read an empty list as a
    /// prohibition would be inventing a restriction the provider never stated. What it is for is
    /// the opposite mistake: a parameter set for a model that does not take it is accepted, sent
    /// and ignored in silence, and this is the only thing that can say so.
    ///
    /// note: `serde(default)`, so a record written before this existed still reads - as "not
    /// published", which is what it was.
    #[serde(default)]
    pub parameters: Vec<String>,
    /// Where the provider sends its requests, where it has an address to say: the base URL, with
    /// no credentials in it.
    ///
    /// note: part of what the kernel compares to announce [`Event::ModelChanged`](crate::Event),
    /// so a provider moved to another address with the same model name is a change in the record.
    /// Without it, a switch of address alone left a record saying the session never moved, and a
    /// session resumed from it had nothing to say where it had been talking.
    ///
    /// note: `None` for a provider with no address - a scripted one, or one in the same process -
    /// and for a record written before this existed, which reads as having said nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl ModelInfo {
    /// Creates a [`ModelInfo`] with the given provider and model names, no known limits, and
    /// no advertised capabilities.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            context_limit: None,
            max_output_tokens: None,
            tool_calling: false,
            reasoning: false,
            parameters: Vec::new(),
            endpoint: None,
        }
    }

    /// The same, with the model's context limit.
    pub fn with_context_limit(mut self, limit: impl Into<Option<usize>>) -> Self {
        self.context_limit = limit.into();
        self
    }

    /// The same, with the most it will write in one answer.
    pub fn with_max_output_tokens(mut self, max: impl Into<Option<usize>>) -> Self {
        self.max_output_tokens = max.into();
        self
    }

    /// The same, saying whether the model can call tools.
    pub fn with_tool_calling(mut self, tool_calling: bool) -> Self {
        self.tool_calling = tool_calling;
        self
    }

    /// The same, saying whether the model exposes its reasoning.
    pub fn with_reasoning(mut self, reasoning: bool) -> Self {
        self.reasoning = reasoning;
        self
    }

    /// The same, with the parameters the model is published to accept.
    pub fn with_parameters(
        mut self,
        parameters: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.parameters = parameters.into_iter().map(Into::into).collect();
        self
    }

    /// The same, with where the requests go; see [`ModelInfo::endpoint`].
    pub fn with_endpoint(mut self, endpoint: impl Into<Option<String>>) -> Self {
        self.endpoint = endpoint.into();
        self
    }
}

/// A source of model responses.
///
/// note: This is the entire model abstraction. The kernel knows a provider's [`ModelInfo`] and
/// how to ask it for a response; it has no notion of a privileged provider, no vendor-specific
/// branches, and no shared HTTP client to inherit assumptions from.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Returns the identity and capabilities of the model behind this provider.
    fn info(&self) -> ModelInfo;

    /// Returns the payload this provider would send for the given request, if it can show one.
    ///
    /// note: The kernel has no wire format of its own, so this is the only way the exact thing
    /// that goes to the model can be seen. What it is *not* is a guarantee: like a
    /// [`Tool`](crate::Tool)'s declared capabilities, it is the provider's own account of itself,
    /// and the kernel has nothing to check it against. Implement it by rendering the payload
    /// here and having [`Provider::respond`] send *that*, rather than building a second one -
    /// two code paths that are supposed to agree eventually do not, and a preview that has
    /// quietly stopped matching is worse than none.
    ///
    /// note: The body, not the transport. Headers, URLs and credentials are deliberately out of
    /// scope: an `Authorization` header is not something to put on an event stream.
    ///
    /// note: The default returns `None`, which is the honest answer for a provider that cannot
    /// render its request without sending it. [`Kernel::preview_payload`] then says so.
    fn render(&self, request: &ModelRequest) -> Option<Value> {
        let _ = request;

        None
    }

    /// Answers the given request.
    ///
    /// note: A streaming provider should report fragments through `deltas` as they arrive; the
    /// returned [`ModelResponse`] is still expected to be complete. Whether to stream at all is
    /// the provider's business, or the user's via [`Params`] - the kernel does not ask.
    ///
    /// note: Return `Err` for anything that is not an answer, and be suspicious of what counts.
    /// Real services report a rate limit or a dead upstream as an `error` object inside an
    /// otherwise successful `200`; a provider that only checks the status code hands the kernel an
    /// empty response, which it records as the model having said nothing.
    ///
    /// note: The kernel imposes no timeout, because it has no idea what a reasonable one is for
    /// your model - a reasoning model can take minutes. Give the transport its own. A caller can
    /// also drop the future driving [`Kernel::step`]: the kernel returns to
    /// [`State::Idle`](crate::State::Idle) and says so on the event stream.
    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError>;
}

#[cfg(test)]
mod tests {
    use super::{content::sized, *};

    /// A record written before `endpoint` existed still reads, and one with no address says
    /// nothing about one.
    ///
    /// note: every `model.changed` and `model.requested` carries a `ModelInfo`, so a field that a
    /// record without it could not be read past would be every session log from before it.
    #[test]
    fn a_model_with_no_address_reads_and_writes_as_it_did() {
        let old = r#"{"provider":"p","model":"m","context_limit":null,"max_output_tokens":null,"tool_calling":true,"reasoning":false}"#;
        let read: ModelInfo = serde_json::from_str(old).expect("an old record reads");
        assert_eq!(read.endpoint, None);
        assert!(
            !serde_json::to_string(&read)
                .expect("it writes")
                .contains("endpoint")
        );

        let at = ModelInfo::new("p", "m").with_endpoint("https://example.test/v1".to_owned());
        let written = serde_json::to_string(&at).expect("it writes");
        assert!(
            written.contains(r#""endpoint":"https://example.test/v1""#),
            "{written}"
        );
        assert_eq!(
            serde_json::from_str::<ModelInfo>(&written).expect("it reads"),
            at
        );
    }

    /// A prompt figure smaller than its own cache is the one shape that says out loud which
    /// convention an endpoint is speaking.
    #[test]
    fn a_prompt_smaller_than_its_own_cache_is_the_miss_and_is_read_as_one() {
        let exclusive = Usage {
            input_tokens: Some(1_000),
            cached_input_tokens: Some(9_000),
            ..Usage::default()
        };
        assert_eq!(exclusive.settled().input_tokens, Some(10_000));

        // and the dialect's own arrangement is left exactly as it is, cache and all
        let inclusive = Usage {
            input_tokens: Some(143_389),
            cached_input_tokens: Some(75_776),
            ..Usage::default()
        };
        assert_eq!(inclusive.settled(), inclusive);

        // which includes a prompt served wholly from the cache: that is possible in the dialect,
        // so it is not the shape that says the endpoint is speaking another one
        let all_cached = Usage {
            input_tokens: Some(1_000),
            cached_input_tokens: Some(1_000),
            ..Usage::default()
        };
        assert_eq!(all_cached.settled(), all_cached);

        // including when there is nothing to settle it against
        let alone = Usage {
            input_tokens: Some(1_000),
            ..Usage::default()
        };
        assert_eq!(alone.settled(), alone);
        assert_eq!(Usage::default().settled(), Usage::default());
    }

    /// A role is written down as the name the wire gives it.
    #[test]
    fn a_role_is_displayed_as_its_wire_name() {
        for role in [Role::System, Role::User, Role::Assistant, Role::Tool] {
            assert_eq!(role.to_string(), role.as_str());
        }
    }

    #[test]
    fn truncation_stays_inside_the_limit() {
        for limit in [0, 1, 10, 42, 43, 44, 100, 999] {
            let mut content = Content::text("x".repeat(1_000));
            let dropped = content
                .truncate_to(limit)
                .expect("1000 bytes is over every limit");

            assert!(
                content.byte_len() <= limit,
                "a limit of {limit} produced {} bytes",
                content.byte_len()
            );
            assert_eq!(
                dropped,
                1_000 - content.to_text().chars().filter(|c| *c == 'x').count(),
                "the number reported is the number dropped, at a limit of {limit}"
            );
        }
    }

    #[test]
    fn cloning_content_shares_it() {
        let big = Content::text("x".repeat(1 << 20));
        let copy = big.clone();

        let (Content::Text(a), Content::Text(b)) = (&big, &copy) else {
            unreachable!()
        };
        assert!(
            Arc::ptr_eq(a, b),
            "a context item is cloned on every state change and again into every request; \
             copying a megabyte each time would make pruning cost more the more there was to prune"
        );

        // and content made from a string that is already shared is that string
        let shared: Arc<str> = Arc::from("x".repeat(1 << 20));
        let Content::Text(held) = Content::from(shared.clone()) else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(&held, &shared));

        // and truncating one of them leaves the other whole, which is what lets the kernel keep
        // an untruncated tool output beside the truncated copy for nothing
        let mut copy = copy;
        copy.truncate_to(100);
        assert_eq!(big.byte_len(), 1 << 20);
        assert_eq!(copy.byte_len(), 100);
    }

    /// A size is written the way a person reads one, and stays a measurement while it does.
    #[test]
    fn a_size_is_written_in_the_unit_a_person_reads_it_in() {
        assert_eq!(sized(0), "0B");
        assert_eq!(sized(999), "999B");
        assert_eq!(sized(1_000), "1.00kB");
        assert_eq!(sized(292_468), "292.47kB");
        assert_eq!(sized(1_500_000), "1.50MB");
        assert_eq!(sized(2_000_000_000), "2.00GB");
        // nothing above the last unit, so a preposterous payload is still readable
        assert_eq!(sized(5_000_000_000_000), "5000.00GB");
    }

    /// A blob names itself wherever it has to be text, and is its base64 wherever it has to be
    /// a size.
    #[test]
    fn a_blob_names_itself_as_text_and_measures_as_what_goes_out() {
        let content = Content::blob("image/png", "aGVsbG8=");

        assert_eq!(content.to_text(), "[image/png, 8B]");
        assert_eq!(content.byte_len(), 8 + "image/png".len());
        assert_eq!(content.as_blob().map(|b| &*b.media_type), Some("image/png"));
        assert!(content.as_text().is_none(), "it is not text and says so");
    }

    /// A blob over an output limit becomes the sentence naming it, and the count is the payload.
    #[test]
    fn a_blob_that_is_over_a_limit_is_replaced_rather_than_cut() {
        let mut content = Content::blob("image/png", "A".repeat(4_000));
        let dropped = content.truncate_to(200).expect("it is over the limit");

        let said = content.to_text();
        assert!(
            content.as_blob().is_none(),
            "half a picture is not one: {said}"
        );
        assert!(said.starts_with("[image/png, 4.00kB]"), "{said}");
        assert!(said.contains("truncated by an output limit"), "{said}");
        assert!(
            dropped > 3_000,
            "what went is the payload, not the sentence: {dropped}"
        );
        assert!(content.byte_len() <= 200);
    }

    /// It survives a session log as the base64 string it already is.
    #[test]
    fn a_blob_round_trips_through_serde_as_the_string_it_already_is() {
        let content = Content::blob("image/png", "aGVsbG8=");

        let written = serde_json::to_string(&content).expect("a blob serializes");
        assert_eq!(
            written, r#"{"blob":{"media_type":"image/png","data":"aGVsbG8="}}"#,
            "the log carries the base64 and not an array of numbers"
        );
        assert_eq!(
            serde_json::from_str::<Content>(&written).expect("and comes back"),
            content
        );
    }

    #[test]
    fn truncation_leaves_short_content_alone() {
        let mut content = Content::text("hello");
        assert_eq!(content.truncate_to(5), None);
        assert_eq!(content.to_text(), "hello");
    }

    /// A cut lands on a character boundary, with room for the note and without it.
    ///
    /// note: on text where every index is a boundary neither walk down to one ever runs, so the
    /// text is `każdy`, which has an index inside a character every six bytes. At a limit of 64
    /// the note first fits beside a cut at 15, which is one of them. 21 is another, and too small
    /// for the note at any cut: it is the walk that spends the budget on content alone.
    #[test]
    fn truncation_does_not_split_a_character() {
        let mut content = Content::text("każdy".repeat(50));
        content.truncate_to(64).unwrap();
        // getting this far is what proves it: slicing inside a character would have panicked
        assert!(content.to_text().contains("truncated by an output limit"));
        assert!(content.byte_len() <= 64);

        let text = "każdy".repeat(50);
        assert!(!text.is_char_boundary(21));
        let (dropped, content) = within(move || {
            let mut content = Content::text(text);
            (content.truncate_to(21), content)
        })
        .expect("the walk down to a boundary ends");
        assert_eq!(content.to_text(), "każdykażdykażdyka");
        assert_eq!(dropped, Some(300 - 20));
    }

    /// Runs `f` on another thread, and answers `None` if it has not come back within five seconds.
    ///
    /// note: a walk that never ends is a suite that never finishes rather than a test that fails;
    /// bounding the wait is what turns one into the other. The thread is left to run rather than
    /// killed, and the process ends with the suite.
    fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let (sent, received) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sent.send(f());
        });
        received
            .recv_timeout(std::time::Duration::from_secs(5))
            .ok()
    }
}
