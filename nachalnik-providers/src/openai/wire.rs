//! One request, sent and read back: what goes out on the wire, and what a stream of fragments
//! adds up to.
//!
//! note: the [`Provider`] half of [`OpenAiCompatible`] and the readers it needs, in a file of its
//! own because the type next door is about *where* the requests go and this is about what happens
//! when one does. Every function in here is private: what a caller gets is
//! [`Provider::respond`].
//!
//! note: what is in here is what this dialect's events *say*. Sending, waiting and getting events
//! off the socket are the same for both dialects, and are `waiting` and `reading`.

use nachalnik::{
    Blob, Block, BoxError, Content, DeltaSink, Message, ModelInfo, ModelRequest, ModelResponse,
    Provider, Role, StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use serde_json::{Value, json};

use crate::{
    openai::OpenAiCompatible,
    reading::{Events, Read, Stopped, not_a_stream, read},
    waiting::{Asking, Sent, interrupted, sent},
};

/// A tool call being assembled from streamed fragments.
#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    args: String,
    /// Whatever the provider attached to the call, which it will want back verbatim.
    extra: Value,
    /// The number the provider filed this call under, if it used one. Not a position: see the
    /// note where the fragments are gathered.
    slot: Option<u64>,
}

/// Reads a whole summary of the thinking off a chunk, where the endpoint sends one that way.
///
/// note: the other half of `delta.reasoning`, and a different shape rather than a second name for
/// the same one. That field is a *fragment* of the thinking, pushed as it is generated; this one
/// is a finished summary of it, `{"content": ..., "status": "complete"}` on the chunk itself
/// rather than in the delta, and it arrives after the answer it explains. Inception's endpoint
/// sends it, given `reasoning_summary: true` in the parameters. It sends more than one over a
/// turn, each a summary of the reasoning done since the last, so they are appended rather than
/// replaced: the same endpoint's *non*-streamed field is those same summaries joined.
///
/// note: a summary already held is not appended again. The status is not read for anything: a
/// summary with words in it has arrived whichever word is beside it, and `unavailable` and
/// `skipped` both carry no content, so the content alone decides. What it must not do is put the
/// word `unavailable` on the screen as if the model had thought it.
///
/// note: what a caller has to ask for, since the reading is only half of it: `reasoning_summary:
/// true` among the parameters, *and* `reasoning_summary_wait: true` beside it whenever the request
/// is streamed. Without the wait, `mercury-2` ends the stream before any summary exists. A client
/// that sets the first and not the second has asked for thinking that cannot then be sent to it,
/// and this has nothing to read.
fn summarised(chunk: &Value, reasoning: &mut String, deltas: &DeltaSink) {
    let Some(summary) = chunk["reasoning_summary"]["content"]
        .as_str()
        .map(str::trim)
        .filter(|summary| !summary.is_empty() && !reasoning.contains(*summary))
    else {
        return;
    };

    // a blank line between two of them, because they are separate summaries rather than one text
    // arriving in pieces - and the screen is told the same thing the turn is, or a run that was
    // watched live and one that was read back afterwards say different things
    if !reasoning.is_empty() {
        deltas.reasoning("\n\n");
        reasoning.push_str("\n\n");
    }
    deltas.reasoning(summary);
    reasoning.push_str(summary);
}

#[async_trait]
impl Provider for OpenAiCompatible {
    fn info(&self) -> ModelInfo {
        ModelInfo {
            context_limit: *self.context_limit.lock(),
            tool_calling: true,
            reasoning: true,
            parameters: self.parameters.lock().clone(),
            ..ModelInfo::new(self.label.clone(), self.model.lock().clone())
        }
    }

