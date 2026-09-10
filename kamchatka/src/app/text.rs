//! Turning what the runtime hands out into what a person reads.
//!
//! note: every function in here takes a value the kernel made and returns a `String`. None of
//! them touch [`super::App`], which is what makes them the part of the screen that can be read
//! without knowing what a tab is.

use nachalnik::{
    Block, Content, ContextId, ContextItem, ContextKind, Event, GrantSource, Kernel, Projection,
};
use serde_json::{Map, Value};

/// An event's name, and one line of whatever else it has to say.
pub(super) fn trace_line(event: &Event) -> (String, String) {
    let name = event.name();

    let detail = match event {
        Event::StateChanged { from, to } => format!("{from} → {to}"),
        Event::ContextAdded {
            id, label, tokens, ..
        } => format!("[{id}] {label}, {tokens} tokens"),
        Event::ContextChanged { id, from, to, .. } => format!("[{id}] {from} → {to}"),
        Event::ContextRecounted {
            tokens_before,
            tokens_after,
        } => format!("{tokens_before} → {tokens_after} tokens"),
        Event::ModelRequested {
            messages,
            tools,
            tokens,
            ..
        } => format!("{messages} messages, {tools} tools, ~{tokens} tokens"),
        Event::ModelFinished { stop, usage, .. } => match usage {
            Some(usage) => format!(
                "{stop:?}, {} in / {} (reported)",
                usage.input_tokens.unwrap_or(0),
                crate::ui::charged(usage)
            ),
            None => format!("{stop:?}"),
        },
        Event::ToolRequested { tool, .. } | Event::ToolStarted { tool, .. } => tool.clone(),
        Event::ToolFinished { tool, tokens, .. } => format!("{tool}, {tokens} tokens"),
        Event::PermissionRequested { request } => format!("{} ({})", request.tool, request.id),
        Event::PermissionDecided {
            tool,
            grant,
            source,
            ..
        } => format!(
            "{tool}: {grant}, {}",
            match source {
                GrantSource::Policy => "by the policy in force",
                GrantSource::User => "answered when it was asked about",
                GrantSource::Cancellation => "the calls were dropped",
                _ => "by something else",
            }
        ),
        Event::Compacted { report } => format!(
            "{}, {} → {} tokens",
            moved(report),
            report.tokens_before,
            report.tokens_after
        ),
        Event::ModelFailed { error } | Event::StepFailed { error } => one_line(error),
        // note: everything below here used to fall through to the catch-all and print its own
        // name against an empty line. Each of them carries something worth reading, and a log
        // that names an event and then says nothing about it is the shape of a log nobody opens
        Event::SessionStarted { session } => format!("session {session}"),
        Event::SessionResumed {
            session,
            items,
            tokens,
        } => format!("session {session}: {items} items, ~{tokens} tokens"),
        Event::SessionFinished => "nothing more will be recorded".to_owned(),
        Event::Interrupted => "stopped; whatever had arrived is kept".to_owned(),
        // the one event that carries content, because it is the only operation that overwrites
        // something - so the first line of what went is worth the room
        Event::ContextReplaced {
            id,
            tokens_before,
            tokens_after,
            was,
        } => format!(
            "[{id}] {tokens_before} → {tokens_after} tokens; it said: {}",
            one_line(&was.to_text())
        ),
        Event::ContextUndone {
            items,
            removed,
            changed,
        } => format!(
            "{items} items now; {} taken back out, {} put back as they were",
            removed.len(),
            changed.len()
        ),
        Event::ContextRedone {
            items,
            restored,
            changed,
        } => format!(
            "{items} items now; {} back in, {} changed again",
            restored.len(),
            changed.len()
        ),
        Event::ContextAnnotated { id, meta } => format!("[{id}] {}", one_line(&meta.to_string())),
        Event::ModelChanged { from, to } => format!(
            "{} → {}",
            from.as_ref().map(|i| i.model.as_str()).unwrap_or("none"),
            to.as_ref().map(|i| i.model.as_str()).unwrap_or("none")
        ),
        Event::ModelParamsChanged { params } => match params.is_empty() {
            true => "none; the provider's own defaults".to_owned(),
            false => one_line(&serde_json::to_string(params).unwrap_or_default()),
        },
        // not the payload: `/payload` prints the whole of it, and a log line that tried would
        // bury every other line in the pane
        Event::ModelPayload { payload } => format!(
            "{} bytes, rendered by the provider; /payload prints it",
            payload.to_string().len()
        ),
        Event::ToolUnknown { tool, .. } => {
            format!("`{tool}` was asked for and is not registered")
        }
        // a provider that does this is worth knowing about, which is why the kernel announces it
        Event::ToolCallRepaired { call, was, reason } => match was.is_empty() {
            true => format!("gave a call the identifier `{call}`: {reason}"),
            false => format!("`{was}` → `{call}`: {reason}"),
        },
        // and this is the line that says where a repair further down got "earlier in the session"
        // from, in a session that read somebody else's
        Event::ToolCallsReserved { reserved } => {
            format!("{reserved} identifier(s) a loaded session had already used")
        }
        // the names, not the count: `4 tools` is a number somebody has to go and look up, and
        // this line exists because what the model is offered changed
        Event::ToolsChanged { tools } => match tools.is_empty() {
            true => "none; the model is offered no tools at all".to_owned(),
            false => format!("{} offered: {}", tools.len(), tools.join(", ")),
        },
        // a seam being swapped names what went out and what came in; the trace is where somebody
        // reading a session finds out that the thing projecting its requests changed half way
        Event::PolicyChanged { from, to }
        | Event::ProjectorChanged { from, to }
        | Event::CounterChanged { from, to } => format!("{} → {}", short(from), short(to)),
        Event::CompactorChanged { from, to } => format!(
            "{} → {}",
            from.as_deref().map(short).unwrap_or("none"),
            to.as_deref()
                .map(short)
                .unwrap_or("none, so nothing is dropped"),
        ),
        _ => String::new(),
    };

    (name.to_owned(), detail)
}

