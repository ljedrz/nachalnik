use std::{borrow::Cow, fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[cfg(doc)]
use crate::{Config, Context, Kernel, Projector, TokenCounter};
use crate::{error::BoxError, event::DeltaSink, tool::ToolSpec};

/// A piece of content: plain text, structured data, or an ordered sequence of [`Block`]s.
///
/// note: The kernel does not interpret content; it counts it (via a [`TokenCounter`]), moves it
/// around, and hands it to a [`Provider`], which decides how a [`Content::Json`] payload is
/// rendered for its wire format.
///
/// note: Every variant is behind an [`Arc`], so cloning content is a refcount bump rather than
/// a copy. This is not an optimisation detail so much as what makes the rest of the design
/// affordable: a context item is copied on every state change (the undo snapshot holding the old
/// one is the point of undo) and again into a message on every request, and a 4 MiB tool output
/// that were copied each time would make pruning - the thing this crate is *for* - cost more the
/// more there was to prune.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Content {
    /// Plain text.
    Text(Arc<str>),
    /// Structured data.
    Json(Arc<Value>),
    /// An ordered sequence of typed [`Block`]s.
    ///
    /// note: this is what an assistant turn is in a dialect where the *order* is part of the
    /// message - thinking, a sentence, a tool call, more thinking, another call - and it is here
    /// rather than as a field on [`Message`] for one reason: content is the one thing a
    /// [`ModelResponse`], a [`ContextItem`](crate::ContextItem) and a [`Message`] all carry, so
    /// putting the order in it carries the order the whole way from the wire to the context and
    /// back out again. A field on `Message` would have been a shape the context could not hold,
    /// and a context that cannot hold it is a context no projector can project it out of.
    ///
    /// note: an assistant turn is recorded *either* this way *or* the conventional way - content
    /// here, reasoning and calls in their own slots - and never both, so there is never a second
    /// account of the same turn to disagree with the first. [`Message::calls`],
    /// [`ModelResponse::calls`] and [`ContextItem::calls`](crate::ContextItem::calls) read
    /// whichever one is in use, and are what the kernel, the projector and a provider should
    /// reach for.
    ///
    /// note: a [`Block`] holds a [`Content`], so this nests, and nothing here stops it. Nothing
    /// produces a nested one either - a turn is a flat sequence in every dialect there is - and
    /// treating it as flat is what everything in this crate does. It is written down because a
    /// sequence deep enough to matter would be recursing through [`Content::to_text`] and through
    /// `Drop`, and somebody hand-writing a snapshot should know that is on them.
    Blocks(Arc<[Block]>),
}

impl Content {
    /// Creates plain text.
    pub fn text(text: impl Into<Arc<str>>) -> Self {
        Self::Text(text.into())
    }

    /// Creates structured data.
    pub fn json(value: Value) -> Self {
        Self::Json(Arc::new(value))
    }

    /// Creates an ordered sequence of blocks.
    pub fn blocks(blocks: impl IntoIterator<Item = Block>) -> Self {
        Self::Blocks(blocks.into_iter().collect())
    }

