//! The vocabulary both halves agree on: what a request carries, what a response brings back, and
//! the [`Provider`] that turns one into the other.
//!
//! note: nothing in here knows about HTTP, JSON schemas or any particular vendor. It is the shape
//! the kernel builds and a provider renders, which is what lets a dialect whose assistant turn is
//! an ordered list of parts and one whose turn is a content slot beside a list of calls sit behind
//! the same trait.

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
    /// Bytes that are not text: an image, a document, a recording.
    ///
    /// note: the runtime does not look inside it. What it does is carry it, count it and hand it
    /// to a [`Provider`], the same as everything else here - and *name* it wherever it has to be
    /// turned into text, because a gap where a picture was is worse than a sentence saying there
    /// was one.
    Blob(Arc<Blob>),
}

/// Bytes that are not text, in the form every one of these APIs wants them.
///
/// note: the payload is already base64, and that is deliberate rather than lazy. It is the form
/// the wire takes in both dialects this workspace speaks - a `data:` URI in one, `inline_data` in
/// the other - so holding it this way means nothing is encoded on the way out, [`Content::byte_len`]
/// really is the size in the form it would be sent in, and a session log is the base64 string and
/// not a JSON array of six hundred thousand numbers. It also keeps a base64 codec out of a crate
/// that has five dependencies and a rule about growing a sixth.
///
/// note: nothing here validates it. A caller that hands over a string which is not base64 has
/// built a request the endpoint will refuse, and it will say so; a runtime that checked would be
/// deciding what a media type means, which is the thing this crate does not do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Blob {
    /// What the payload is, as an IANA media type: `image/png`, `application/pdf`.
    pub media_type: Arc<str>,
    /// The payload, base64-encoded.
    pub data: Arc<str>,
    /// Whatever the producer knows about the payload that a byte length does not say.
    ///
    /// note: The kernel never reads this, and the bargain is the one
    /// [`ContextItem::meta`](crate::ContextItem::meta) already strikes: somewhere to put a fact
    /// the runtime has no business having an opinion about. Here the fact that matters is what a
    /// [`TokenCounter`](crate::TokenCounter) would need to price a payload, because a byte length
    /// cannot reach it - `{"w": 1024, "h": 768}` for a picture, `{"pages": 12}` for a document,
    /// `{"seconds": 184, "fps": 30}` for a recording. Every vendor's formula is over figures like
    /// those and each vendor's is different, so the crate carries none of them and carries the
    /// place to put the inputs instead.
    ///
    /// note: on the blob rather than on the item, which is the whole reason it is a field here.
    /// A budget is counted over the *projected messages*, and a [`Message`] carries a [`Content`]
    /// and nothing else a counter could read - so a fact left on `ContextItem::meta` reaches
    /// [`TokenCounter::count_item`](crate::TokenCounter::count_item) and never reaches the figure
    /// a [`Compactor`](crate::Compactor) acts on.
    ///
    /// note: whoever produced the base64 had the payload decoded a moment earlier, which is why
    /// this costs a caller nothing to fill in and is the only place that knows.
    ///
    /// note: a counter is the reason this exists and not the only thing entitled to read it. A
    /// [`Provider`] may too, and one does: the conventional dialect's attachment part will not go
    /// out without a filename, so `nachalnik-providers` reads `name` here and derives one from the
    /// media type when nobody set it. That is a convention between a caller and a provider rather
    /// than anything this crate enforces - there is no key here the kernel knows the meaning of,
    /// which is the entire point of the field.
    #[serde(default = "null", skip_serializing_if = "is_null")]
    pub meta: Arc<Value>,
}

impl Blob {
    /// Creates a blob from a media type and an already-base64 payload, with nothing known about
    /// it beyond those two.
    pub fn new(media_type: impl Into<Arc<str>>, data: impl Into<Arc<str>>) -> Self {
        Self {
            media_type: media_type.into(),
            data: data.into(),
            meta: null(),
        }
    }

    /// Attaches what the producer knows about the payload; see [`Blob::meta`].
    pub fn with_meta(mut self, meta: impl Into<Arc<Value>>) -> Self {
        self.meta = meta.into();
        self
    }

    /// How large the payload is, as base64 - which is what goes on the wire.
    pub fn byte_len(&self) -> usize {
        self.data.len()
    }