    /// The payload, rendered once. `respond` sends exactly this, so previewing it is not a second
    /// opinion about what goes out - it is the thing that goes out.
    fn render(&self, request: &ModelRequest) -> Option<Value> {
        let mut body = json!({
            "model": *self.model.lock(),
            "messages": request.messages.iter().map(to_wire).collect::<Vec<_>>(),
        });
        // note: only when one is wanted, because an endpoint asked for a whole answer and handed a
        // `stream` it did not want is being asked a different question
        if self.stream {
            body["stream"] = json!(true);
        }

        if !request.tools.is_empty() {
            body["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|spec| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": spec.id,
                                "description": spec.description,
                                "parameters": spec.schema,
                            },
                        })
                    })
                    .collect(),
            );
        }
        // only what the user set, and nothing else: the kernel invents no parameters, and neither
        // does this provider. Last, so that `stream` is one of the things they can decide
        for (key, value) in &request.params {
            body[key] = value.clone();
        }

        // note: after the parameters rather than beside `stream` above, because it has to follow
        // whatever they settled on, and it is wrong in both directions if it does not. Sent to an
        // endpoint that was asked for a whole answer it is a 400 about a field nobody set; left
        // off a request whose parameters turned streaming *on* it costs the usage report, and the
        // turn goes into the record with its cost unknown
        //
        // note: added to options somebody set rather than put in place of them, since the rest of
        // what they asked for is theirs
        match body["stream"] == json!(true) {
            true => match body["stream_options"].as_object_mut() {
                Some(options) => {
                    options.insert("include_usage".to_owned(), json!(true));
                }
                None => body["stream_options"] = json!({ "include_usage": true }),
            },
            false => {
                if let Some(body) = body.as_object_mut() {
                    body.remove("stream_options");
                }
            }
        }

        Some(body)
    }

    async fn respond(
        &self,
        request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        if self.recording {
            self.requests.lock().push(request.clone());
        }
        let body = self.render(&request).expect("this provider always renders");
        // read off the body rather than off the field, so that a caller who set `stream` in its
        // own parameters gets the path it asked for: `render` puts those on last, deliberately
        let streaming = body["stream"] == json!(true);

        let (endpoint, model, limit) = (
            self.endpoint(),
            self.model.lock().clone(),
            *self.context_limit.lock(),
        );
        let asking = Asking {
            model: &model,
            deltas: &deltas,
            notice: &self.notice,
        };

        let sending = || {
            self.attributed(
                self.client
                    .post(format!("{endpoint}/chat/completions"))
                    .bearer_auth(&self.api_key),
            )
            .json(&body)
        };
        let mut response = match sent(&asking, &self.attempts, limit, streaming, sending).await? {
            Sent::Interrupted => return Ok(interrupted()),
            Sent::Whole(payload) => return Ok(whole(&payload, self.thinking_in_content)),
            Sent::Streaming(response) => response,
        };

        let mut streamed = Streamed::default();
        match read(&mut response, &asking, limit, &mut streamed).await? {
            Read::Events(events, stopped) => Ok(self.answer(streamed, events, stopped)),
            Read::Interrupted => Ok(interrupted()),
            // a server that ignored `stream: true` and answered whole has still answered
            Read::Unstreamed(body) => match serde_json::from_str::<Value>(&body) {
                Ok(payload) if payload["choices"].is_array() => {
                    Ok(whole(&payload, self.thinking_in_content))
                }
                _ => Err(not_a_stream(&body)),
            },
        }
    }
}

/// A stream, read: the turn as the fragments left it.
///
/// note: nothing here is the answer yet - `text` still holds whatever thinking the model wrote
/// into it, and a call's arguments are still the string they arrived in.
#[derive(Default)]
struct Streamed {
    /// What the model said, as it was streamed.
    text: String,
    /// What it thought, where the server sent that in a slot of its own.
    reasoning: String,
    /// The calls, each gathered out of the fragments that carry it.
    gathering: Gathering,
    /// Why the turn ended, once something has said.
    finish: Option<String>,
    /// What the request cost, where the server reported it.
    usage: Option<Usage>,
}

impl Events for Streamed {
    fn event(&mut self, chunk: &Value, deltas: &DeltaSink) {
        if let Some(reported) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(usage_of(reported));
        }

        let choice = &chunk["choices"][0];
        if let Some(reason) = choice["finish_reason"].as_str() {
            self.finish = Some(reason.to_owned());
        }

