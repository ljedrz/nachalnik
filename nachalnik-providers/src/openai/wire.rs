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

use std::borrow::Cow;

use nachalnik::{
    Blob, Block, BoxError, Content, DeltaSink, Message, ModelInfo, ModelRequest, ModelResponse,
    Provider, Role, StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use serde_json::{Value, json};

use crate::{
    openai::OpenAiCompatible,
    reading::{Events, Read, Stopped, not_a_stream},
    refused,
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
/// note: a summary already held is not appended again, and held means among the summaries: the
/// thinking streamed beside them may say the same words, and a shorter summary may be part of a
/// longer one, and neither is the same summary arriving twice. The status is not read for anything: a
/// summary with words in it has arrived whichever word is beside it, and `unavailable` and
/// `skipped` both carry no content, so the content alone decides. What it must not do is put the
/// word `unavailable` on the screen as if the model had thought it.
///
/// note: what a caller has to ask for, since the reading is only half of it: `reasoning_summary:
/// true` among the parameters, *and* `reasoning_summary_wait: true` beside it whenever the request
/// is streamed. Without the wait, `mercury-2` ends the stream before any summary exists. A client
/// that sets the first and not the second has asked for thinking that cannot then be sent to it,
/// and this has nothing to read.
fn summarised(chunk: &Value, reasoning: &mut String, held: &mut Vec<String>, deltas: &DeltaSink) {
    let Some(summary) = chunk["reasoning_summary"]["content"]
        .as_str()
        .map(str::trim)
        .filter(|summary| !summary.is_empty() && !held.iter().any(|it| it == summary))
    else {
        return;
    };
    held.push(summary.to_owned());

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

/// The fields of a request this dialect builds from the [`ModelRequest`] and the model, which a
/// parameter of the same name does not replace.
const BUILT: [&str; 3] = ["model", "messages", "tools"];

#[async_trait]
impl Provider for OpenAiCompatible {
    fn info(&self) -> ModelInfo {
        // note: one lock to a statement, here and in `respond`. A guard lives to the end of the
        // statement that took it, so a struct literal reading three fields holds all three at once
        // - and this runs on whatever draws the model's name every frame while `respond` runs on
        // the turn. Held together in two orders, `context_limit` then `model` here and the reverse
        // there, the two threads waited on each other for ever at the start of a request
        let context_limit = *self.context_limit.lock();
        let (parameters, max_output_tokens) = {
            let listed = self.listed.lock();
            (listed.parameters.clone(), listed.max_output_tokens)
        };
        let model = self.model.lock().clone();
        let endpoint = crate::recorded(&self.base_url.lock());

        ModelInfo::new(self.label.clone(), model)
            .with_context_limit(context_limit)
            .with_max_output_tokens(max_output_tokens)
            .with_tool_calling(true)
            .with_reasoning(true)
            .with_parameters(parameters)
            .with_endpoint(endpoint)
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
        //
        // note: except the three fields built from the request itself. A parameter is carried
        // beside the conversation, not in place of it: `messages` set here went out instead of
        // the context, under a `model.requested` naming items that were never sent, and `model`
        // asked somebody the record does not name
        for (key, value) in &request.params {
            if !BUILT.contains(&key.as_str()) {
                body[key] = value.clone();
            }
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

        // one lock to a statement; see `info`
        let endpoint = self.endpoint();
        let model = self.model.lock().clone();
        let limit = *self.context_limit.lock();
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
        let mut streamed = Streamed::default();
        match sent(
            &asking,
            &self.attempts,
            limit,
            streaming,
            sending,
            &mut streamed,
        )
        .await?
        {
            Sent::Interrupted => Ok(interrupted()),
            // note: held to what the streamed path holds a whole body to, below. A good status
            // with JSON that is not a completion - a proxy's own `{"status":"ok"}` - is not an
            // answer with nothing in it, and taken as one it finishes the turn with nothing said
            Sent::Whole(payload) if completion(&payload) => {
                Ok(whole(payload, self.thinking_in_content))
            }
            Sent::Whole(payload) => Err(not_a_completion(&payload)),
            Sent::Streamed(Read::Events(events, stopped)) => {
                // note: a stream that carried no choice in it is the same refusal a whole body
                // gets, in the same words. Read as an answer it finished the turn with nothing
                // said and a session that ended normally - a run that looked like it worked and a
                // model that said nothing, with the body only in `raw` and the notice blaming
                // the stream rather than the answer. A server that says the turn is over and
                // carries no choice in it has not answered either, and the two ways a stream can
                // say that are one thing.
                //
                // note: never where something was answered. A stream that carried a choice and
                // was then cut off is what `reading` exists to keep - it was generated and
                // billed for - and refusing it here would throw away the one thing that reader
                // calls the worst outcome. An interrupt nobody pressed is the same.
                match never_answered(streamed.answered, events.is_empty()) {
                    true => Err(not_a_completion(&events[0])),
                    false => Ok(self.answer(streamed, events, stopped)),
                }
            }
            Sent::Streamed(Read::Interrupted) => Ok(interrupted()),
            // a server that ignored `stream: true` and answered whole has still answered
            Sent::Streamed(Read::Unstreamed(body)) => match serde_json::from_str::<Value>(&body) {
                Ok(payload) if completion(&payload) => Ok(whole(payload, self.thinking_in_content)),
                _ => Err(not_a_stream(&body)),
            },
            Sent::Streamed(Read::Refused { said, .. }) => Err(refused(said, limit)),
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
    /// The summaries of the thinking appended to `reasoning` so far.
    summaries: Vec<String>,
    /// The calls, each gathered out of the fragments that carry it.
    gathering: Gathering,
    /// Why the turn ended, once something has said.
    finish: Option<String>,
    /// Whether any event so far carried a choice: an array with nothing in it is not one.
    ///
    /// note: what tells a finished turn from a body that was never an answer. `{"choices":[]}`
    /// is the array and no answer, and so is the usage that arrives with an empty one beside it,
    /// which every endpoint that reports a cost sends as the last event of a turn - so, read as
    /// answers, a stream of nothing but those would end as a turn that said nothing, and a session
    /// that ended normally.
    ///
    /// note: read on the array rather than on `choices[0]`, which is `null` for an event with
    /// nothing in it and would be a choice that is not there.
    answered: bool,
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
        self.answered |= chunk["choices"]
            .as_array()
            .is_some_and(|choices| !choices.is_empty());

        let delta = &choice["delta"];
        if let Some(fragment) = delta["content"].as_str().filter(|f| !f.is_empty()) {
            deltas.text(fragment);
            self.text.push_str(fragment);
        }
        // `reasoning_content` is the other spelling: DeepSeek's, llama.cpp's and vLLM's
        // reasoning parser's. Read under the one name, the other's thinking arrived only in `raw`
        if let Some(fragment) = reasoning_in(delta) {
            deltas.reasoning(fragment);
            self.reasoning.push_str(fragment);
        }
        summarised(chunk, &mut self.reasoning, &mut self.summaries, deltas);

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
            ..
        } = streamed;
        match stopped {
            // note: the server's own end of the stream is an end of the turn, and it is the only
            // thing that says so where no `finish_reason` came with it. Filling it in here rather
            // than leaving the turn unreported is what keeps the two apart: a turn the server
            // finished on purpose and a turn that was cut off before it said anything are the
            // same absence of a `finish_reason` and must not get the same word - which is what
            // `unreported` is for, below.
            //
            // note: a finish that *was* reported is not overwritten. Some endpoints send the
            // marker and then nothing, and some send a reason and the marker both; where the
            // server named the reason it is more precise than what the marker implies.
            Stopped::Done if finish.is_none() => finish = Some("stop".to_owned()),
            Stopped::Done => {}
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
                    let args = arguments_of(&call.args);

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
                // note: an identifier seen before continues its call, unless what arrives is a
                // whole call of its own - a name, and arguments that parse - after one already
                // complete. That is two calls a server gave one identifier, and folded together
                // they are one call to a tool whose name is written twice. A server that repeats
                // the name on every fragment sends fragments that do not parse alone, and those
                // still continue the call
                Some(id) => match self.calls.iter().position(|call| call.id == id) {
                    Some(at) if !whole_again(&self.calls[at], requested) => at,
                    _ => {
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
        //
        // note: an object sent as an object, where the dialect says a string, is the whole of the
        // arguments written out - which is how the whole-answer path reads one too
        let fragment = match &requested["function"]["arguments"] {
            arguments @ Value::Object(_) => Some(Cow::Owned(arguments.to_string())),
            other => match other.as_str() {
                Some(fragment) => Some(Cow::Borrowed(fragment)).filter(|it| !it.is_empty()),
                // note: a null is nothing written, the same as the empty string the dialect
                // spells a call to a tool that takes no arguments with
                None if other.is_null() => None,
                // note: and anything else that is not a string is a fragment like any other.
                // Read as no fragment it vanished, and a call whose arguments arrived as `42`
                // and *then* as `{"path": "a.txt"}` came back with the second half only - a
                // call the model never wrote, and nothing keeping a record that a fragment was
                // dropped on the way
                None => Some(Cow::Owned(other.to_string())),
            },
        };
        let call = &mut self.calls[at];

        // an empty identifier names nothing, which is how the lookup above already reads one - so
        // it does not get to write over the one the call was opened with either
        if let Some(id) = requested["id"].as_str().filter(|id| !id.is_empty()) {
            call.id = id.to_owned();
        }
        // a name is appended to for an endpoint that streams one in pieces, and not for one that
        // repeats the whole of it on every fragment
        if let Some(name) = requested["function"]["name"].as_str()
            && call.name != name
        {
            call.name.push_str(name);
        }
        if !requested["extra_content"].is_null() {
            call.extra = requested["extra_content"].clone();
        }
        let streamed = fragment.map(|fragment| {
            call.args.push_str(&fragment);
            (ToolCallId(call.id.clone()), fragment)
        });
        if let Some((id, fragment)) = streamed {
            self.latest = Some(at);
            deltas.tool_args(id, fragment);
        }
    }
}

/// Whether a fragment is a whole call arriving after `call` is already complete: a name, and
/// arguments that parse on their own, for a call whose own arguments already do.
///
/// note: the fragment is parsed first because it is small and almost never whole; the call's
/// arguments, which grow, are parsed only when it is.
fn whole_again(call: &PartialCall, requested: &Value) -> bool {
    let function = &requested["function"];
    let parses = |arguments: &str| serde_json::from_str::<Value>(arguments).is_ok();

    function["name"]
        .as_str()
        .is_some_and(|name| !name.is_empty())
        && match &function["arguments"] {
            Value::Object(_) => true,
            arguments => arguments.as_str().is_some_and(parses),
        }
        && parses(&call.args)
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

/// Whether a JSON body is a completion: one with a choice in it to read.
///
/// note: a choice rather than the array. `choices: []` is the array and no answer, and read as one
/// it is the empty turn the check is there to refuse.
fn completion(body: &Value) -> bool {
    body["choices"]
        .as_array()
        .is_some_and(|choices| !choices.is_empty())
}

/// The error for a body that carried the envelope of a completion and no choice in it, quoting
/// the server's own.
///
/// note: one refusal for the two shapes it arrives in - a whole body asked for and a stream read
/// an event at a time - and the same words for both. The body is quoted because it is the only
/// account of what came back, and a caller who cannot see it has nothing to act on.
fn not_a_completion(body: &Value) -> BoxError {
    let short = crate::markup::quoted(&body.to_string());
    format!("the answer was not a completion: {short}").into()
}

/// Whether a stream is a body that was never an answer: nothing in it carried a choice, and there
/// is an event to quote for the refusal.
/// note: on having carried no choice, and not on how it stopped. A server that sends
/// `{"choices":[]}` and closes has not answered, and one that sends it and then `[DONE]` has not
/// answered either - the second is a stream the server ended on purpose, which is the same fault
/// by another route. Neither stop reason is the news; the empty array is.
///
/// note: and never where something *was* answered. A stream that carried a choice and was then
/// cut off is what the reader exists to keep, since it was generated and billed for, and an
/// event with no choice in it afterwards is the usage report every endpoint that reports a cost
/// sends last. A guard that looked at the whole stream would throw away the answer of every turn
/// that reported what it cost.
fn never_answered(answered: bool, empty: bool) -> bool {
    !answered && !empty
}

/// Reads a whole answer - one JSON body, no fragments - into a turn.
///
/// note: the streamed path assembles the same thing from `delta` objects a piece at a time; this
/// one is handed `message` finished. What they must agree about is what they make of it: a model
/// whose arguments will not parse, a usage report whose reasoning has to be inferred, and thinking
/// written into the content. The last two go through the same readers on both paths, `usage_of`
/// and `said_and_thought`, and the first gets the same `_unparsed` answer on both.
fn whole(body: Value, inline: bool) -> ModelResponse {
    let choice = &body["choices"][0];
    let message = &choice["message"];

    // note: the summary is on the body rather than on the message, and is read here for the
    // reason `summarised` reads it off a chunk: an endpoint that reports the thinking only as
    // a finished summary is an endpoint whose thinking is otherwise dropped. Not streamed,
    // this one is already the several summaries joined, so there is nothing to append
    let (content, reasoning) = said_and_thought(
        message["content"].as_str().unwrap_or_default(),
        reasoning_in(message).or_else(|| {
            body["reasoning_summary"]["content"]
                .as_str()
                .filter(|text| !text.is_empty())
        }),
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
                // arguments and nothing anywhere to say why. An object sent as an object is taken
                // as it is, which some servers do where the dialect says a string, and so is
                // anything else that is not a string: a number or a list read as
                // `arguments_of("")`, which is a call to a tool that takes no arguments, and the
                // model was told that rather than shown what it wrote
                let args = match &call["function"]["arguments"] {
                    arguments @ Value::Object(_) => arguments.clone(),
                    Value::Null => arguments_of(""),
                    written => match written.as_str() {
                        Some(written) => arguments_of(written),
                        None => json!({ "_unparsed": written.to_string() }),
                    },
                };

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
        raw: Some(body),
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
///
/// note: and inferred only from all three figures. A prompt count left out, read as `0`, made the
/// whole prompt a residual and billed it as generated output, which is the count that could not
/// reach something returning a number anyway.
fn usage_of(reported: &Value) -> Usage {
    let input = reported["prompt_tokens"].as_u64();
    let output = reported["completion_tokens"].as_u64();
    let residual = || {
        let total = reported["total_tokens"].as_u64()?;
        total.checked_sub(input?.checked_add(output?)?)
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
/// note: four shapes for a blob, picked by media type - `image_url` for a picture, `input_audio`
/// for a recording this dialect names, `video_url` for a film, and `file` for a document or
/// anything this dialect has no part of its own for. The split is the dialect's and not this
/// crate's opinion: `image_url` means an image and a PDF sent through it is a 400, and a
/// recording in the `file` part is not refused either - it is dropped, and the model answers a
/// question about bytes it never received.
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
            Some(blob) if audio(&blob.media_type).is_some() => json!({
                // note: the bare base64, which is what this one field holds, where the other
                // parts take a `data:` URI
                "type": "input_audio",
                "input_audio": { "data": blob.data, "format": audio(&blob.media_type) },
            }),
            // note: a film goes in as a `data:` URI, which is the only form any endpoint has
            // been seen to take
            Some(blob) if blob.media_type.starts_with("video/") => json!({
                "type": "video_url",
                "video_url": { "url": data(blob) },
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

/// What `input_audio` calls the two containers it names, by media type.
///
/// note: that field takes the container and the codec as one word where everything else here takes
/// a media type, so the mapping is a table and not a rule: a media type is a claim the producer
/// made, and reading `audio/wav` as `wav` and `audio/mpeg` as `mp3` is this dialect's
/// vocabulary, not a fact about the media types. A type with no word here goes out as a `file`,
/// where a document is wanted anyway, and the endpoint refusing it is better than a guess.
///
/// note: `audio/mp3` and `audio/x-wav` beside the two are the spellings a producer reaches for
/// when the producer is a person, and they are the same containers. `audio/flac` and `audio/ogg`
/// are not in the table, because what this dialect names them is not settled here and a `file`
/// part is where a document is looked for.
const AUDIO: &[(&str, &str)] = &[
    ("audio/wav", "wav"),
    ("audio/x-wav", "wav"),
    ("audio/mpeg", "mp3"),
    ("audio/mp3", "mp3"),
];

/// The word `input_audio` takes for a media type, or `None` where it takes none.
fn audio(media_type: &str) -> Option<&'static str> {
    AUDIO
        .iter()
        .find_map(|(type_, format)| (*type_ == media_type).then_some(*format))
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
        // `nachalnik-mcp` does and is better than a 400. Asked before the parts are built, which
        // encode every blob in the message, on every request
        let user = message.role == Role::User;
        wire["content"] = match user.then(|| parts_of(content)).flatten() {
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

/// The thinking a delta or a message carries, under either of the two names it goes by.
fn reasoning_in(carrier: &Value) -> Option<&str> {
    carrier["reasoning"]
        .as_str()
        .filter(|text| !text.is_empty())
        .or_else(|| {
            carrier["reasoning_content"]
                .as_str()
                .filter(|text| !text.is_empty())
        })
}

/// A call's arguments, from the text the model wrote.
///
/// note: nothing written is no arguments, which is how a server spells a call to a tool that
/// takes none - read as JSON, it failed, and the model was told its arguments were invalid and
/// sent the same correct call again. Anything else that is not JSON is kept as it was written.
fn arguments_of(written: &str) -> Value {
    match written.trim() {
        "" => json!({}),
        written => {
            serde_json::from_str(written).unwrap_or_else(|_| json!({ "_unparsed": written }))
        }
    }
}

#[cfg(test)]
mod tests {
    use nachalnik::{Message, ModelRequest, Params, ToolSpec};

    use super::*;

    /// Thinking under `reasoning_content` is thinking, streamed or whole.
    ///
    /// note: DeepSeek, llama.cpp and vLLM's reasoning parser send it under that name, and it was
    /// read under `reasoning` alone - so it reached nothing but `raw`, and the turn had no thinking.
    #[test]
    fn thinking_under_its_other_name_is_thinking() {
        let answered = whole(
            json!({ "choices": [{ "message": {
                "content": "4", "reasoning_content": "two and two"
            }, "finish_reason": "stop" }] }),
            false,
        );
        assert_eq!(
            answered
                .reasoning
                .map(|it| it.to_text().into_owned())
                .as_deref(),
            Some("two and two")
        );

        let mut streamed = Streamed::default();
        let deltas = DeltaSink::disconnected();
        for chunk in [
            json!({ "choices": [{ "delta": { "reasoning_content": "two and " } }] }),
            json!({ "choices": [{ "delta": { "reasoning_content": "two" } }] }),
            json!({ "choices": [{ "delta": { "content": "4" }, "finish_reason": "stop" }] }),
        ] {
            streamed.event(&chunk, &deltas);
        }
        assert_eq!(streamed.reasoning, "two and two");
    }

    /// A summary that arrives a second time is kept once.
    ///
    /// note: an endpoint may summarise one stretch of reasoning more than once, and appending each
    /// copy would show the reader the same paragraph twice as if the model had thought it twice.
    #[test]
    fn a_summary_that_arrives_twice_is_kept_once() {
        let mut streamed = Streamed::default();
        let deltas = DeltaSink::disconnected();
        let summary = json!({ "choices": [{ "delta": {} }],
            "reasoning_summary": { "content": "working out the budget", "status": "complete" } });

        streamed.event(&summary, &deltas);
        streamed.event(&summary, &deltas);
        assert_eq!(streamed.reasoning, "working out the budget");
    }

    /// A summary is kept when the thinking streamed before it, or an earlier summary, already
    /// holds its words.
    #[test]
    fn a_summary_that_repeats_some_words_is_still_kept() {
        let mut streamed = Streamed::default();
        let deltas = DeltaSink::disconnected();
        let thought = json!({ "choices": [{ "delta": { "reasoning": "first read the file" } }] });
        let summary = json!({ "choices": [{ "delta": {} }],
            "reasoning_summary": { "content": "read the file", "status": "complete" } });

        streamed.event(&thought, &deltas);
        streamed.event(&summary, &deltas);
        assert_eq!(streamed.reasoning, "first read the file\n\nread the file");
    }

    /// A whole answer whose thinking is reported only as a summary on the body keeps it as its
    /// thinking.
    ///
    /// note: `mercury-2` asked with `reasoning_summary: true` puts it there rather than on the
    /// message, and read only off the message the turn has the answer and none of the working.
    #[test]
    fn a_summary_on_a_whole_answer_is_its_thinking() {
        let answered = whole(
            json!({ "choices": [{ "message": { "content": "9" }, "finish_reason": "stop" }],
                "reasoning_summary": { "content": "all but 9 means 9 stay", "status": "complete" } }),
            false,
        );
        assert_eq!(
            answered
                .reasoning
                .map(|it| it.to_text().into_owned())
                .as_deref(),
            Some("all but 9 means 9 stay")
        );
    }

    /// No arguments written is a call with no arguments, and an object sent as one is taken as it
    /// is; only text that is not JSON is `_unparsed`.
    ///
    /// note: an empty string failed to parse, so a call to a tool that takes nothing came back
    /// telling the model its JSON was invalid, and it sent the same correct call again. An object
    /// where the dialect says a string became `{}`, a call run with nothing to say why.
    #[test]
    fn arguments_are_what_was_written() {
        let call = |arguments: Value| {
            whole(
                json!({ "choices": [{ "message": { "tool_calls": [{
                    "id": "c1", "function": { "name": "ls", "arguments": arguments }
                }] }, "finish_reason": "tool_calls" }] }),
                false,
            )
            .tool_calls
            .remove(0)
            .args
        };
        assert_eq!(*call(json!("")), json!({}));
        assert_eq!(*call(json!({ "path": "." })), json!({ "path": "." }));
        assert_eq!(*call(json!("{\"path\": \".\"}")), json!({ "path": "." }));
        assert_eq!(*call(json!("{oops")), json!({ "_unparsed": "{oops" }));
        assert_eq!(arguments_of("  "), json!({}));
    }

    /// Arguments that are neither a string nor an object are shown as they came, not as none.
    ///
    /// note: `unwrap_or_default()` on the `as_str()` of a number, a `null` or a list made the empty
    /// string, and `arguments_of("")` is deliberately a call to a tool that takes no arguments -
    /// so a model that wrote `42` was told it had called `fs` with no arguments, and the tool was
    /// run on that. Streamed, the same value contributed no fragment at all: a call whose
    /// arguments arrived as `42` and *then* as `{"op":"read"}` kept the second half only.
    ///
    /// note: `null` is the exception, and stays what it has always been: a field present and
    /// empty is a call to a tool that takes no arguments, which is how this dialect spells it.
    #[test]
    fn arguments_that_are_neither_a_string_nor_an_object_are_what_the_model_wrote() {
        // the whole-answer path
        let call = |arguments: Value| {
            whole(
                json!({ "choices": [{ "message": { "tool_calls": [{
                    "id": "c1", "function": { "name": "fs", "arguments": arguments }
                }] }, "finish_reason": "tool_calls" }] }),
                false,
            )
            .tool_calls
            .remove(0)
            .args
        };
        for (sent, kept) in [
            (json!(42), json!("42")),
            (json!([1, 2]), json!("[1,2]")),
            (json!(true), json!("true")),
        ] {
            let args = call(sent.clone());
            assert_eq!(
                args["_unparsed"], kept,
                "{sent} has to be visible to the model"
            );
        }
        assert_eq!(*call(json!(null)), json!({}), "null is nothing written");

        // and the streamed one, which is where half a call went missing
        let deltas = DeltaSink::disconnected();
        let mut gathering = Gathering::default();
        for arguments in [json!(42), json!("{\"op\":\"read\"}")] {
            gathering.fold(
                &json!({ "index": 0, "id": "c1", "function": { "name": "fs", "arguments": arguments } }),
                &deltas,
            );
        }
        assert_eq!(gathering.calls.len(), 1);
        let args = arguments_of(&gathering.calls[0].args);
        assert!(
            args["_unparsed"]
                .as_str()
                .is_some_and(|sent| sent.contains("42")),
            "the fragment that was not a string is kept: {args}"
        );
        assert!(
            args["_unparsed"]
                .as_str()
                .is_some_and(|sent| sent.contains("read")),
            "and so is the one that was: {args}"
        );
    }

    /// A stream that carried no choice is the same refusal a whole body with none gets, and an
    /// answer that ended on the server's own marker is a turn with a reason.
    ///
    /// note: `completion()` refuses a whole body whose `choices` is empty, and the streamed path
    /// was checked by neither it nor anything else - so the same events that are an error in one
    /// request became a turn that finished with nothing said and a session that ended normally.
    /// The usage event belongs in the fixture: an endpoint that reports a turn's cost sends
    /// `choices: []` beside it as the last event, so the reading has to be of the stream as a
    /// whole rather than of whichever event came last.
    #[test]
    fn a_stream_with_no_choice_in_it_is_not_an_answer() {
        let deltas = DeltaSink::disconnected();
        let mut streamed = Streamed::default();
        for chunk in [
            json!({ "id": "gen", "choices": [] }),
            json!({ "id": "gen", "choices": [], "usage": {
                "prompt_tokens": 9, "completion_tokens": 0, "total_tokens": 9 } }),
        ] {
            streamed.event(&chunk, &deltas);
        }
        assert!(
            !streamed.answered,
            "an empty array is the array and no answer"
        );

        // and the same events read as a whole body, which is the other request
        let body: Value = serde_json::from_str(r#"{"choices":[],"id":"gen"}"#).expect("a fixture");
        assert!(!completion(&body));
        assert!(
            not_a_completion(&body)
                .to_string()
                .contains("not a completion")
        );

        // whereas an event with a choice in it is an answer, and the usage beside it is not
        let mut said = Streamed::default();
        said.event(
            &json!({ "choices": [{ "delta": { "content": "here" } }] }),
            &deltas,
        );
        said.event(&json!({ "choices": [] }), &deltas);
        assert!(said.answered, "one choice is enough");

        // a turn the server ended on its own marker, and reported no reason for, is an end of
        // the turn rather than a reason nobody gave. Asked through `answer`, which is where the
        // marker is turned into a reason - `unreported` is the word for a turn that really was
        // cut off, and the two absences of a `finish_reason` are told apart by what else came
        let provider = OpenAiCompatible::new("m", "https://example.invalid/v1", "k");
        let mut marker = Streamed::default();
        marker.event(
            &json!({ "choices": [{ "delta": { "content": "ok" } }] }),
            &deltas,
        );
        let ended = provider.answer(marker, Vec::new(), Stopped::Done);
        assert_eq!(ended.stop, StopReason::EndTurn);

        // and a reason that was reported is not overwritten by the marker
        let mut reported = Streamed::default();
        reported.event(
            &json!({ "choices": [{ "delta": {}, "finish_reason": "length" }] }),
            &deltas,
        );
        let ended = provider.answer(reported, Vec::new(), Stopped::Done);
        assert_eq!(
            ended.stop,
            StopReason::Length,
            "what the server said is more precise than what the marker implies"
        );

        // and a turn cut off before it said anything is still the one that word is for
        let mut cut = Streamed::default();
        cut.event(
            &json!({ "choices": [{ "delta": { "content": "half" } }] }),
            &deltas,
        );
        let ended = provider.answer(cut, Vec::new(), Stopped::CutOff);
        assert_eq!(ended.stop, StopReason::Other("cut off".to_owned()));
    }

    /// A stream is kept or refused by whether it answered, and not by how it stopped.
    ///
    /// note: the refusal is for a body that was never an answer. A stream that said something and
    /// was then cut off is what `reading` exists to keep - it was generated and billed for - and a
    /// guard that looked only at the whole stream would throw away the answer of every turn whose
    /// last event was the usage report, which is what an endpoint that reports a cost sends.
    ///
    /// note: and the refusal is on having carried no choice, not on how it stopped. A server that
    /// sends `{"choices":[]}` and closes, and one that sends it and then `[DONE]`, have not
    /// answered in either case, and the second is a stream the server ended on purpose.
    #[test]
    fn a_stream_is_kept_or_refused_by_whether_it_answered_and_not_by_how_it_stopped() {
        let provider = OpenAiCompatible::new("m", "https://example.invalid/v1", "k");
        let deltas = DeltaSink::disconnected();
        let no_choice = vec![json!({ "id": "gen", "choices": [] })];

        // nothing was answered, whichever way the stream stopped
        for stopped in [Stopped::Done, Stopped::CutOff, Stopped::Interrupted] {
            let mut streamed = Streamed::default();
            streamed.event(&no_choice[0], &deltas);
            assert!(!streamed.answered);
            assert!(
                never_answered(streamed.answered, no_choice.is_empty()),
                "{stopped:?}: no choice in it is not an answer, however it stopped"
            );
        }

        // and a turn that did answer is kept through all three
        for stopped in [Stopped::Done, Stopped::CutOff, Stopped::Interrupted] {
            let said = "the first half ";
            let mut streamed = Streamed::default();
            for chunk in [
                json!({ "choices": [{ "delta": { "content": said } }] }),
                json!({ "choices": [], "usage": { "completion_tokens": 4 } }),
            ] {
                streamed.event(&chunk, &deltas);
            }
            assert!(streamed.answered, "one choice is enough");

            let ended = provider.answer(streamed, no_choice.clone(), stopped);
            assert_eq!(
                ended.content.map(|it| it.to_text().into_owned()).as_deref(),
                Some(said),
                "{stopped:?}: what arrived is kept"
            );
        }
    }

    /// This dialect says the model behind it calls tools and thinks.
    ///
    /// note: `ModelInfo::new` says neither, and a client reads both to decide whether to offer
    /// tools and whether to show a thinking pane - so a dialect that carries a `function` part and
    /// a `reasoning` field has to say so. The other dialect's is in `tests/gemini.rs`.
    #[test]
    fn the_model_is_described_as_this_dialect_serves_it() {
        let info = OpenAiCompatible::new("m", "https://example.invalid/v1", "k").info();
        assert!(info.tool_calling, "a `function` part is a call");
        assert!(info.reasoning, "and a `reasoning` field is thinking");
    }

    /// Reasoning is inferred from the total only where the prompt and the completion are both
    /// reported, so a prompt left out is not billed as output.
    #[test]
    fn a_figure_left_out_is_not_read_as_nothing() {
        let usage = usage_of(&json!({ "completion_tokens": 50, "total_tokens": 1_050 }));
        assert_eq!(usage.output_tokens, Some(50), "{usage:?}");
        assert_eq!(usage.reasoning_tokens, None, "{usage:?}");

        let usage = usage_of(
            &json!({ "prompt_tokens": 1_000, "completion_tokens": 50, "total_tokens": 1_100 }),
        );
        assert_eq!(usage.output_tokens, Some(100), "{usage:?}");
    }

    /// A request's tools go out as functions, and a request with none carries no `tools` at all.
    ///
    /// note: absent rather than empty. `"tools": []` is a different request from one with no
    /// field, and it would go out on every request a session makes before it has a tool.
    #[test]
    fn tools_are_sent_only_where_there_are_some() {
        let rendered = |tools: Vec<ToolSpec>| {
            OpenAiCompatible::new("m", "https://example.invalid/v1", "k")
                .render(&ModelRequest {
                    messages: Vec::new(),
                    tools,
                    params: Params::new(),
                })
                .expect("this provider renders")
        };

        let body = rendered(vec![ToolSpec::new("write", "writes a file")]);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "write");
        assert_eq!(body["tools"][0]["function"]["description"], "writes a file");

        assert!(rendered(Vec::new()).get("tools").is_none());
    }

    /// A parameter naming a field built from the request does not replace it.
    ///
    /// note: `messages` and `tools` are what `model.requested` names and `model` is who it names,
    /// so a parameter put over them would send something the record does not describe.
    #[test]
    fn a_parameter_does_not_replace_what_the_request_is_built_from() {
        let provider = OpenAiCompatible::new("m", "https://example.invalid/v1", "k");
        let mut request = ModelRequest {
            messages: vec![Message::user("hello")],
            tools: vec![ToolSpec::new("write", "writes a file")],
            params: Params::new(),
        };
        let untouched = provider.render(&request).expect("this provider renders");

        for key in BUILT {
            request.params.insert(key.to_owned(), json!(null));
        }
        request.params.insert("seed".to_owned(), json!(7));
        let body = provider.render(&request).expect("this provider renders");

        for key in BUILT {
            assert_eq!(body[key], untouched[key], "{key}: {body:#}");
        }
        assert_eq!(body["seed"], 7);
    }

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

    /// Arguments streamed as an object, where the dialect says a string, are the call's arguments.
    ///
    /// note: the whole-answer path took such an object as it was, and the streamed one read only a
    /// string, so the same call came back with its arguments in one and as `{}` in the other - a
    /// call run with nothing to say why.
    #[test]
    fn arguments_streamed_as_an_object_are_the_arguments() {
        let mut gathering = Gathering::default();
        let deltas = DeltaSink::disconnected();
        gathering.fold(
            &json!({ "index": 0, "id": "call_1", "function": {
                "name": "look", "arguments": { "path": "a.txt" }
            } }),
            &deltas,
        );

        assert_eq!(
            arguments_of(&gathering.calls[0].args),
            json!({ "path": "a.txt" })
        );
    }

    /// A name repeated on every fragment is written once, and two whole calls that share an
    /// identifier and carry no index are two calls.
    ///
    /// note: the name was appended to at every fragment, so a server repeating it made `fsfs`,
    /// and two calls under one identifier folded into one such call with both sets of arguments
    /// run together, `_unparsed`. The kernel repairs a repeated identifier; it cannot repair that.
    #[test]
    fn a_repeated_name_or_identifier_does_not_run_calls_together() {
        let deltas = DeltaSink::disconnected();

        let mut repeated = Gathering::default();
        for arguments in ["{\"path\":", " \"a.txt\"}"] {
            repeated.fold(
                &json!({ "index": 0, "id": "n1", "function": { "name": "fs", "arguments": arguments } }),
                &deltas,
            );
        }
        assert_eq!(repeated.calls.len(), 1);
        assert_eq!(repeated.calls[0].name, "fs");
        assert_eq!(
            arguments_of(&repeated.calls[0].args),
            json!({ "path": "a.txt" })
        );

        // the same, with no index, so that the identifier is what files each fragment
        let mut unindexed = Gathering::default();
        for arguments in ["{\"path\":", " \"a.txt\"}"] {
            unindexed.fold(
                &json!({ "id": "n1", "function": { "name": "fs", "arguments": arguments } }),
                &deltas,
            );
        }
        assert_eq!(unindexed.calls.len(), 1);
        assert_eq!(
            arguments_of(&unindexed.calls[0].args),
            json!({ "path": "a.txt" })
        );

        let mut shared = Gathering::default();
        for path in ["a.txt", "b.txt"] {
            shared.fold(
                &json!({ "id": "dup", "function": {
                    "name": "fs", "arguments": format!("{{\"path\":\"{path}\"}}")
                } }),
                &deltas,
            );
        }
        let calls: Vec<_> = shared
            .calls
            .iter()
            .map(|call| (call.name.as_str(), arguments_of(&call.args)))
            .collect();
        assert_eq!(
            calls,
            [
                ("fs", json!({ "path": "a.txt" })),
                ("fs", json!({ "path": "b.txt" }))
            ]
        );
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
