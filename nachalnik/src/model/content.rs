//! A message is made of [`Content`], the [`Blob`] that carries a picture or a document, and the
//! [`Part`]s and [`Block`]s an ordered turn is written in.

use std::{borrow::Cow, fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ToolCall, is_null, json_len, null};

#[cfg(doc)]
use super::{Message, ModelResponse, Provider};
#[cfg(doc)]
use crate::{Context, Kernel, Projector, TokenCounter};

/// A piece of content: plain text, structured data, or an ordered sequence of [`Block`]s.
///
/// note: The kernel does not interpret content; it counts it (via a [`TokenCounter`]), moves it
/// around, and hands it to a [`Provider`], which decides how a [`Content::Json`] payload is
/// rendered for its wire format.
///
/// note: Every variant is behind an [`Arc`], so cloning content is a refcount bump rather than a
/// copy. That is what makes the rest of the design affordable: an item is copied on every state
/// change (the undo snapshot keeps the old one) and again into a message on every request, so
/// copying a 4 MiB tool output each time would make excluding cost more the more there was to
/// exclude.
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
    /// note: in a dialect where the *order* is part of the message, an assistant turn is written
    /// this way - thinking, a sentence, a tool call, more thinking, another call. The order is here
    /// rather than on [`Message`] because content is the one thing a [`ModelResponse`], a
    /// [`ContextItem`](crate::ContextItem) and a [`Message`] all carry, so the order travels the
    /// whole way from the wire to the context and back. A field on `Message` would be a shape the
    /// context could not hold and no projector could project.
    ///
    /// note: an assistant turn is recorded *either* this way *or* the conventional way - content
    /// here, reasoning and calls in their own slots - and never both, so there is never a second
    /// account of the same turn. [`Message::calls`], [`ModelResponse::calls`] and
    /// [`ContextItem::calls`](crate::ContextItem::calls) read whichever is in use, and the kernel,
    /// the projector and a provider should use them.
    ///
    /// note: a [`Block`] holds a [`Content`], so this nests and nothing here stops it. Nothing
    /// produces a nested one (a turn is a flat sequence in every dialect) and everything in this
    /// crate treats it as flat. A sequence deep enough to matter would recurse through
    /// [`Content::to_text`] and through `Drop`, and that is on whoever hand-wrote the snapshot.
    Blocks(Arc<[Block]>),
    /// Bytes that are not text: an image, a document, a recording.
    ///
    /// note: the runtime does not look inside it. It carries it, counts it and hands it to a
    /// [`Provider`] like everything else here, and *names* it wherever it has to be turned into
    /// text, because a gap where a picture was is worse than a sentence saying there was one.
    Blob(Arc<Blob>),
}

/// Bytes that are not text, in the form every one of these APIs wants them.
///
/// note: the payload is already base64, because that is the form the wire takes in both dialects
/// this workspace speaks - a `data:` URI in one, `inline_data` in the other. Holding it this way
/// means nothing is encoded on the way out, [`Content::byte_len`] really is the size in the form
/// it would be sent in, and a session log is the base64 string and not a JSON array of a number
/// per byte. It also keeps a base64 codec out of a crate that has five dependencies and a rule
/// about growing a sixth.
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
    /// note: the kernel never reads this. Like [`ContextItem::meta`](crate::ContextItem::meta), it
    /// is a place for a fact the runtime does not interpret. The fact that matters here is what a
    /// [`TokenCounter`](crate::TokenCounter) needs to price a payload, which a byte length cannot
    /// reach: `{"w": 1024, "h": 768}` for a picture, `{"pages": 12}` for a document,
    /// `{"seconds": 184, "fps": 30}` for a recording. Each vendor prices from figures like these,
    /// and each differently, so the crate carries no formula, only the place to put its inputs.
    ///
    /// note: on the blob rather than on the item because a budget is counted over the *projected
    /// messages*, and a [`Message`] carries an item's [`Content`] and not its metadata. A fact left
    /// on `ContextItem::meta` reaches [`TokenCounter::count_item`](crate::TokenCounter::count_item)
    /// but never the figure a [`Compactor`](crate::Compactor) acts on.
    ///
    /// note: a counter is the reason this exists, not the only reader. A [`Provider`] may read it,
    /// and one does: the conventional dialect's attachment part needs a filename, so
    /// `nachalnik-providers` reads `name` here and derives one from the media type when it is
    /// unset. That is a convention between caller and provider; no key here has a meaning the
    /// kernel knows.
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

    /// How large the whole blob is on the wire: the payload and the media type.
    ///
    /// note: [`Blob::meta`] is not counted, because it is not sent. A dialect may read a key and
    /// derive a field of its own, as `nachalnik-providers` does with `name` for the attachment
    /// filename, but what a dialect wraps around the payload is the dialect's to count, as is the
    /// envelope of the message carrying the blob.
    pub fn wire_len(&self) -> usize {
        self.byte_len() + self.media_type.len()
    }
}

