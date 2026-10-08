//! OpenAI's Responses API: the same endpoint as chat completions, asked at `/responses`, where a
//! turn is an ordered list of typed items rather than a message with slots.
//!
//! note: a mode of [`OpenAiCompatible`] rather than a dialect of its own, because everything about
//! *where* the requests go is the same - the key, the listing, the attribution, the probe, ollama's
//! loaded limit - and only what a request says and what an answer means differ. Those are here.
//!
//! note: what makes it worth having is the reasoning. A reasoning model's thinking comes back as a
//! `reasoning` item whose `encrypted_content` is the thinking itself, sealed, and the chat
//! completions dialect has nowhere to put it - so every turn there starts thinking from nothing,
//! and pays to. Sent back, it is read again rather than redone: measured through OpenRouter on
//! `gpt-5-nano`, the request that carried a call's result back spent no reasoning tokens with the
//! item and 64 without it.
//!
//! note: asked with `store: false`, so nothing is kept on the server between requests and the
//! encrypted reasoning is the only way the thinking survives one - which is why `include` always
//! asks for it. The whole conversation goes every time, as in the other dialects: the context is
//! the session's, not the server's.
//!
//! note: only the fields this API defines go back on an item, as in the Anthropic dialect. An item
//! that came through OpenRouter carries a `format` and an id of OpenRouter's own (`fc_tmp_...`),
//! and a session that started elsewhere carries other providers' fields on its blocks.

use nachalnik::{
    Block, BoxError, Content, DeltaSink, Message, ModelRequest, ModelResponse, Part, Role,
    StopReason, ToolCall, ToolCallId, Usage,
};
use serde_json::{Map, Value, json};

use super::{
    OpenAiCompatible,
    wire::{arguments_of, filename},
};
use crate::{
    Keyed,
    reading::{Events, Read, Stopped, not_a_stream},
    refused,
    waiting::{Asking, Sent, interrupted, sent},
};

/// The fields of a request this mode builds from the [`ModelRequest`] and the model, which a
/// parameter of the same name does not replace.
pub(super) const BUILT: [&str; 3] = ["model", "input", "tools"];

/// What `include` has to ask for for the reasoning to come back in a form that can be sent again.
const SEALED: &str = "reasoning.encrypted_content";

/// The request, as this API takes it.
pub(super) fn render(provider: &OpenAiCompatible, request: &ModelRequest) -> Value {
    let mut input = Vec::new();
    for message in &request.messages {
        items_of(message, &mut input);
    }

    let mut body = json!({
        "model": *provider.model.lock(),
        "input": input,
        "store": false,
    });
    if provider.stream {
        body["stream"] = json!(true);
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|spec| {
                json!({
                    "type": "function",
                    "name": spec.id,
                    "description": spec.description,
                    "parameters": spec.schema,
                })
            })
            .collect();
    }
    for (key, value) in &request.params {
        if !BUILT.contains(&key.as_str()) {
            body[key] = value.clone();
        }
    }

    // note: after the parameters, and added to an `include` of the caller's rather than put in
    // place of it. Unless the caller asked the server to keep the conversation itself - `store:
    // true`, which is theirs to ask - the sealed reasoning is the only way a turn's thinking
    // reaches the next request, and an `include` set for some other reason would quietly lose it
    if body["store"] != json!(true) {
        match body["include"].as_array_mut() {
            Some(included) if !included.iter().any(|it| it == SEALED) => {
                included.push(json!(SEALED))
            }
            Some(_) => {}
            None => body["include"] = json!([SEALED]),
        }
    }

    body
}

/// One message of the conversation as the items it goes out as.
///
/// note: an instruction stays where it was put, as a `system` message among the others, rather than
/// being gathered into `instructions` at the top. OpenAI caches a prompt by its prefix and by
/// nothing else, so an instruction added mid-session and moved to the front would make every
/// request after it a miss from the first token.
fn items_of(message: &Message, items: &mut Vec<Value>) {
    match message.role {
        Role::Assistant => turn(message, items),
        Role::Tool => items.push(answering(message)),
        role => {
            if let Some(content) = said(message.content.as_ref()) {
                let role = match role {
                    Role::System => "system",
                    _ => "user",
                };
                items.push(json!({ "type": "message", "role": role, "content": content }));
            }
        }
    }
}