        let delta = &choice["delta"];
        if let Some(fragment) = delta["content"].as_str().filter(|f| !f.is_empty()) {
            deltas.text(fragment);
            self.text.push_str(fragment);
        }
        if let Some(fragment) = delta["reasoning"].as_str().filter(|f| !f.is_empty()) {
            deltas.reasoning(fragment);
            self.reasoning.push_str(fragment);
        }
        summarised(chunk, &mut self.reasoning, deltas);

        for requested in delta["tool_calls"].as_array().into_iter().flatten() {
            self.gathering.fold(requested, deltas);
        }
    }

    fn finished(&self) -> bool {
        self.finish.is_some()
    }
}

impl OpenAiCompatible {
    /// The turn the stream came to: the thinking taken out of what was said, the arguments
    /// parsed, and the whole of it kept as it arrived.
    fn answer(&self, streamed: Streamed, events: Vec<Value>, stopped: Stopped) -> ModelResponse {
        let Streamed {
            text,
            reasoning,
            gathering,
            mut finish,
            usage,
        } = streamed;
        match stopped {
            Stopped::Ended => {}
            Stopped::Interrupted => finish = Some("interrupted".to_owned()),
            Stopped::CutOff => finish = Some("cut off".to_owned()),
        }

        // note: the fragments have already gone out as `Delta::Text`, thinking and all, because
        // nothing streaming them knows a `</think>` is coming until it arrives - and holding them
        // back on the chance that one might would leave a model that never writes one silent to the
        // end of its turn. So the live view shows what the wire showed and the *turn* is what gets
        // taken apart, which is the copy that is kept, sent back and read again
        let (content, reasoning) = said_and_thought(
            &text,
            (!reasoning.is_empty()).then_some(reasoning.as_str()),
            self.thinking_in_content,
        );

        ModelResponse {
            content,
            reasoning,
            tool_calls: gathering
                .calls
                .into_iter()
                .map(|call| {
                    // a model that produces invalid JSON gets to see that it did
                    let args: Value = serde_json::from_str(&call.args)
                        .unwrap_or_else(|_| json!({ "_unparsed": call.args }));

                    // an empty or repeated identifier is repaired by the kernel, which says so on
                    // the event stream; a provider does not have to paper over it
                    ToolCall::new(call.id, call.name, args).with_extra(call.extra)
                })
                .collect(),
            stop: stop_reason(finish.as_deref()),
            usage,
            raw: Some(json!({ "stream": events })),
        }
    }
}

/// The calls being assembled, and which of them a fragment that names nothing belongs to.
///
/// note: a struct rather than a bare `Vec`, for `latest` alone. A fragment carrying neither an
/// index nor an identifier is a *continuation*, and what it continues is whatever was last being
/// written - which is not the same as the last call in the list the moment a second one has been
/// announced and has not begun. Read as the last in the list, such a fragment lands on the new call
/// and is missing from the old one, so a single misfiled brace breaks two calls rather than none.
#[derive(Default)]
struct Gathering {
    /// The calls, in the order they were first seen.
    calls: Vec<PartialCall>,
    /// Which call the last argument fragment was written to.
    latest: Option<usize>,
}