impl fmt::Display for Blob {
    /// The blob as text, for the places something has to be text.
    ///
    /// note: bracketed, as `nachalnik-mcp` names a tool result it could not carry, and giving the
    /// two facts useful in its place: what it was, and how much of it there was.
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
pub(super) fn sized(bytes: usize) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];

    let mut size = bytes as f64;
    let mut unit = 0;
    // what two decimals round to a thousand of is one of the next unit, not `1000.00kB`
    while size >= 999.995 && unit + 1 < UNITS.len() {
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
    /// note: for [`Content::Blocks`] this is what the turn said - the text blocks joined with a
    /// newline - not what it costs to send. Thinking and tool calls are left out: a provider that
    /// put them in a `content` field would send the model its own reasoning as if it had said it
    /// out loud. [`Content::byte_len`] counts all of them. The newline is there because two text
    /// blocks were separated by something - a call, a thought - and running them together would
    /// form a word that was never in the output.
    ///
    /// note: for [`Content::Blob`] there is no faithful answer, so this names it:
    /// `[image/png, 12.05kB]`. An empty string would make a picture vanish from a transcript with
    /// nothing to say it was there. Anything that wants the payload asks [`Content::as_blob`].
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
    /// note: this is the seam a [`TokenCounter`](crate::TokenCounter) needs and could not build for
    /// itself, because of the nesting. A turn that is a sentence and a screenshot is a
    /// `Blocks` holding a `Blob` one level down, which is the shape both dialects send - so a
    /// counter matching only on `Content::Blob` sees a plain picture and misses every picture a
    /// model was actually shown.
    ///
    /// note: it allocates only when there is something to put in the vector, which is what makes it
    /// affordable on a path that runs for every item on every recount.
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
        let (truncated, dropped) = cut(&self.to_text(), limit, whole);
        *self = Self::Text(truncated.into());

        Some(dropped)
    }

    /// Truncates as [`Content::truncate_to`] does, except that a picture of at most `pictures`
    /// bytes is kept whole and not measured against `limit`: only the text around it is cut.
    ///
    /// note: for a tool that returns a picture beside some text, as an MCP server's screenshot
    /// tool does. Measured with the rest, a picture is over any limit a tool's text is held to, and
    /// [`Content::truncate_to`] turns the whole of it into a sentence naming it - so the model is
    /// never shown the one thing it asked for. A picture is not text the model reads by the byte,
    /// and [`TokenCounter`] does not price it by its length either; what bounds it is `pictures`,
    /// which a caller sets to what the APIs it talks to take. One over that is named, as before.
    ///
    /// note: only a picture that is the content, or a text block of a [`Content::Blocks`], is kept,
    /// which is how a tool result carries one. Where something other than plain text would have to
    /// be cut to fit, the whole is truncated as [`Content::truncate_to`] does it, because cutting a
    /// turn's calls or its reasoning is not cutting text.
    pub fn truncate_around_pictures(&mut self, limit: usize, pictures: usize) -> Option<usize> {
        let kept = |block: &Block| match block {
            Block::Text(part) => {
                matches!(&part.content, Self::Blob(blob) if blob.wire_len() <= pictures)
            }
            _ => false,
        };
        let blocks = match self {
            Self::Blob(blob) if blob.wire_len() <= pictures => return None,
            Self::Blocks(blocks) if blocks.iter().any(kept) => blocks.clone(),
            _ => return self.truncate_to(limit),
        };

        let measured: usize = blocks
            .iter()
            .filter(|it| !kept(it))
            .map(Block::byte_len)
            .sum();
        if measured <= limit {
            return None;
        }
        let plain = |block: &Block| match block {
            Block::Text(part) => matches!(part.content, Self::Text(_) | Self::Json(_)),
            _ => false,
        };
        if !blocks.iter().all(|it| kept(it) || plain(it)) {
            return self.truncate_to(limit);
        }

        // the text before the first block that does not fit goes as it is; that block is cut, with
        // the note counting everything that went, and the text after it goes with it. The
        // pictures stay where they were
        let mut shown = Vec::with_capacity(blocks.len());
        let (mut room, mut dropped) = (limit, None);
        for (at, block) in blocks.iter().enumerate() {
            if kept(block) {
                shown.push(block.clone());
            } else if dropped.is_none() && block.byte_len() <= room {
                room -= block.byte_len();
                shown.push(block.clone());
            } else if dropped.is_none() {
                let after: usize = blocks[at + 1..]
                    .iter()
                    .filter(|it| !kept(it))
                    .map(Block::byte_len)
                    .sum();
                let Block::Text(part) = block else {
                    unreachable!("only plain text blocks are left")
                };
                let (truncated, went) =
                    cut(&part.content.to_text(), room, block.byte_len() + after);
                if !truncated.is_empty() {
                    shown.push(Block::text(truncated));
                }
                dropped = Some(went);
            }
        }
        *self = Self::Blocks(shown.into());

        dropped
    }
}