/// What a person said, or a tool answered: a string where it is only text, and the parts it is
/// made of where it is more than that. `None` for nothing at all.
fn said(content: Option<&Content>) -> Option<Value> {
    let content = content?;
    let blocks: Vec<&Content> = match content {
        Content::Blocks(blocks) => blocks
            .iter()
            .filter_map(Block::said)
            .map(|said| &said.content)
            .collect(),
        said => vec![said],
    };

    if blocks.iter().all(|said| said.as_blob().is_none()) {
        let text = content.to_text();
        return (!text.is_empty()).then(|| json!(text));
    }
    Some(blocks.into_iter().map(part).collect())
}

/// One piece of content as the input part this API carries it in.
///
/// note: a picture is an `input_image` and anything else an `input_file`, by media type, as the
/// Anthropic dialect splits them. Audio and film have no input part here at all, and this API
/// refusing a file in its own words says more than a guess at one.
fn part(content: &Content) -> Value {
    match content.as_blob() {
        Some(blob) => {
            let data = format!("data:{};base64,{}", blob.media_type, blob.data);
            match blob.media_type.starts_with("image/") {
                true => json!({ "type": "input_image", "image_url": data }),
                false => json!({
                    "type": "input_file",
                    "filename": filename(blob),
                    "file_data": data,
                }),
            }
        }
        None => json!({ "type": "input_text", "text": content.to_text() }),
    }
}

/// An assistant turn, as the items it was answered with, in the order it was.
///
/// note: a turn recorded the conventional way - by a session that started against another provider
/// - goes out as what was said and then the calls. Its thinking has no sealed form and is not sent.
fn turn(message: &Message, items: &mut Vec<Value>) {
    let Some(blocks) = message.blocks() else {
        if let Some(text) = message.content.as_ref().map(Content::to_text)
            && !text.is_empty()
        {
            items.push(json!({ "type": "message", "role": "assistant", "content": text }));
        }
        items.extend(message.tool_calls.iter().map(asking));
        return;
    };

    let from = items.len();
    for block in blocks {
        match block {
            Block::Reasoning(part) => items.extend(thought(part)),
            Block::Text(part) => {
                let text = part.content.to_text();
                if !text.is_empty() {
                    items.push(json!({ "type": "message", "role": "assistant", "content": text }));
                }
            }
            Block::Call(call) => items.push(asking(call)),
            // `Block` is `#[non_exhaustive]`; see the Anthropic dialect
            _ => {}
        }
    }

    // note: a reasoning item has to be followed by what it reasoned towards - a message or a call
    // - and this API refuses one that is not. A turn cut off after it had thought and before it
    // said anything leaves one at its end, and that turn would otherwise refuse every request the
    // session made after it
    while items.len() > from && items.last().is_some_and(|item| item["type"] == "reasoning") {
        items.pop();
    }
}

/// A turn's thinking as this API takes it back: sealed, or not at all.
///
/// note: the summary goes back as the one part the session holds it as. It is what was shown of the
/// thinking, and the sealed content is the thinking: this API reads the second.
fn thought(part: &Part) -> Option<Value> {
    let extra = part.extra.as_object()?;
    let sealed = extra
        .get("encrypted_content")
        .and_then(Value::as_str)
        .filter(|sealed| !sealed.is_empty())?;

    let summary = part.content.to_text();
    let mut item = json!({
        "type": "reasoning",
        "summary": match summary.is_empty() {
            true => json!([]),
            false => json!([{ "type": "summary_text", "text": summary }]),
        },
        "encrypted_content": sealed,
    });
    if let Some(id) = extra.get("id").and_then(Value::as_str) {
        item["id"] = json!(id);
    }

    Some(item)
}

/// One tool call, as a `function_call` item.
///
/// note: without the item's own `id`. The call is matched to its result by `call_id`, and an `id`
/// names an item the server kept - which with `store: false` it did not, and which through
/// OpenRouter is one OpenRouter made up.
fn asking(call: &ToolCall) -> Value {
    json!({
        "type": "function_call",
        "call_id": call.id.0,
        "name": call.tool,
        "arguments": call.args.to_string(),
    })
}

/// One tool result, as a `function_call_output` item.
fn answering(message: &Message) -> Value {
    let output = match message.content.as_ref() {
        Some(Content::Json(value)) => json!(value.to_string()),
        content => said(content).unwrap_or_else(|| json!("")),
    };

    json!({
        "type": "function_call_output",
        "call_id": message.tool_call_id.as_ref().map(|id| id.0.as_str()).unwrap_or_default(),
        "output": output,
    })
}