impl Gathering {
    /// Folds one `tool_calls` fragment into the calls gathered so far, and streams whatever
    /// arguments came with it.
    ///
    /// note: OpenAI numbers the calls in a message and streams each one's arguments in fragments,
    /// so the index is what says which call a fragment belongs to. Google's compatible endpoint
    /// sends no index at all - one whole call per chunk, each with an identifier of its own - and
    /// taking that for index zero folds parallel calls into one: the names run together into
    /// `writewritewrite` and the model is told there is no such tool. So the identifier decides
    /// when there is no index, and a fragment with neither continues whatever was last written to.
    fn fold(&mut self, requested: &Value, deltas: &DeltaSink) {
        let at = match requested["index"].as_u64() {
            // note: the index says which call a fragment belongs to. It is *not* a position in the
            // list: minimax numbers its calls from one, and using it as a slot leaves an unfilled
            // call at zero, which the kernel reports as a repaired identifier and a tool with no
            // name - a wasted round trip and an error the model has to read. So an index is
            // looked up, and a number never seen before starts a new call at the end
            Some(index) => match self.calls.iter().position(|call| call.slot == Some(index)) {
                Some(at) => at,
                None => {
                    self.calls.push(PartialCall {
                        slot: Some(index),
                        ..PartialCall::default()
                    });
                    self.calls.len() - 1
                }
            },
            None => match requested["id"].as_str().filter(|id| !id.is_empty()) {
                Some(id) => match self.calls.iter().position(|call| call.id == id) {
                    Some(at) => at,
                    None => {
                        self.calls.push(PartialCall::default());
                        self.calls.len() - 1
                    }
                },
                // note: the call last *written to* first, and only then the last announced. The two
                // differ where a call has been opened with a name and no arguments while the one
                // before it is still being streamed - and where nothing has been written yet there
                // is nothing else the fragment can mean
                None => match self.latest.or_else(|| self.calls.len().checked_sub(1)) {
                    Some(at) => at,
                    None => {
                        self.calls.push(PartialCall::default());
                        0
                    }
                },
            },
        };

        // note: read before the call is borrowed, and *empty is not a fragment* - the same rule the
        // content and reasoning branches of `Streamed::event` follow. An opener carrying
        // `"arguments": ""` announces a call rather than writing to one, so counting it would put
        // `latest` on a call nothing has been streamed to and hand it the next loose fragment
        let fragment = requested["function"]["arguments"]
            .as_str()
            .filter(|fragment| !fragment.is_empty());
        let call = &mut self.calls[at];

        // an empty identifier names nothing, which is how the lookup above already reads one - so
        // it does not get to write over the one the call was opened with either
        if let Some(id) = requested["id"].as_str().filter(|id| !id.is_empty()) {
            call.id = id.to_owned();
        }
        if let Some(name) = requested["function"]["name"].as_str() {
            call.name.push_str(name);
        }
        if !requested["extra_content"].is_null() {
            call.extra = requested["extra_content"].clone();
        }
        let streamed = fragment.map(|fragment| {
            call.args.push_str(fragment);
            (ToolCallId(call.id.clone()), fragment)
        });
        if let Some((id, fragment)) = streamed {
            self.latest = Some(at);
            deltas.tool_args(id, fragment);
        }
    }
}

/// What opens a block of thinking a model wrote into its own content, where it writes one at all.
const OPENS_THINKING: &str = "<think>";

/// What closes it, which is the half that actually arrives. See
/// [`OpenAiCompatible::thinking_in_content`].
const CLOSES_THINKING: &str = "</think>";

/// The thinking and the answer, out of content the two were written into together, or `None` where
/// there is no `</think>` in it and so nothing to take apart.
///
/// note: the first one only. A model that has closed its thinking and gone on to write about the
/// tags is writing about them, and re-reading the second as a delimiter would take the answer apart
/// at a word in it.
///
/// note: the opener is *stripped where it is there and not required to be*. The case this exists
/// for is a chat template that ends the prompt inside the block, so the model's first token is
/// already thinking and the only delimiter it ever emits is the closing one - which is why the
/// thinking is taken to start at the start of the content rather than at a tag.
fn thinking_of(content: &str) -> Option<(&str, &str)> {
    let at = content.find(CLOSES_THINKING)?;
    let (thought, rest) = content.split_at(at);
    let thought = thought.trim();

    Some((
        thought
            .strip_prefix(OPENS_THINKING)
            .unwrap_or(thought)
            .trim(),
        rest[CLOSES_THINKING.len()..].trim_start(),
    ))
}

/// What a turn said and what it was thinking, given the two fields this dialect has for them and
/// whether thinking found in the wrong one is to be moved.
///
/// note: only where `reasoning` came back empty. An endpoint that fills it has a reasoning parser
/// of its own, and its content is content - `</think>` in that content is a model writing the
/// characters. The field is either the endpoint's answer or nobody's.
fn said_and_thought(
    content: &str,
    reasoning: Option<&str>,
    inline: bool,
) -> (Option<Content>, Option<Content>) {
    let split = match reasoning {
        None if inline => thinking_of(content),
        _ => None,
    };
    let (said, thought) = match split {
        Some((thought, said)) => (said, Some(thought)),
        None => (content, reasoning),
    };

    (
        (!said.is_empty()).then(|| Content::text(said)),
        thought
            .filter(|thought| !thought.is_empty())
            .map(Content::text),
    )
}