/// Cuts `text` to at most `limit` bytes with a note saying how much went, where `whole` is what it
/// stood for - so what went is `whole` less what is kept, which counts what was never in `text`.
///
/// note: the note's own length depends on the number it reports, so a cut that fits is settled
/// before committing to one. The limit is a measure of `byte_len` and the cut is an index into
/// `to_text`, and for a blob or a turn of blocks those are two different strings - a 4 MB picture
/// is over any limit and names itself in nineteen characters. Starting at the limit would walk down
/// a character boundary at a time from a number the text never reaches.
fn cut(text: &str, limit: usize, whole: usize) -> (String, usize) {
    let mut cut = limit.min(text.len());
    loop {
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let dropped = whole - cut;
        let note = format!("\n[... {dropped} bytes truncated by an output limit ...]");

        if cut + note.len() <= limit {
            return (format!("{}{note}", &text[..cut]), dropped);
        }
        if cut == 0 {
            // there is no room for the note at all, so spend the budget on content instead
            let mut cut = limit.min(text.len());
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            return (text[..cut].to_owned(), whole - cut);
        }
        cut -= 1;
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

/// One piece of an assistant turn, in the order the model produced it.
///
/// note: there are three variants because a turn interleaves what the model thought, what it said,
/// and what it asked for. Some dialects keep that order: Anthropic's content blocks, Gemini's
/// `parts`, and a reasoning model that thinks again between two calls. A message with one content
/// slot, one reasoning slot, and a flat list of calls cannot express that order, no matter how it
/// is projected. The order carries information; this enum stores it.
///
/// note: this adds no new state. A [`Block::Call`] is the same [`ToolCall`] a conventional turn
/// carries, with the same `extra`. A [`Part`] is that `extra` for the two variants that are not
/// calls. What is new is the ordered list.
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
    /// note: it does not return [`Block::Reasoning`], which is what the model did not say out loud.
    /// This distinction keeps [`Content::to_text`] from sending a provider the model's own thinking
    /// as content.
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