/// The request the kernel would send, or what stopped it building one.
pub(super) fn request_preview(kernel: &Kernel) -> String {
    // "why is that not in there?" is the question somebody opens this to answer, and the JSON on
    // its own can only say what *is* in there. The projection knows what it left out and what it
    // had to change to keep the request valid, so both go above it - and above the reason there
    // is no request at all, which is where they are worth most
    let projection = kernel.project();
    let mut header = String::new();
    for left_out in &projection.skipped {
        header.push_str(&format!(
            "  [{}] left out: {}\n",
            left_out.id, left_out.reason
        ));
    }
    for repair in &projection.repairs {
        header.push_str(&format!("  repaired: {repair}\n"));
    }

    let request = match kernel.preview_request() {
        // through `pretty`, so that a picture in the context is named here rather than printed:
        // this is the view somebody opens to see the *shape* of a request
        Ok(request) => match serde_json::to_value(&request) {
            Ok(value) => pretty(&value),
            Err(e) => format!("it will not serialize: {e}"),
        },
        Err(e) => nothing_to_send(kernel, &e.to_string()),
    };
    if header.is_empty() {
        return request;
    }

    format!(
        "{} item(s) in, {} out:\n{header}\n{request}",
        projection.included.len(),
        projection.skipped.len()
    )
}

/// Why there is no request to show, in a sentence somebody can do something about.
///
/// note: `the context projects to an empty request` is the runtime's own sentence and it is
/// accurate - `step` refuses to send a request with no messages in it, and it says so in the
/// vocabulary of the thing that refused. It is the wrong answer to "what would go next?" asked
/// in a session nobody has typed into yet, where what happened is that nothing has been said and
/// the projector is working perfectly. And when the context is *not* empty, this is the moment
/// the list of what was left out is worth most - which is exactly when it used to be thrown away,
/// because the error returned before the header was built.
pub(super) fn nothing_to_send(kernel: &Kernel, why: &str) -> String {
    match kernel.items().len() {
        0 => "nothing yet: there is nothing in the context to send. Whatever you type next goes \
              in as an item, and this is where you will see what it turns into - as will \
              --system, --file, -r and /load, which put things in before you type anything."
            .to_owned(),
        items => format!(
            "{why}: not one of the {items} item(s) it holds is going. The list above says which \
             and why; `space` on the context tab puts one back."
        ),
    }
}

/// What a compaction pass moved, in the word belonging to each mechanism.
///
/// note: removing and eliding are two mechanisms and both places that announced a pass named only
/// the first. The compactor that ships here never removes anything - it elides, so that the call
/// each result answers keeps its answer - so every pass it has ever made was announced as
/// `0 items out`, in the line and the trace row that are the only account a person gets of a
/// context changing under them. Third instance of the same conflation, after `Trim`'s candidates
/// and `/budget`'s held-back line.
pub(super) fn moved(report: &nachalnik::CompactionReport) -> String {
    match (report.removed.len(), report.elided.len()) {
        (0, 0) => "nothing moved".to_owned(),
        (0, elided) => format!("{elided} elided"),
        (removed, 0) => format!("{removed} out"),
        (removed, elided) => format!("{removed} out, {elided} elided"),
    }
}