/// Sends the request and reads what comes back.
pub(super) async fn respond(
    provider: &OpenAiCompatible,
    request: ModelRequest,
    deltas: DeltaSink,
) -> Result<ModelResponse, BoxError> {
    let body = render(provider, &request);
    let streaming = body["stream"] == json!(true);

    // one lock to a statement; see the chat-completions `info`
    let endpoint = provider.endpoint();
    let model = provider.model.lock().clone();
    let limit = *provider.context_limit.lock();
    let asking = Asking {
        model: &model,
        deltas: &deltas,
        notice: &provider.notice,
        tries: provider.tries(),
    };

    let sending = || {
        provider
            .attributed(
                provider
                    .client
                    .post(format!("{endpoint}/responses"))
                    .bearer(&provider.api_key),
            )
            .json(&body)
    };
    let mut streamed = Streamed::default();
    match sent(
        &asking,
        &provider.attempts,
        limit,
        streaming,
        sending,
        &mut streamed,
    )
    .await?
    {
        Sent::Interrupted => Ok(interrupted()),
        Sent::Whole(payload) => whole(payload.to_string(), streamed, &deltas),
        Sent::Streamed(Read::Events(events, stopped)) => {
            match streamed.started || streamed.status.is_some() {
                true => Ok(answer(streamed, events, stopped)),
                // a stream that only ever said a response exists has not answered, and read as
                // an answer it is a turn that finished having said nothing
                false => Err(not_a_stream(&Value::Array(events).to_string())),
            }
        }
        Sent::Streamed(Read::Interrupted) => Ok(interrupted()),
        Sent::Streamed(Read::Unstreamed(body)) => whole(body, streamed, &deltas),
        Sent::Streamed(Read::Refused { said, .. }) => Err(refused(said, limit)),
    }
}

/// One item of the answer, being assembled from the stream.
enum Partial {
    /// Thinking: its summary as it arrived, and the item's own fields.
    Reasoning(String, Map<String, Value>),
    /// What the model said, and what it refused to, which this API keeps apart.
    Message { text: String, refusal: String },
    /// A call, its arguments as they arrived.
    Call {
        call_id: String,
        name: String,
        arguments: String,
    },
}

/// A stream, read: the answer as its items left it.
#[derive(Default)]
struct Streamed {
    /// The items so far, by the `output_index` the stream gave each one.
    items: Vec<(u64, Partial)>,
    /// Whether any item has begun.
    started: bool,
    /// The response's status once it has ended: `completed`, `incomplete`, `failed`.
    status: Option<String>,
    /// Why it was incomplete, where it said.
    incomplete: Option<String>,
    /// The thinking's summary part being written, so a second one starts on a line of its own.
    summary_index: Option<(u64, u64)>,
    usage: Option<Usage>,
}

impl Streamed {
    /// The item at an index, if it has begun.
    fn at(&mut self, index: Option<u64>) -> Option<&mut Partial> {
        self.items
            .iter_mut()
            .rev()
            .find(|(at, _)| index.is_none_or(|index| *at == index))
            .map(|(_, partial)| partial)
    }

    /// An item, opened or finished: what it says replaces what was assembled of it only where the
    /// fragments said nothing, so a stream and a whole answer arrive at the same turn.
    ///
    /// note: and what arrives whole is handed on as it would have been in fragments, once: an item
    /// that opens with its text in it, or finishes with text no fragment carried, is a whole
    /// answer's or a terse stream's, and the screen is otherwise shown nothing of it.
    fn item(&mut self, index: u64, item: &Value, deltas: &DeltaSink) {
        let Some(opened) = opened(item) else {
            return;
        };
        self.started = true;
        match self.items.iter_mut().find(|(at, _)| *at == index) {
            None => {
                shown(&opened, deltas);
                self.items.push((index, opened));
            }
            Some((_, partial)) => match (partial, opened) {
                (Partial::Reasoning(text, fields), Partial::Reasoning(whole, more)) => {
                    if text.is_empty() && !whole.is_empty() {
                        deltas.reasoning(&whole);
                        *text = whole;
                    }
                    fields.extend(more);
                }
                (
                    Partial::Message { text, refusal },
                    Partial::Message {
                        text: whole,
                        refusal: refused,
                    },
                ) => {
                    if text.is_empty() && !whole.is_empty() {
                        deltas.text(&whole);
                        *text = whole;
                    }
                    if refusal.is_empty() && !refused.is_empty() {
                        deltas.text(&refused);
                        *refusal = refused;
                    }
                }
                (
                    Partial::Call {
                        call_id,
                        name,
                        arguments,
                    },
                    Partial::Call {
                        call_id: id,
                        name: called,
                        arguments: whole,
                    },
                ) => {
                    if call_id.is_empty() {
                        *call_id = id;
                    }
                    if name.is_empty() {
                        *name = called;
                    }
                    // note: the finished item's arguments where it has them, rather than the
                    // fragments: they are the same text when nothing went wrong, and the item's
                    // are the server's account of it when something did
                    if !whole.is_empty() {
                        *arguments = whole;
                    }
                }
                (partial, opened) => *partial = opened,
            },
        }
    }