/// Reads a whole answer - one JSON body, no fragments - into a turn.
///
/// note: the streamed path assembles the same thing from `delta` objects a piece at a time; this
/// one is handed `message` finished. What they must agree about is what they make of it: a model
/// whose arguments will not parse, a usage report whose reasoning has to be inferred, and thinking
/// written into the content. The last two go through the same readers on both paths, `usage_of`
/// and `said_and_thought`, and the first gets the same `_unparsed` answer on both.
fn whole(body: &Value, inline: bool) -> ModelResponse {
    let choice = &body["choices"][0];
    let message = &choice["message"];

    // note: the summary is on the body rather than on the message, and is read here for the
    // reason `summarised` reads it off a chunk: an endpoint that reports the thinking only as
    // a finished summary is an endpoint whose thinking is otherwise dropped. Not streamed,
    // this one is already the several summaries joined, so there is nothing to append
    let (content, reasoning) = said_and_thought(
        message["content"].as_str().unwrap_or_default(),
        message["reasoning"]
            .as_str()
            .or_else(|| body["reasoning_summary"]["content"].as_str())
            .filter(|text| !text.is_empty()),
        inline,
    );

    ModelResponse {
        content,
        reasoning,
        tool_calls: message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|call| {
                // a model that produces invalid JSON gets to see that it did - the same answer
                // the streamed path gives. Handing it `{}` instead would be a call with no
                // arguments and nothing anywhere to say why
                let written = call["function"]["arguments"].as_str().unwrap_or("{}");
                let args: Value = serde_json::from_str(written)
                    .unwrap_or_else(|_| json!({ "_unparsed": written }));

                ToolCall::new(
                    call["id"].as_str().unwrap_or_default(),
                    call["function"]["name"].as_str().unwrap_or_default(),
                    args,
                )
                .with_extra(call["extra_content"].clone())
            })
            .collect(),
        stop: stop_reason(choice["finish_reason"].as_str()),
        usage: body.get("usage").filter(|u| !u.is_null()).map(usage_of),
        raw: Some(body.clone()),
    }
}

/// What a turn cost, as this dialect reports it.
///
/// note: the two branches disagree about containment, and the difference has to be settled here
/// or it is settled wrongly by whoever reads the result. A reported
/// `completion_tokens_details.reasoning_tokens` is already inside `completion_tokens`; a residual
/// is by construction outside it, being what the total has left over once the prompt and the
/// completion are taken off. [`Usage::output_tokens`] is everything generated, so the residual is
/// added to it and the reported one is not.
///
/// note: the residual is worth inferring because an endpoint that bills for reasoning and reports
/// none by name is the case where a turn's cost is otherwise invisible.
fn usage_of(reported: &Value) -> Usage {
    let input = reported["prompt_tokens"].as_u64();
    let output = reported["completion_tokens"].as_u64();
    let residual = || {
        let total = reported["total_tokens"].as_u64()?;
        total.checked_sub(input.unwrap_or_default() + output.unwrap_or_default())
    };

    let reported_reasoning = reported["completion_tokens_details"]["reasoning_tokens"].as_u64();
    let inferred = reported_reasoning.is_none().then(residual).flatten();

    Usage {
        input_tokens: input,
        output_tokens: match (output, inferred) {
            (output, None) => output,
            (output, Some(extra)) => Some(output.unwrap_or_default() + extra),
        },
        reasoning_tokens: reported_reasoning.or(inferred).filter(|tokens| *tokens > 0),
        cached_input_tokens: reported["prompt_tokens_details"]["cached_tokens"].as_u64(),
    }
}