    /// How large the whole blob is on the wire: the payload and the media type naming it.
    ///
    /// note: [`Blob::meta`] is not in it, because meta does not go on the wire at all - it is for
    /// whoever is counting, and a provider never sees it.
    pub fn wire_len(&self) -> usize {
        self.byte_len() + self.media_type.len()
    }
}

impl fmt::Display for Blob {
    /// Names it, for the places something has to be text.
    ///
    /// note: the same shape `nachalnik-mcp` has always used for a tool result it could not carry,
    /// because the useful facts are the same two: what it was, and how much of it there was.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}, {}]", self.media_type, sized(self.byte_len()))
    }
}

/// A byte count in the unit a person reads it in, rather than as a row of digits.
///
/// note: `292.47kB` and not `292468 bytes`. This string lands in a terminal's narrowest column,
/// in a model's context, and in a sentence standing where a payload was - and in all three the
/// digits past the third are noise. Two decimals keep it a *measurement*: `1.05MB` and `1.10MB`
/// are different files, where `1MB` and `1MB` are not.
///
/// note: thousands, not 1024, and `kB` rather than `KiB`. What these figures get compared against
/// is an API's documented limit, and those are quoted in the decimal unit.
fn sized(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];

    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit + 1 < UNITS.len() {
        size /= 1000.0;
        unit += 1;
    }

    match unit {
        // no decimals on a count of bytes, which is already exact
        0 => format!("{bytes}B"),
        _ => format!("{size:.2}{}", UNITS[unit]),
    }
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

    /// Creates a blob from a media type and an already-base64 payload.
    pub fn blob(media_type: impl Into<Arc<str>>, data: impl Into<Arc<str>>) -> Self {
        Self::Blob(Arc::new(Blob::new(media_type, data)))
    }

    /// Returns the text, if this is [`Content::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            Self::Json(_) | Self::Blocks(_) | Self::Blob(_) => None,
        }
    }

    /// Returns the blocks, if this is [`Content::Blocks`].
    pub fn as_blocks(&self) -> Option<&[Block]> {
        match self {
            Self::Blocks(blocks) => Some(blocks),
            Self::Text(_) | Self::Json(_) | Self::Blob(_) => None,
        }
    }

    /// Returns the blob, if this is [`Content::Blob`].
    pub fn as_blob(&self) -> Option<&Blob> {
        match self {
            Self::Blob(blob) => Some(blob),
            Self::Text(_) | Self::Json(_) | Self::Blocks(_) => None,
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
    ///
    /// note: for [`Content::Blob`] there is no faithful answer, so this names it rather than
    /// giving one - `[image/png, 12.05kB]`. The alternatives were an empty string, which
    /// makes a picture vanish from a transcript with nothing to say it was ever there, and the
    /// base64 itself, which is six hundred thousand characters of noise wherever anything expects
    /// prose. Anything that wants the payload asks [`Content::as_blob`] for it.
    pub fn to_text(&self) -> Cow<'_, str> {
        match self {
            Self::Text(s) => Cow::Borrowed(s),
            Self::Json(v) => Cow::Owned(v.to_string()),
            Self::Blob(blob) => Cow::Owned(blob.to_string()),
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
            // the base64 and the media type, because both go out and neither is free. It is not
            // what the *model* charges for a picture - that is a count no byte length can reach,
            // and see `TokenCounter` for what this crate does and does not claim about it
            Self::Blob(blob) => blob.wire_len(),
        }
    }

    /// Collects every [`Blob`] in the content, including any nested in a [`Content::Blocks`]
    /// turn.
    ///
    /// note: this is the seam a [`TokenCounter`](crate::TokenCounter) needs and could not build
    /// for itself, and the nesting is the whole reason. A turn that is a sentence and a
    /// screenshot is a `Blocks` holding a `Blob` one level down, which is the shape both dialects
    /// send - so a counter matching only on `Content::Blob` sees a plain picture and misses every
    /// picture a model was actually shown. `BytesPerToken` made exactly that mistake: a bare blob
    /// counted `0` and the same blob inside a turn counted its base64 at four bytes a token, so a
    /// 400 KB screenshot went from free to a hundred thousand tokens depending on which shape it
    /// arrived in.
    ///
    /// note: it allocates only when there is something to put in the vector, which is what makes
    /// it affordable on a path that runs for every item on every recount: text and JSON return an
    /// empty `Vec` without touching the allocator.
    pub fn blobs(&self) -> Vec<&Blob> {
        let mut found = Vec::new();
        self.collect_blobs(&mut found);

        found
    }

    fn collect_blobs<'a>(&'a self, found: &mut Vec<&'a Blob>) {
        match self {
            Self::Blob(blob) => found.push(blob),
            Self::Blocks(blocks) => {
                for block in blocks.iter() {
                    // a call is a name and its arguments, and both are JSON: there is nowhere in
                    // one for a blob to be
                    if let Block::Text(part) | Block::Reasoning(part) = block {
                        part.content.collect_blobs(found);
                    }
                }
            }
            Self::Text(_) | Self::Json(_) => {}
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
    /// note: Truncating [`Content::Json`], [`Content::Blocks`] or [`Content::Blob`] turns it into
    /// [`Content::Text`] - a truncated JSON document is not JSON, a cut string is not an ordered
    /// sequence of blocks, and half a PNG is not a picture. Pretending otherwise would hide the
    /// truncation. A blob over the limit therefore arrives as the sentence naming it and the
    /// count of what went, which is the whole payload.
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

    /// Returns what the model thought, wherever it is recorded.
    ///
    /// note: the same accessor [`ContextItem::thinking`](crate::ContextItem::thinking) has had all
    /// along, on the type a turn arrives as rather than the one it is kept as. A turn is recorded
    /// one of two ways - the [`ModelResponse::reasoning`] field, or a [`Block::Reasoning`] among
    /// ordered blocks - and which one a caller gets is a property of whichever provider it happens
    /// to be talking to. [`ModelResponse::calls`] has read both since ordered turns existed, and a
    /// context item reads both; a `ModelResponse` in hand was the one place left where asking what
    /// the model thought meant reading a field and being right on one dialect only.
    ///
    /// note: an iterator of [`Content`], because that is what the two accessors beside it are.
    /// One shape per question: a provider writing a conformance case, a client showing somebody a
    /// turn, and a kernel recording one should not each need a different call.
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
    /// The names of the [`Params`] this model accepts, where the provider publishes them.
    ///
    /// note: empty means "not published", never "takes none" - an endpoint that says nothing
    /// about its parameters is the common case, and a caller that read an empty list as a
    /// prohibition would be inventing a restriction the provider never stated. What it is for is
    /// the opposite mistake: a parameter set for a model that does not take it is accepted, sent
    /// and ignored in silence, and this is the only thing that can say so.
    pub parameters: Vec<String>,
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
        // an untruncated tool output beside the truncated copy for nothing
        let mut copy = copy;
        copy.truncate_to(100);
        assert_eq!(big.byte_len(), 1 << 20);
        assert_eq!(copy.byte_len(), 100);
    }

    /// A size is written the way a person reads one, and stays a measurement while it does.
    ///
    /// note: the boundaries are the whole of it. Under a thousand it is a count and gets no
    /// decimals; at a thousand it changes unit; and two decimals are kept because one file
    /// against another is the comparison these figures exist for - `1.05MB` and `1.10MB` are
    /// different, where `1MB` and `1MB` are not.
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
    ///
    /// note: the two questions get different answers on purpose. `to_text` is asked by anything
    /// that has to *show* the content - a transcript, a truncation, a dialect with nowhere to put
    /// it - and there is no faithful text for a picture, so it says what was there. `byte_len` is
    /// asked by an output limit, and what the limit is protecting is the request, which carries
    /// the base64.
    #[test]
    fn a_blob_names_itself_as_text_and_measures_as_what_goes_out() {
        let content = Content::blob("image/png", "aGVsbG8=");

        assert_eq!(content.to_text(), "[image/png, 8B]");
        assert_eq!(content.byte_len(), 8 + "image/png".len());
        assert_eq!(content.as_blob().map(|b| &*b.media_type), Some("image/png"));
        assert!(content.as_text().is_none(), "it is not text and says so");
    }

    /// A blob over an output limit becomes the sentence naming it, and the count is the payload.
    ///
    /// note: the alternative is half a PNG, which is not a picture and is not detectable as not
    /// being one. `truncate_to` already turns JSON and blocks into text for the same reason.
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

    /// It survives a session log, which is the whole reason the payload is held as base64.
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

    #[test]
    fn truncation_does_not_split_a_character() {
        let mut content = Content::text("każdy".repeat(50));
        content.truncate_to(60).unwrap();
        // the round trip is what proves it: invalid UTF-8 would not have got this far
        assert!(content.to_text().contains("truncated by an output limit"));
        assert!(content.byte_len() <= 60);
    }
}