    /// Returns the text, if this is [`Content::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            Self::Json(_) | Self::Blocks(_) => None,
        }
    }

    /// Returns the blocks, if this is [`Content::Blocks`].
    pub fn as_blocks(&self) -> Option<&[Block]> {
        match self {
            Self::Blocks(blocks) => Some(blocks),
            Self::Text(_) | Self::Json(_) => None,
        }
    }

    /// Returns the content as text, serializing it first if it is [`Content::Json`].
    ///
    /// note: for [`Content::Blocks`] this is what the turn *said* - the text blocks, joined with
    /// a newline - and not what it costs to send: the thinking and the tool calls are not text
    /// the model uttered, and a provider that put them in a `content` field would be sending the
    /// model its own reasoning as if it had said it out loud. [`Content::byte_len`] is the other
    /// question, and it counts all of them. The newline is there because two text blocks were
    /// separated by *something* - a call, a thought - and running them together would make a word
    /// that was never in the output.
    pub fn to_text(&self) -> Cow<'_, str> {
        match self {
            Self::Text(s) => Cow::Borrowed(s),
            Self::Json(v) => Cow::Owned(v.to_string()),
            Self::Blocks(blocks) => match blocks.iter().filter_map(Block::said).collect::<Vec<_>>()
            {
                said if said.len() == 1 => said[0].content.to_text(),
                said => Cow::Owned(
                    said.iter()
                        .map(|part| part.content.to_text())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
            },
        }
    }

    /// Returns the size of the content in bytes, in the form it would be sent in.
    ///
    /// note: nothing is built to measure it - a [`Content::Json`] payload is walked and its
    /// bytes counted as they are written; see `json_len`.
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Text(s) => s.len(),
            Self::Json(v) => json_len(v),
            // all of them, including the thinking and the calls: what this answers is what the
            // turn costs, which is not what `to_text` answers
            Self::Blocks(blocks) => blocks.iter().map(Block::byte_len).sum(),
        }
    }

    /// Truncates the content to at most `limit` bytes, appending a note stating how much was
    /// dropped; returns the number of bytes dropped, or `None` if nothing was.
    ///
    /// note: the note names no crate and no program. What reads it is a model, which has never
    /// heard of either, and the useful facts are that something was cut and that there is more
    /// where it came from - both of which tell it to ask for less next time.
    ///
    /// note: The note counts against the limit, so the result really is at most `limit` bytes -
    /// a limit that is not one would be a poor foundation for a budget. Where the note alone
    /// does not fit, the content is cut to the limit without one, and the truncation is reported
    /// by the return value and by [`Event::ToolFinished`](crate::Event::ToolFinished) as usual.
    ///
    /// note: Truncating [`Content::Json`] or [`Content::Blocks`] turns it into
    /// [`Content::Text`] - a truncated JSON document is not JSON and a cut string is not an
    /// ordered sequence of blocks, and pretending otherwise would hide the truncation.
    ///
    /// note: what has to fit is [`Content::byte_len`] and what is cut is [`Content::to_text`],
    /// which are the same string for text and for JSON and are not for blocks: a turn whose
    /// words fit but whose tool calls do not is over the limit, and the number reported counts
    /// everything that went.
    pub fn truncate_to(&mut self, limit: usize) -> Option<usize> {
        let whole = self.byte_len();
        if whole <= limit {
            return None;
        }
        let text = self.to_text();

        // the note's own length depends on the number it reports, so settle on a cut that fits
        // before committing to one
        let mut cut = limit;
        let (truncated, dropped) = loop {
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            let dropped = whole - cut;
            let note = format!("\n[... {dropped} bytes truncated by an output limit ...]");

            if cut + note.len() <= limit {
                break (format!("{}{note}", &text[..cut]), dropped);
            }
            if cut == 0 {
                // there is no room for the note at all, so spend the budget on content instead
                let mut cut = limit;
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                break (text[..cut].to_owned(), whole - cut);
            }
            cut -= 1;
        };

        *self = Self::Text(truncated.into());

        Some(dropped)
    }
}

impl Default for Content {
    /// Returns empty text.
    fn default() -> Self {
        Self::Text("".into())
    }
}

impl From<String> for Content {
    fn from(s: String) -> Self {
        Self::Text(s.into())
    }
}

impl From<&str> for Content {
    fn from(s: &str) -> Self {
        Self::Text(s.into())
    }
}

impl From<Arc<str>> for Content {
    fn from(s: Arc<str>) -> Self {
        Self::Text(s)
    }
}

impl From<Value> for Content {
    fn from(v: Value) -> Self {
        Self::Json(Arc::new(v))
    }
}

impl fmt::Display for Content {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_text())
    }
}

/// Something the model produced, and whatever the provider attached to it.
///
/// note: this is [`ToolCall::extra`] for the parts of a turn that are not calls, and it exists
/// for the same reason. Some APIs sign each piece of an assistant turn rather than the turn as a
/// whole - Gemini's `thoughtSignature` rides on a text part as readily as on a call - and a
/// request that returns one altered is rejected or answered worse. The kernel never looks inside
/// it, never separates it from the block it belongs to, and a provider that has nothing to attach
/// pays a null.
///
/// note: bound to the block rather than kept in a list beside it, so that whatever removes the
/// block removes the signature of the thing that is no longer there. An elided turn is the case
/// that makes it matter: the marker replacing the words is not the words, and it must not go out
/// signed as if it were.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    /// What the model produced.
    pub content: Content,
    /// Whatever the provider attached to it, carried verbatim.
    #[serde(default = "null", skip_serializing_if = "is_null")]
    pub extra: Arc<Value>,
}

impl Part {
    /// Creates a part with nothing attached to it.
    pub fn new(content: impl Into<Content>) -> Self {
        Self {
            content: content.into(),
            extra: Arc::new(Value::Null),
        }
    }

    /// Attaches the provider's own opaque state to the part.
    pub fn with_extra(mut self, extra: impl Into<Arc<Value>>) -> Self {
        self.extra = extra.into();
        self
    }

    /// Returns the size of the part in bytes, in the form it would be sent in.
    pub fn byte_len(&self) -> usize {
        let extra = match self.extra.is_null() {
            true => 0,
            false => json_len(&self.extra),
        };

        self.content.byte_len() + extra
    }
}

impl<C: Into<Content>> From<C> for Part {
    fn from(content: C) -> Self {
        Self::new(content)
    }
}

/// One piece of an assistant turn, in the position the model produced it in.
///
/// note: three variants, because three things interleave in a turn: what the model thought, what
/// it said, and what it asked for. A dialect that keeps their order - Anthropic's content blocks,
/// Gemini's `parts`, a reasoning model that thinks again between two calls - cannot be expressed
/// by a message with one content slot, one reasoning slot and a flat list of calls, however
/// cleverly it is projected. The order is itself the information, and this is where it goes.
///
/// note: nothing here is new *state*. A [`Block::Call`] is the same [`ToolCall`] a conventional
/// turn carries, with the same `extra`; a [`Part`] is that `extra` for the two variants that are
/// not calls. What is new is that they are in a list, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Block {
    /// Something the model said.
    Text(Part),
    /// Something the model thought.
    Reasoning(Part),
    /// A tool the model asked for, where it asked for it.
    Call(ToolCall),
}