/// Why the model stopped, in the kernel's vocabulary.
fn stop_reason(finish: Option<&str>) -> StopReason {
    match finish {
        Some("stop") => StopReason::EndTurn,
        Some("interrupted") => StopReason::Other("interrupted".to_owned()),
        Some("tool_calls" | "function_call") => StopReason::ToolUse,
        Some("length") => StopReason::Length,
        Some("content_filter") => StopReason::Refusal,
        Some(other) => StopReason::Other(other.to_owned()),
        None => StopReason::Other("unreported".to_owned()),
    }
}

/// A message's content as this dialect's list of typed parts, where a plain string cannot carry
/// what is in it.
///
/// note: `None` unless there is a blob somewhere. A plain string is what every endpoint speaking
/// this dialect accepts and some of the smaller ones accept nothing else, so a turn of text has to
/// go out as a plain string.
///
/// note: [`Content::Blocks`] is the shape a turn takes when it is *both* - a sentence and the
/// screenshot it is about - and it is the one a caller building a multimodal client reaches for.
/// The blocks are read through [`Block::said`], so a turn's thinking and its calls stay where
/// they belong, which is the `tool_calls` array below and nowhere at all.
///
/// note: two shapes for a blob, chosen by media type - `image_url` for a picture and `file` for
/// everything else - because this dialect gives an attachment its own part and refuses one sent
/// as an image. There is a third, `input_audio`, and it is deliberately not here: no test in this
/// workspace sends a recording, so it would be a shape written from documentation and pinned by
/// nothing. A caller sending one - `kamchatka` attaches mp3, wav and ogg - gets the `file` part,
/// which is the best guess available and is wrong in a way the endpoint will say out loud.
fn parts_of(content: &Content) -> Option<Value> {
    fn part(content: &Content) -> Value {
        let data = |blob: &Blob| format!("data:{};base64,{}", blob.media_type, blob.data);

        match content.as_blob() {
            // note: a picture and a document are two different parts in this dialect, and the
            // media type is the only thing that says which. Sending a PDF as `image_url` is a 400
            // from every endpoint that implements the spec: the field means an image, not an
            // attachment
            Some(blob) if blob.media_type.starts_with("image/") => json!({
                "type": "image_url",
                "image_url": { "url": data(blob) },
            }),
            Some(blob) => json!({
                "type": "file",
                "file": { "filename": filename(blob), "file_data": data(blob) },
            }),
            None => json!({ "type": "text", "text": content.to_text() }),
        }
    }

    match content {
        Content::Blob(_) => Some(json!([part(content)])),
        Content::Blocks(blocks) => blocks
            .iter()
            .filter_map(Block::said)
            .any(|said| said.content.as_blob().is_some())
            .then(|| {
                blocks
                    .iter()
                    .filter_map(Block::said)
                    .map(|said| part(&said.content))
                    .collect()
            }),
        _ => None,
    }
}

/// What to call a payload that is not a picture, because this dialect's `file` part will not go
/// out without a name.
///
/// note: `meta["name"]` first, which is the producer saying what the file was called. The kernel
/// never reads [`Blob::meta`] and neither does anything else here - it is a free-form value for
/// facts a byte length cannot reach, and which file this was is one of them. A client that has the
/// path already, as `kamchatka`'s `/attach` does, costs nothing to fill it in.
///
/// note: and a derived name when nobody supplied one, rather than no field at all. The endpoint
/// refuses the part without it, and `file.pdf` is a worse label than `quarterly-results.pdf` and a
/// far better one than a 400. The media type is where it comes from because the media type is what
/// there is: it is the same string the `file_data` URI declares, so the two cannot disagree.
fn filename(blob: &Blob) -> String {
    match blob.meta.get("name").and_then(Value::as_str) {
        Some(name) => name.to_owned(),
        // `application/pdf` -> `file.pdf`; a type with no slash in it is not one this can improve
        // on, so it keeps the whole of it and lets the endpoint say what it makes of that
        None => match blob.media_type.split_once('/') {
            Some((_, subtype)) => format!("file.{subtype}"),
            None => format!("file.{}", blob.media_type),
        },
    }
}