/// The last part of a type's path, which is the part somebody reads.
///
/// note: a seam names itself with `std::any::type_name`, so what arrives here is
/// `kamchatka::tools::Trim` and the column it goes in is thirty characters wide.
fn short(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

/// JSON, indented.
pub(super) fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(&without_blobs(value))
        .unwrap_or_else(|e| format!("it will not serialize: {e}"))
}

/// The same JSON with its base64 payloads taken out and named.
///
/// note: `/request`, `/payload` and `/raw` all print JSON that can carry a
/// [`nachalnik::Content::Blob`], and a blob on this screen is several megabytes of `AAAAAAAA`
/// where somebody was looking for the shape of a request. What they need to know is the same two
/// things the context tab's own row says - that it is there, and how much of it there is - so
/// this says them and drops the rest. The record keeps the whole of it: this is a view.
///
/// note: by *shape* rather than by length, because a long tool result is a thing somebody opened
/// this to read and must not be cut. The three shapes below are the three this workspace
/// produces: the kernel's own `Content::Blob`, a `data:` URI in the conventional dialect, and
/// Google's `inline_data`. A dialect nobody here speaks goes through unelided, which is the right
/// failure - it shows too much rather than hiding something.
fn without_blobs(value: &Value) -> Value {
    match value {
        // `data:image/png;base64,AAAA...`, wherever it sits
        Value::String(text) => match text.split_once(";base64,") {
            Some((head, payload)) if head.starts_with("data:") => {
                Value::String(named(head.trim_start_matches("data:"), payload.len()))
            }
            _ => value.clone(),
        },
        Value::Array(items) => Value::Array(items.iter().map(without_blobs).collect()),
        // `{ media_type | mime_type, data }`, which is both the kernel's shape and Google's
        Value::Object(fields) => {
            let mut elided: Map<String, Value> = fields
                .iter()
                .map(|(key, value)| (key.clone(), without_blobs(value)))
                .collect();

            let media = fields.get("media_type").or_else(|| fields.get("mime_type"));
            if let (Some(Value::String(media)), Some(Value::String(payload))) =
                (media, fields.get("data"))
            {
                elided.insert(
                    "data".to_owned(),
                    Value::String(named(media, payload.len())),
                );
            }

            Value::Object(elided)
        }
        _ => value.clone(),
    }
}

/// What is shown where a payload was.
///
/// note: an exact count, and deliberately not the `292.47kB` `Blob`'s own `Display` gives. This
/// one stands inside `/payload`, which is the request byte for byte with the base64 taken out -
/// so the number is the length of the exact string that was removed from the JSON being read,
/// and rounding it would make the one view whose promise is exactness stop keeping it.
fn named(media_type: &str, bytes: usize) -> String {
    format!("[ base64 blob, {media_type}, {bytes} bytes ]")
}

/// Exactly what one item puts into the next request, in the projector's own words.
///
/// note: the whole context is projected rather than the item on its own, because an item's
/// message is not always a function of the item. A tool result whose call is missing is repaired
/// away, and an item projected alone has no call anywhere - so a lone projection would report
/// "the model gets nothing" about a result the model is about to read.
///
/// note: `included` and `messages` line up one for one under a projector that makes one message
/// per item, which is the dialect this program speaks. One that merges them - and the `Projector`
/// documentation offers exactly that as an example - has no per-item answer to give, and saying
/// so is better than pointing confidently at the wrong message.
pub(super) fn projected(projection: &Projection, id: ContextId) -> String {
    if let Some(left_out) = projection.skipped.iter().find(|item| item.id == id) {
        return format!(
            "nothing: this item is not in the request.\n\n  {}",
            left_out.reason
        );
    }
    let Some(at) = projection.included.iter().position(|other| *other == id) else {
        return "nothing: the projector neither included this item nor said why.".to_owned();
    };
    if projection.included.len() != projection.messages.len() {
        return format!(
            "{} item(s) became {} message(s), so no one of them is this item's alone. \
             ctrl+p shows the whole request.",
            projection.included.len(),
            projection.messages.len()
        );
    }

    // what the projector had to change about this item to keep the request valid: a dropped call,
    // an ordered turn flattened into slots. It is the answer to "why does this not look like what
    // I am reading on the other page?", and it is only ever visible on ctrl+p otherwise
    let mine: Vec<&str> = projection
        .repairs
        .iter()
        .filter(|repair| repair.contains(&format!("item {id}")))
        .map(String::as_str)
        .collect();
    let header = match mine.is_empty() {
        true => String::new(),
        false => format!("repaired: {}\n\n", mine.join("\nrepaired: ")),
    };

    format!("{header}{}", as_sent(&projection.messages[at]))
}