    /// The end of the response: why, what it cost, and - where no item was streamed - what it
    /// said.
    fn ended(&mut self, response: &Value, deltas: &DeltaSink) {
        self.status = Some(
            response["status"]
                .as_str()
                .unwrap_or("completed")
                .to_owned(),
        );
        self.incomplete = response["incomplete_details"]["reason"]
            .as_str()
            .map(str::to_owned);
        if let Some(reported) = response.get("usage").filter(|usage| !usage.is_null()) {
            self.usage = Some(usage_of(reported));
        }
        if self.items.is_empty() {
            for (index, item) in response["output"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                self.item(index as u64, item, deltas);
            }
        }
    }
}

impl Events for Streamed {
    fn event(&mut self, event: &Value, deltas: &DeltaSink) {
        let index = event["output_index"].as_u64();
        match event["type"].as_str().unwrap_or_default() {
            "response.output_item.added" | "response.output_item.done" => {
                let index = index.unwrap_or(self.items.len() as u64);
                self.item(index, &event["item"], deltas);
            }
            "response.output_text.delta" => {
                let more = event["delta"].as_str().unwrap_or_default();
                if let Some(Partial::Message { text, .. }) = self.at(index) {
                    deltas.text(more);
                    text.push_str(more);
                }
            }
            "response.refusal.delta" => {
                let more = event["delta"].as_str().unwrap_or_default();
                if let Some(Partial::Message { refusal, .. }) = self.at(index) {
                    deltas.text(more);
                    refusal.push_str(more);
                }
            }
            kind @ ("response.reasoning_summary_text.delta" | "response.reasoning_text.delta") => {
                let more = event["delta"].as_str().unwrap_or_default();
                // a summary in several parts is several paragraphs, and the screen is told so too
                let part = (
                    index.unwrap_or_default(),
                    event["summary_index"].as_u64().unwrap_or_default(),
                );
                let summary = kind == "response.reasoning_summary_text.delta";
                let next = summary && self.summary_index.is_some_and(|at| at != part);
                if summary {
                    self.summary_index = Some(part);
                }
                if let Some(Partial::Reasoning(text, _)) = self.at(index) {
                    if next && !text.is_empty() {
                        deltas.reasoning("\n\n");
                        text.push_str("\n\n");
                    }
                    deltas.reasoning(more);
                    text.push_str(more);
                }
            }
            "response.function_call_arguments.delta" => {
                let more = event["delta"].as_str().unwrap_or_default();
                if let Some(Partial::Call {
                    call_id, arguments, ..
                }) = self.at(index)
                {
                    deltas.tool_args(ToolCallId(call_id.clone()), more);
                    arguments.push_str(more);
                }
            }
            "response.completed" | "response.incomplete" => self.ended(&event["response"], deltas),
            // `response.created`, `response.in_progress`, the `.done` of each fragment - already
            // whole in the item they belong to - and whatever is added next
            _ => {}
        }
    }

    fn finished(&self) -> bool {
        self.status.is_some()
    }

    fn started(&self) -> bool {
        self.started
    }
}

/// Hands on what an item arrived already holding.
fn shown(partial: &Partial, deltas: &DeltaSink) {
    match partial {
        Partial::Reasoning(text, _) if !text.is_empty() => deltas.reasoning(text),
        Partial::Message { text, refusal } => {
            for said in [text, refusal].into_iter().filter(|said| !said.is_empty()) {
                deltas.text(said);
            }
        }
        Partial::Call {
            call_id, arguments, ..
        } if !arguments.is_empty() => deltas.tool_args(ToolCallId(call_id.clone()), arguments),
        _ => {}
    }
}