/// Translates a kernel message into the wire format.
fn to_wire(message: &Message) -> Value {
    let mut wire = json!({ "role": message.role.as_str() });

    if let Some(content) = &message.content {
        // note: only on a user turn. `tool` content is a string in this dialect whatever is in
        // it, so a tool that returned a picture sends the sentence naming it - which is what
        // `nachalnik-mcp` does and is better than a 400
        wire["content"] = match parts_of(content).filter(|_| message.role == Role::User) {
            Some(parts) => parts,
            None => json!(content.to_text()),
        };
    }
    // `calls()` rather than the field: a turn projected as ordered blocks keeps its calls in
    // its content, and reading the field would send the words of a turn with none of the calls
    // in it - which this API rejects, and which is very hard to see afterwards
    let calls: Vec<_> = message.calls().collect();
    if !calls.is_empty() {
        wire["tool_calls"] = Value::Array(
            calls
                .iter()
                .map(|call| {
                    let mut wire = json!({
                        "id": call.id.0,
                        "type": "function",
                        "function": { "name": call.tool, "arguments": call.args.to_string() },
                    });
                    // some APIs hand back a signature per call and reject the next request
                    // without it, so it goes back exactly as it arrived
                    if !call.extra.is_null() {
                        wire["extra_content"] = (*call.extra).clone();
                    }

                    wire
                })
                .collect(),
        );
    }
    if let Some(id) = &message.tool_call_id {
        wire["tool_call_id"] = json!(id.0);
    }
    if let Some(name) = &message.name {
        wire["name"] = json!(name);
    }

    wire
}

#[cfg(test)]
mod tests {
    use nachalnik::{ModelRequest, Params};

    use super::*;

    /// `stream_options` follows whatever the parameters settled `stream` on, in both directions.
    ///
    /// note: it is wrong two different ways otherwise, and one of them is silent. Sent to an
    /// endpoint that was asked for a whole answer it is a 400 about a field nobody set. *Left
    /// off* a request whose parameters turned streaming on - which is how somebody asks for a
    /// stream from a provider built without one - the endpoint reports no usage at all, and the
    /// turn goes into the record with its cost unknown.
    #[test]
    fn stream_options_go_wherever_the_parameters_leave_the_stream() {
        let rendered = |streaming: bool, asked: Option<bool>| {
            let provider =
                OpenAiCompatible::new("m", "https://example.invalid/v1", "k").streaming(streaming);
            let mut request = ModelRequest {
                messages: Vec::new(),
                tools: Vec::new(),
                params: Params::new(),
            };
            if let Some(asked) = asked {
                request.params.insert("stream".to_owned(), json!(asked));
            }
            provider.render(&request).expect("this provider renders")
        };

        for (streaming, asked) in [(true, None), (true, Some(true)), (false, Some(true))] {
            let body = rendered(streaming, asked);
            assert_eq!(body["stream"], json!(true), "{streaming} {asked:?}");
            assert_eq!(
                body["stream_options"],
                json!({ "include_usage": true }),
                "a streamed request has to ask for the usage: {streaming} {asked:?}"
            );
        }

        for (streaming, asked) in [(false, None), (false, Some(false)), (true, Some(false))] {
            let body = rendered(streaming, asked);
            assert_ne!(body["stream"], json!(true), "{streaming} {asked:?}");
            assert!(
                body.get("stream_options").is_none(),
                "a whole answer must not be handed a stream's options: {streaming} {asked:?}"
            );
        }

        // and options somebody set for a stream are added to rather than replaced
        let provider = OpenAiCompatible::new("m", "https://example.invalid/v1", "k");
        let mut params = Params::new();
        params.insert(
            "stream_options".to_owned(),
            json!({ "continuous_usage_stats": true }),
        );
        let body = provider
            .render(&ModelRequest {
                messages: Vec::new(),
                tools: Vec::new(),
                params,
            })
            .expect("this provider renders");
        assert_eq!(
            body["stream_options"],
            json!({ "continuous_usage_stats": true, "include_usage": true })
        );
    }