/// A projected message, laid out for reading.
///
/// note: not `to_string_pretty`. The JSON of a message writes every newline in its content out as
/// `\n` on one enormous line, and the content is the whole of what this page is for - the same
/// reason a permission question does not show somebody the JSON of what a tool is about to run.
/// `ctrl+p` is still the byte-for-byte view, of this and of everything around it.
fn as_sent(message: &nachalnik::Message) -> String {
    let mut out = format!("role: {}", message.role);
    if let Some(name) = &message.name {
        out.push_str(&format!("\nanswers: {name}"));
    }
    out.push_str("\n\n");

    match &message.content {
        Some(content) => out.push_str(&whole(content)),
        None => out.push_str("(no content)"),
    }
    if let Some(reasoning) = &message.reasoning {
        out.push_str(&format!("\n\nreasoning:\n{}", whole(reasoning)));
    }
    for call in &message.tool_calls {
        out.push_str(&format!("\n\n{}({})", call.tool, call.args));
    }

    out
}

/// The whole of what an item holds, including what its kind carries beside its content.
///
/// note: a turn recorded in the conventional three slots keeps its calls and its reasoning in the
/// kind rather than in the content, so reading the content alone showed an empty box for a turn
/// that was nothing but tool calls - which is most of them. One recorded as ordered blocks has
/// all three in the content already, and `whole` lays those out in the order they were produced.
pub(super) fn stored(item: &ContextItem) -> String {
    let mut out = whole(&item.content);
    // note: why the item is here at all, which outlives every state it passes through and is
    // therefore the only place a fact about what it holds can be kept. `introspect`'s own item
    // view has printed this all along and it was always empty, because nothing set it; the pair
    // an output limit leaves behind is the first thing that does, and this is where the person
    // reads what the model reads there
    if let Some(because) = &item.included_because {
        out = format!("it is here because: {because}\n\n{out}");
    }
    if let ContextKind::AssistantMessage {
        tool_calls,
        reasoning,
    } = &item.kind
    {
        if let Some(reasoning) = reasoning {
            out = format!("reasoning:\n{}\n\n{out}", whole(reasoning));
        }
        for call in tool_calls {
            out.push_str(&format!("\n\n{}({})", call.tool, call.args));
        }
    }

    out.trim().to_owned()
}

/// The whole of what an item says, including the parts `to_text` leaves out.
///
/// note: for most items this is the content and nothing else. For a turn a provider recorded in
/// the order it was produced, `to_text` is only the *text* blocks - which is right on the wire and
/// wrong on this screen, where the whole point is to be shown what the item really holds. The
/// thinking and the calls are read out where they happened, because between two calls is where
/// the thinking that led to the second one belongs.
pub(super) fn whole(content: &Content) -> String {
    let Some(blocks) = content.as_blocks() else {
        return unpadded(&content.to_text()).to_owned();
    };

    blocks
        .iter()
        .map(|block| {
            let said = match block {
                Block::Call(call) => format!("{}({})", call.tool, call.args),
                _ => block
                    .part()
                    .map(|part| part.content.to_text().into_owned())
                    .unwrap_or_default(),
            };
            match block {
                Block::Text(_) => said,
                _ => format!("{}:\n{said}", block.name()),
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The first line of something, shortened.
pub(super) fn one_line(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    match first.chars().count() > 96 {
        true => format!("{}…", first.chars().take(95).collect::<String>()),
        false => first.to_owned(),
    }
}

/// The first few lines of something, with a note if there were more.
/// The same text without the blank lines a provider put in front of it.
///
/// note: a presentation fix, not a correction to the record. The newlines are the provider's -
/// `inception/mercury` opens every message with two, the recorded `gemini` sessions have none -
/// and the item keeps exactly what arrived, because a runtime whose record is "what arrived,
/// tidied up" cannot answer what arrived. What they must not do is cost three rows of a screen.
///
/// note: leading blank *lines*, not leading whitespace. Trimming the latter takes the indentation
/// off the first line of a message that opens with a code block.
pub(super) fn unpadded(text: &str) -> &str {
    let mut rest = text;
    loop {
        let Some((line, tail)) = rest.split_once('\n') else {
            return rest;
        };
        if !line.trim().is_empty() {
            return rest;
        }
        rest = tail;
    }
}

/// The first few lines of something, and a mark if there was more.
pub(super) fn head(text: &str, lines: usize) -> String {
    // the blank ones first, or a provider that opens every message with two of them spends four
    // of the six rows a tool result gets to make its case in, and truncates two lines early
    let text = unpadded(text);
    let mut kept: Vec<&str> = text.lines().take(lines).collect();
    let total = text.lines().count();
    if total > lines {
        kept.push("…");
    }

    kept.join("\n")
}