/// Reads an item as it opened or finished.
fn opened(item: &Value) -> Option<Partial> {
    match item["type"].as_str()? {
        "reasoning" => {
            let summary = joined(&item["summary"], "summary_text");
            // the thinking itself, where an endpoint shows it - `gpt-oss` behind a provider that
            // passes it on - is the better of the two to keep
            let thinking = joined(&item["content"], "reasoning_text");
            let fields = item
                .as_object()
                .map(|fields| {
                    fields
                        .iter()
                        .filter(|(key, _)| matches!(key.as_str(), "id" | "encrypted_content"))
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            Some(Partial::Reasoning(
                match thinking.is_empty() {
                    true => summary,
                    false => thinking,
                },
                fields,
            ))
        }
        "message" => Some(Partial::Message {
            text: joined(&item["content"], "output_text"),
            refusal: item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|part| part["type"] == "refusal")
                .filter_map(|part| part["refusal"].as_str())
                .collect(),
        }),
        "function_call" => Some(Partial::Call {
            call_id: item["call_id"].as_str().unwrap_or_default().to_owned(),
            name: item["name"].as_str().unwrap_or_default().to_owned(),
            arguments: item["arguments"].as_str().unwrap_or_default().to_owned(),
        }),
        // a hosted tool's call - a web search, a file search - and whatever comes next: dropped
        // rather than guessed at, as the other dialects drop what they have no variant for. It is
        // still in `ModelResponse::raw`
        _ => None,
    }
}

/// The text of every part of one kind in a list of them, in a paragraph each.
fn joined(parts: &Value, kind: &str) -> String {
    parts
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part["type"] == kind)
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// A body that was not a stream, read as the events it would have been - or, where it is not a
/// response, the error that says so.
fn whole(
    body: String,
    mut streamed: Streamed,
    deltas: &DeltaSink,
) -> Result<ModelResponse, BoxError> {
    let response = match serde_json::from_str::<Value>(body.trim()) {
        Ok(response) if response["output"].is_array() => response,
        _ => return Err(not_a_stream(&body)),
    };

    for (index, item) in response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        streamed.item(index as u64, item, deltas);
    }
    streamed.ended(&response, deltas);
    Ok(answer(streamed, vec![response], Stopped::Done))
}

/// The turn the response came to: its items in the order they were opened, and what ended it.
fn answer(streamed: Streamed, events: Vec<Value>, stopped: Stopped) -> ModelResponse {
    let Streamed {
        mut items,
        status,
        incomplete,
        usage,
        ..
    } = streamed;

    items.sort_by_key(|(index, _)| *index);
    let mut refused = false;
    let blocks: Vec<Block> = items
        .into_iter()
        .filter_map(|(_, partial)| match partial {
            Partial::Reasoning(said, fields) => Some(Block::Reasoning(
                Part::new(said).with_extra(Value::Object(fields)),
            )),
            Partial::Message { text, refusal } => {
                refused |= !refusal.is_empty();
                let said = match refusal.is_empty() {
                    true => text,
                    false if text.is_empty() => refusal,
                    false => format!("{text}\n\n{refusal}"),
                };
                (!said.is_empty()).then(|| Block::Text(Part::new(said)))
            }
            Partial::Call {
                call_id,
                name,
                arguments,
            } => Some(Block::Call(ToolCall::new(
                call_id,
                name,
                arguments_of(&arguments),
            ))),
        })
        .collect();
    let asked = blocks.iter().any(|block| block.call().is_some());

    let stop = match (stopped, status.as_deref(), incomplete.as_deref()) {
        (Stopped::Interrupted, ..) => StopReason::Other("interrupted".to_owned()),
        (Stopped::CutOff, ..) => StopReason::Other("cut off".to_owned()),
        (_, _, Some("max_output_tokens")) => StopReason::Length,
        (_, _, Some("content_filter")) => StopReason::Refusal,
        _ if refused => StopReason::Refusal,
        _ if asked => StopReason::ToolUse,
        (_, Some("completed"), _) => StopReason::EndTurn,
        (_, _, Some(other)) => StopReason::Other(other.to_owned()),
        (_, Some(other), _) => StopReason::Other(other.to_owned()),
        (_, None, _) => StopReason::Other("unreported".to_owned()),
    };

    ModelResponse {
        content: (!blocks.is_empty()).then(|| Content::blocks(blocks)),
        reasoning: None,
        tool_calls: Vec::new(),
        stop,
        usage,
        raw: Some(json!({ "stream": events })),
    }
}

/// What a response cost, as this API reports it.
///
/// note: `input_tokens` already holds the cached part and `output_tokens` the reasoning, which is
/// what [`Usage`] asks of both - unlike Anthropic's, nothing has to be added.
fn usage_of(reported: &Value) -> Usage {
    Usage {
        input_tokens: reported["input_tokens"].as_u64(),
        output_tokens: reported["output_tokens"].as_u64(),
        reasoning_tokens: reported["output_tokens_details"]["reasoning_tokens"].as_u64(),
        cached_input_tokens: reported["input_tokens_details"]["cached_tokens"].as_u64(),
    }
}