    /// An empty identifier on a later fragment does not write over the one the call opened with.
    ///
    /// note: the lookup reads an empty identifier as naming nothing, and the write has to agree -
    /// otherwise the call ends up with none, and its early argument fragments are filed under a
    /// different identifier from its late ones.
    #[test]
    fn an_empty_identifier_does_not_unname_a_call() {
        let mut gathering = Gathering::default();
        let deltas = DeltaSink::disconnected();
        gathering.fold(
            &json!({ "index": 0, "id": "call_1", "function": { "name": "look", "arguments": "{" } }),
            &deltas,
        );
        gathering.fold(
            &json!({ "index": 0, "id": "", "function": { "arguments": "}" } }),
            &deltas,
        );

        assert_eq!(gathering.calls[0].id, "call_1");
        assert_eq!(gathering.calls[0].args, "{}");
    }

    /// The shape that is actually seen: no opener, because the template supplied it, and the
    /// answer following the one tag the model wrote.
    #[test]
    fn thinking_with_no_opener_ends_where_the_closing_tag_is() {
        let (thought, said) =
            thinking_of("I should check the budget first.</think>Let me check the budget:")
                .expect("there is a tag in it");

        assert_eq!(thought, "I should check the budget first.");
        assert_eq!(said, "Let me check the budget:");
    }

    /// A model that writes both tags is read the same way, and neither of them survives.
    #[test]
    fn a_matched_pair_leaves_no_tag_in_either_half() {
        let (thought, said) =
            thinking_of("<think>\nweighing it up\n</think>\n\nthe answer").expect("a tag");

        assert_eq!(thought, "weighing it up");
        assert_eq!(said, "the answer");
        assert!(!thought.contains("think"), "{thought}");
    }

    /// Only the first one is a delimiter. Past it a model is writing about the tags, and taking
    /// the second for a delimiter would cut the answer apart at a word inside it.
    #[test]
    fn a_second_closing_tag_is_a_word_in_the_answer_and_not_a_delimiter() {
        let (thought, said) =
            thinking_of("deciding</think>a `</think>` closes the block").expect("a tag");

        assert_eq!(thought, "deciding");
        assert_eq!(said, "a `</think>` closes the block");
    }

    /// Thinking that finished with nothing after it is thinking, and the turn said nothing; none of
    /// it is read as the answer.
    #[test]
    fn thinking_with_nothing_after_it_leaves_the_answer_empty() {
        let (thought, said) = thinking_of("still working on it</think>").expect("a tag");

        assert_eq!(thought, "still working on it");
        assert_eq!(said, "");
    }

    /// Content with no tag in it is content. A model that never writes one must keep every word of
    /// its answer, which is what the `None` here protects.
    #[test]
    fn content_that_closes_no_thinking_is_left_exactly_as_it_was() {
        assert_eq!(thinking_of("just an ordinary answer"), None);

        let (said, thought) = said_and_thought("just an ordinary answer", None, true);
        assert_eq!(
            said.map(|c| c.to_text().into_owned()).as_deref(),
            Some("just an ordinary answer")
        );
        assert_eq!(thought, None);
    }

    /// An endpoint that filled `reasoning` itself has a parser of its own, so a `</think>` in its
    /// content is a model writing the characters and the content is left whole.
    #[test]
    fn content_is_left_alone_where_the_endpoint_reported_its_own_reasoning() {
        let (said, thought) = said_and_thought("a `</think>` tag", Some("weighed it"), true);

        assert_eq!(
            said.map(|c| c.to_text().into_owned()).as_deref(),
            Some("a `</think>` tag")
        );
        assert_eq!(
            thought.map(|c| c.to_text().into_owned()).as_deref(),
            Some("weighed it")
        );
    }

    /// And turned off, nothing is moved at all.
    #[test]
    fn nothing_is_taken_out_of_the_content_when_it_is_turned_off() {
        let (said, thought) = said_and_thought("thinking</think>answer", None, false);

        assert_eq!(
            said.map(|c| c.to_text().into_owned()).as_deref(),
            Some("thinking</think>answer")
        );
        assert_eq!(thought, None);
    }
}