impl Block {
    /// Creates a text block with nothing attached to it.
    pub fn text(content: impl Into<Content>) -> Self {
        Self::Text(Part::new(content))
    }

    /// Creates a thinking block with nothing attached to it.
    pub fn reasoning(content: impl Into<Content>) -> Self {
        Self::Reasoning(Part::new(content))
    }

    /// Returns the name of the block, as used in reports.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Reasoning(_) => "reasoning",
            Self::Call(_) => "call",
        }
    }

    /// Returns the call, if this is [`Block::Call`].
    pub fn call(&self) -> Option<&ToolCall> {
        match self {
            Self::Call(call) => Some(call),
            _ => None,
        }
    }

    /// Returns what the model uttered, if this is [`Block::Text`].
    ///
    /// note: not [`Block::Reasoning`], which is the thing the model did *not* say out loud. The
    /// difference is what keeps [`Content::to_text`] from handing a provider the model's own
    /// thinking to send back as content.
    pub fn said(&self) -> Option<&Part> {
        match self {
            Self::Text(part) => Some(part),
            _ => None,
        }
    }

    /// Returns the model's thinking, if this is [`Block::Reasoning`].
    pub fn thought(&self) -> Option<&Part> {
        match self {
            Self::Reasoning(part) => Some(part),
            _ => None,
        }
    }

    /// Returns the part, for either of the two variants that are not a call.
    pub fn part(&self) -> Option<&Part> {
        match self {
            Self::Text(part) | Self::Reasoning(part) => Some(part),
            Self::Call(_) => None,
        }
    }

    /// Returns whatever the provider attached to this block, wherever it keeps it.
    pub fn extra(&self) -> &Arc<Value> {
        match self {
            Self::Text(part) | Self::Reasoning(part) => &part.extra,
            Self::Call(call) => &call.extra,
        }
    }

    /// Returns the size of the block in bytes, in the form it would be sent in.
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Text(part) | Self::Reasoning(part) => part.byte_len(),
            Self::Call(call) => call.byte_len(),
        }
    }
}

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
/// note: which of the two a request carries is [`LinearProjector::send_blocks`], and that really
/// is a decision a [`Projector`] gets to make now - a dialect that puts tool results inside a
/// user turn, or keeps thinking-only turns, or flattens everything into one string, or wants the
/// order, is a projector away. What a projector still cannot do is invent an order that was never
/// recorded: a turn that arrived through a provider speaking the three-slot dialect has no order
/// to carry, and flattening one that does is lossy and says so in [`Projection::repairs`].
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
    /// APIs hand back a piece of opaque state per call - Google's `thought_signature`, an
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
    /// the model wrote the arguments, and they go out on every request that follows. This is the
    /// figure [`TokenCounter::count_item`](crate::TokenCounter::count_item) has always added on
    /// top of the content, said once so that a [`Block::Call`] can be measured the same way.
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
    /// Tokens in the response.
    pub output_tokens: Option<u64>,
    /// Tokens spent on reasoning, where reported separately.
    pub reasoning_tokens: Option<u64>,
    /// Request tokens that were served from the provider's cache.
    pub cached_input_tokens: Option<u64>,
}

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
}

/// The identity and capabilities of the model behind a [`Provider`], as reported by it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        }
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
    /// otherwise successful `200`; a provider that only checks the status code will hand the
    /// kernel an empty response, which it will faithfully record as the model having said
    /// nothing. Both of this crate's example providers had to learn that the hard way.
    ///
    /// note: The kernel imposes no timeout, because it has no idea what a reasonable one is for
    /// your model - a reasoning model can take minutes. Give the transport its own timeout. A
    /// caller can also simply drop the future driving [`Kernel::step`]: the kernel returns to
    /// [`State::Idle`](crate::State::Idle) and says so on the event stream.
    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError>;
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // and truncating one of them leaves the other whole, which is what lets the kernel keep
        // an untruncated tool output beside the shortened copy for nothing
        let mut copy = copy;
        copy.truncate_to(100);
        assert_eq!(big.byte_len(), 1 << 20);
        assert_eq!(copy.byte_len(), 100);
    }

    #[test]
    fn truncation_leaves_short_content_alone() {
        let mut content = Content::text("hello");
        assert_eq!(content.truncate_to(5), None);
        assert_eq!(content.to_text(), "hello");
    }

    #[test]
    fn truncation_does_not_split_a_character() {
        let mut content = Content::text("każdy".repeat(50));
        content.truncate_to(60).unwrap();
        // the round trip is what proves it: invalid UTF-8 would not have got this far
        assert!(content.to_text().contains("truncated by an output limit"));
        assert!(content.byte_len() <= 60);
    }
}
