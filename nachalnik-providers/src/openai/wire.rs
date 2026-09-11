//! One request, sent and read back: what goes out on the wire, and what a stream of fragments
//! adds up to.
//!
//! note: the [`Provider`] half of [`OpenAiCompatible`] and the readers it needs, in a file of its
//! own because the type next door is about *where* the requests go and this is about what happens
//! when one does. Every function in here is private: what a caller gets is
//! [`Provider::respond`].
//!
//! note: each of the readers below is a shape some server actually sent. A refusal arrives as an
//! error object, as a list of failed candidates, or midway through a stream that had already
//! started, and all three have to come out as the same sentence.

use std::{sync::atomic::Ordering, time::Duration};

use nachalnik::{
    Blob, Block, BoxError, Content, DeltaSink, Message, ModelInfo, ModelRequest, ModelResponse,
    Provider, Role, StopReason, ToolCall, ToolCallId, Usage, async_trait,
};
use serde_json::{Value, json};

use crate::{
    openai::OpenAiCompatible,
    out_of_quota,
    waiting::{
        HEARTBEAT, LINGER, PATIENCE, RETRIES, Silence, Unsent, Vigil, WHOLE_ANSWER, gone_quiet,
        interrupted, watched,
    },
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

/// What to say about a request the server refused: its own sentence, rather than its envelope.
///
/// note: a spent quota came back as six hundred characters of JSON - the message, the remedy, the
/// rate-limit headers, and the account's `user_id` - and all of it went into the transcript and
/// into the session log, which is a file people send each other. What a reader needs is the
/// sentence. Nobody needs an identifier for their account written into it.
fn complaint(status: reqwest::StatusCode, body: &str) -> String {
    match serde_json::from_str::<Value>(body)
        .ok()
        .as_ref()
        .and_then(said)
    {
        Some(said) => format!("{status}: {said}"),
        None => {
            let short: String = unmarked(body).chars().take(300).collect();
            match short.is_empty() {
                true => format!("{status}"),
                false => format!("{status}: {short}"),
            }
        }
    }
}

/// The words out of a body that is not JSON, with any markup around them taken off.
///
/// note: for the address that is a web page rather than an API, which is what a mistyped
/// `base_url` produces and is common enough to be worth handling well. `https://example.com/v1`
/// answers 405 with a whole HTML document, and the first three hundred characters of it are a
/// doctype, a `<link rel=icon>` and the opening of a stylesheet - four lines of CSS in the
/// conversation, the session log and the file somebody sends on. That is the same noise as the
/// rate-limit headers this function already exists to strip, arriving through the one branch
/// that was quoting its input.
///
/// note: the *words* rather than a refusal to show any, because a short page is often the only
/// account there is - `<html>gateway timeout</html>` from a proxy that speaks no JSON says the
/// one thing worth knowing. What goes is the tags, and the contents of `<style>` and `<script>`,
/// which are markup wearing the shape of text.
///
/// note: not an HTML parser and not trying to be. A body that is not markup passes through with
/// its whitespace collapsed, which is what a plain-text error wants anyway.
fn unmarked(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body.trim();

    while let Some(at) = rest.find('<') {
        out.push_str(&rest[..at]);
        out.push(' ');
        rest = &rest[at..];

        // a `<style>` or `<script>` is skipped whole: its contents are not prose, and taking
        // only the tags off would leave the stylesheet behind as if it were
        let skip = ["style", "script"].into_iter().find(|element| {
            rest[1..]
                .trim_start()
                .to_ascii_lowercase()
                .starts_with(*element)
        });
        rest = match skip {
            Some(element) => match rest.to_ascii_lowercase().find(&format!("</{element}")) {
                Some(end) => &rest[end..],
                // an unclosed one runs to the end, and the end is where this stops
                None => "",
            },
            None => rest,
        };
        // and past the tag itself, or - for a `<` that never closes - past the `<`
        rest = match rest.find('>') {
            Some(end) => &rest[end + 1..],
            None => "",
        };
    }
    out.push_str(rest);

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The sentence inside an error object, wherever the server put it.
///
/// note: two shapes, both seen on the same endpoint in the same afternoon. A refused request
/// nests it under `error`; a stream that fails halfway sends the object on its own, with `message`
/// at the top. Reading only the first left the second one printing its whole envelope, which is
/// the thing this function exists to stop.
fn said(value: &Value) -> Option<String> {
    let error = match value.get("error").filter(|error| !error.is_null()) {
        Some(nested) => nested,
        None => value,
    };
    let message = match error["message"].as_str() {
        Some(message) => message.trim().to_owned(),
        // a third shape, and the one where reading a sentence mattered most: see `refusals`
        None => refusals(&error["message"])?,
    };

    let mut said = message.chars().take(400).collect::<String>();
    // the upstream's own words, where the wrapper is only reporting that something upstream
    // failed - "Provider returned error" on its own names neither the provider nor the problem
    if let Some(raw) = error["metadata"]["raw"]
        .as_str()
        .map(str::trim)
        .filter(|raw| !raw.is_empty() && !message.contains(*raw))
    {
        said.push_str(" - ");
        said.push_str(&raw.chars().take(300).collect::<String>());
    }
    Some(said)
}

/// Reads a whole summary of the thinking off a chunk, where the endpoint sends one that way.
///
/// note: the other half of `delta.reasoning`, and a different shape rather than a second name for
/// the same one. That field is a *fragment* of the thinking, pushed as it is generated; this one
/// is a finished summary of it, `{"content": ..., "status": "complete"}` on the chunk itself
/// rather than in the delta, and it arrives after the answer it explains. Inception's endpoint is
/// the case - `reasoning_summary: true` in the parameters - and it sends more than one over a
/// turn, each a summary of the reasoning done since the last, which is why they are appended and
/// not replaced: the same endpoint's *non*-streamed field is those same summaries joined, so
/// joining them is reading it the way its author writes it.
///
/// note: a summary already held is not appended again. The status is not read for anything: a
/// summary with words in it has arrived whichever word is beside it, and `unavailable` and
/// `skipped` both carry no content, so the content is the whole of the question. What it must not
/// do is put the word `unavailable` on the screen as if the model had thought it.
///
/// note: what a caller has to ask for, since the reading is only half of it: `reasoning_summary:
/// true` among the parameters, *and* `reasoning_summary_wait: true` beside it whenever the request
/// is streamed - which this crate's always are. Measured against `mercury-2`: with the wait, two
/// of ten chunks carry a summary; without it the stream ends before any summary exists and none
/// of seven do. A client that sets the first and not the second has asked for thinking that cannot
/// then be sent to it, and this has nothing to read.
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

/// The sentences inside a rejected request's list of what was wrong with it, each named by the
/// parameter it is about.
///
/// note: a validated endpoint answers a bad parameter with a *list* rather than a sentence -
/// Inception's `message` is the pydantic shape, `[{"type": "value_error", "loc": ["body",
/// "reasoning_effort"], "msg": "...", "input": "banana", "ctx": {...}}]` - and until this read it,
/// `message` was not a string, [`said`] gave up, and the three hundred characters of envelope the
/// clip left went into the transcript and into the session log. The sentence was in there, forty
/// characters in, wrapped in the machinery. That is the failure [`said`] exists to stop, arriving
/// in the one place where the words are most worth having: the answer to what a request got wrong
/// says which parameter and what it takes.
///
/// note: `loc` is prepended only where the message does not already name the field. Pydantic's
/// own wording varies on exactly that point - a `value_error` raised by a validator usually names
/// it, a type failure says "Input should be a valid boolean" and names nothing - and a message
/// that does not say which of eight parameters it means is a message somebody has to guess at.
fn refusals(message: &Value) -> Option<String> {
    let listed = message.as_array().filter(|listed| !listed.is_empty())?;

    let said: Vec<String> = listed
        .iter()
        .filter_map(|wrong| {
            let msg = wrong["msg"].as_str()?.trim();
            let field = wrong["loc"]
                .as_array()
                .and_then(|loc| loc.last())
                .and_then(Value::as_str)
                .filter(|field| !msg.contains(*field));

            Some(match field {
                Some(field) => format!("{field}: {msg}"),
                None => msg.to_owned(),
            })
        })
        .collect();

    (!said.is_empty()).then(|| said.join("; "))
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
        // off a request whose parameters turned streaming *on* it costs the usage report, and a
        // turn whose cost is unknown is the one thing this workspace will not have
        match body["stream"] == json!(true) {
            true => body["stream_options"] = json!({ "include_usage": true }),
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
        // nothing arrives until the whole answer does, when it is not a stream, so the silence
        // that means "this has stalled" is a much longer one
        let patience = match streaming {
            true => PATIENCE,
            false => WHOLE_ANSWER,
        };

        // read once, and used for every line said about this request: a name that changed halfway
        // through would make one wait look like two
        let model = self.model.lock().clone();

        // a free tier answers "busy" often enough that not retrying makes the whole thing look
        // broken when it is not. Waiting and trying again is the *provider's* business: the
        // kernel must not silently send a request twice behind a caller's back
        let mut response = loop {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let response = match watched(
                self.attributed(
                    self.client
                        .post(format!("{}/chat/completions", self.endpoint()))
                        .bearer_auth(&self.api_key),
                )
                .json(&body)
                .send(),
                &deltas,
                &model,
                &self.notice,
                patience,
            )
            .await
            {
                Ok(response) => response,
                // a connection that timed out is a busy server wearing different clothes, and it
                // used to be the one thing here that was not waited out: a 429 got four tries and
                // a doubling, a stall got none. Eleven of fourteen runs against one upstream died
                // this way while the same model answered a single request in six seconds. A
                // refused connection is *not* this - it is a definite answer, usually an address
                // with nothing behind it, and making a typo take four doublings to report helps
                // nobody
                Err(reason) if reason.worth_waiting_out() => {
                    let attempt = self.backoff.fetch_add(1, Ordering::SeqCst) + 1;
                    let wait = Duration::from_secs(1 << attempt);
                    if attempt >= RETRIES {
                        self.backoff.store(0, Ordering::SeqCst);
                        return Err(reason.giving_up(&model));
                    }

                    *self.notice.lock() = Some(format!(
                        "{model} {}; trying again in {}s",
                        reason.what_happened(),
                        wait.as_secs()
                    ));
                    tokio::time::sleep(wait).await;
                    continue;
                }
                // nobody is owed an error for being obeyed
                Err(Unsent::Interrupted) => return Ok(interrupted()),
                Err(reason) => return Err(reason.giving_up(&model)),
            };

            let status = response.status();
            if status.is_success() {
                // a whole answer is read here rather than after the loop, because this dialect's
                // other way of saying 429 is an `error` object inside a perfectly good 200 - and
                // an upstream limit reported that way is exactly as worth waiting out as one
                // reported as a status. There is nothing to watch arrive and nothing to
                // interrupt: by the time this reads it the model has finished and been billed
                if !streaming {
                    let text = response.text().await?;
                    let payload: Value = serde_json::from_str(&text)
                        .map_err(|e| format!("the answer was not JSON ({e}): {text}"))?;
                    let Some(error) = payload.get("error").filter(|e| !e.is_null()) else {
                        self.backoff.store(0, Ordering::SeqCst);
                        return Ok(whole(&payload));
                    };

                    let code = error["code"].as_u64().unwrap_or_default();
                    // a spent daily quota is a 429 that will still be one in a minute, so it is
                    // told apart here rather than waited out four times over
                    let transient =
                        (code == 429 || (500..600).contains(&code)) && !out_of_quota(&text);
                    let attempt = self.backoff.fetch_add(1, Ordering::SeqCst) + 1;
                    let wait = Duration::from_secs(1 << attempt);
                    if !transient || attempt >= RETRIES {
                        self.backoff.store(0, Ordering::SeqCst);
                        return Err(match said(error) {
                            Some(said) => said,
                            None => format!("{error}").chars().take(300).collect(),
                        }
                        .into());
                    }

                    *self.notice.lock() = Some(format!(
                        "{model} answered {code}; trying again in {}s",
                        wait.as_secs()
                    ));
                    tokio::time::sleep(wait).await;
                    continue;
                }

                // the budget belongs to a request, not to a session: without this an afternoon
                // that had already ridden out four busy servers answered the fifth by giving up
                // on the first try
                self.backoff.store(0, Ordering::SeqCst);
                break response;
            }

            // the server's own answer to "when?", where it gives one. Guessing at a doubling is
            // for a server that did not say
            let asked = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .map(Duration::from_secs);

            let transient = status.as_u16() == 429 || status.is_server_error();
            let attempt = self.backoff.fetch_add(1, Ordering::SeqCst) + 1;
            let wait = asked.unwrap_or(Duration::from_secs(1 << attempt));
            if !transient || attempt >= RETRIES || wait > LINGER {
                self.backoff.store(0, Ordering::SeqCst);
                let body = response.text().await.unwrap_or_default();
                let mut said = complaint(status, &body);
                if transient && wait > LINGER {
                    said.push_str(&format!(
                        " - it asked to be left for {}s, which is longer than this waits",
                        wait.as_secs()
                    ));
                }
                return Err(said.into());
            }

            *self.notice.lock() = Some(format!(
                "{} answered {}; trying again in {}s",
                self.model.lock(),
                status.as_u16(),
                wait.as_secs()
            ));
            tokio::time::sleep(wait).await;
        };

        // bytes rather than a `String`, because a chunk boundary is not a character boundary. A
        // multi-byte character split across two reads used to be decoded twice, lossily, and
        // arrived as two replacement characters that then went into the context, the transcript
        // and the session log: `zażółć` came back `za\u{fffd}\u{fffd}ółć`. Held as bytes, the tail of
        // a split character waits in here for the rest of itself, and only whole lines are decoded
        let mut buffer: Vec<u8> = Vec::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut calls: Vec<PartialCall> = Vec::new();
        let mut finish = None;
        let mut usage = None;
        // every payload the server sent, verbatim
        let mut chunks = Vec::new();
        let mut vigil = Vigil::new();

        loop {
            // the timeout is what makes a model that says nothing at all interruptible; without
            // it this sits in `chunk` until the server feels like talking, and a request that
            // stalls before its first byte leaves an interrupt doing nothing at all. The same
            // reason the shell tool has one
            let bytes = match tokio::time::timeout(HEARTBEAT, response.chunk()).await {
                Ok(Ok(Some(bytes))) => {
                    if vigil.heard() {
                        *self.notice.lock() =
                            Some(format!("{} is answering again", self.model.lock()));
                    }
                    bytes
                }
                Ok(Ok(None)) => break,
                // the body stopped arriving in the middle of an answer. Everything parsed so far
                // is kept and the socket is abandoned, which is what the interrupt below already
                // does for the other way a stream ends early - this is that case without the
                // consent, so it is marked with a name of its own instead of `interrupted`.
                //
                // note: it used to return the error, which failed the turn and threw away every
                // token the model had produced *and been billed for*: one session spent 148
                // seconds on an answer and kept none of it. Retrying is the other candidate and
                // is worse, because every attempt is billed too - an answer that reliably outruns
                // an upstream's patience would be paid for four times and fail anyway - and the
                // loop above retries only where nothing was generated
                Ok(Err(e)) => {
                    // nothing arrived at all, so there is nothing to keep and no answer to
                    // report; the transport's own account is the most useful thing there is
                    if chunks.is_empty() {
                        return Err(e.into());
                    }
                    // a turn whose `finish_reason` already arrived is a complete answer that lost
                    // its trailing bytes, and calling that cut off would be inventing a fault
                    if finish.is_none() {
                        *self.notice.lock() = Some(format!(
                            "{} was cut off mid-answer ({e}); what had arrived is kept",
                            self.model.lock()
                        ));
                        finish = Some("cut off".to_owned());
                    }
                    break;
                }
                Err(_) => {
                    if deltas.is_interrupted() {
                        finish = Some("interrupted".to_owned());
                        break;
                    }
                    match vigil.waited() {
                        Silence::Enough => {
                            return Err(format!(
                                "{} answered and then said nothing for {}s; giving up",
                                self.model.lock(),
                                PATIENCE.as_secs()
                            )
                            .into());
                        }
                        Silence::Worth(seconds) => {
                            *self.notice.lock() = Some(gone_quiet(&self.model.lock(), seconds));
                        }
                        Silence::Ordinary => {}
                    }
                    continue;
                }
            };
            buffer.extend_from_slice(&bytes);

            while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                // somebody pressed escape. Whatever has been parsed is kept and the rest of the
                // socket is abandoned; the check is here, before the next fragment, so that a
                // fragment is never read and then thrown away
                if deltas.is_interrupted() {
                    finish = Some("interrupted".to_owned());
                    break;
                }

                // a whole line, so whatever multi-byte characters it holds are all here
                let line = String::from_utf8_lossy(&buffer[..end]).trim().to_owned();
                buffer.drain(..=end);

                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                // these APIs report an upstream failure - a rate limit, a dead provider - as an
                // error object rather than an HTTP status, sometimes mid-stream
                if let Some(error) = chunk.get("error").filter(|e| !e.is_null()) {
                    // the same treatment the refused-request path gets: this one arrives as a
                    // bare object with `message` at the top rather than nested under `error`, and
                    // printing it whole put the provider's entire envelope on the screen
                    return Err(match said(error) {
                        Some(said) => said,
                        None => format!("{error}").chars().take(300).collect(),
                    }
                    .into());
                }

                if let Some(reported) = chunk.get("usage").filter(|u| !u.is_null()) {
                    usage = Some(usage_of(reported));
                }

                let choice = &chunk["choices"][0];
                if let Some(reason) = choice["finish_reason"].as_str() {
                    finish = Some(reason.to_owned());
                }

                let delta = &choice["delta"];
                if let Some(fragment) = delta["content"].as_str().filter(|f| !f.is_empty()) {
                    deltas.text(fragment);
                    text.push_str(fragment);
                }
                if let Some(fragment) = delta["reasoning"].as_str().filter(|f| !f.is_empty()) {
                    deltas.reasoning(fragment);
                    reasoning.push_str(fragment);
                }
                summarised(&chunk, &mut reasoning, &deltas);

                for requested in delta["tool_calls"].as_array().into_iter().flatten() {
                    // note: OpenAI numbers the calls in a message and streams each one's arguments
                    // in fragments, so the index is what says which call a fragment belongs to.
                    // Google's compatible endpoint sends no index at all - one whole call per
                    // chunk, each with an identifier of its own - and taking that for index zero
                    // folded three parallel calls into one: the names ran together into
                    // `writewritewrite` and the model was told there was no such tool. So the
                    // identifier decides when there is no index, and a fragment with neither
                    // continues whatever came last
                    let at = match requested["index"].as_u64() {
                        // note: the index says which call a fragment belongs to. It is *not* a
                        // position in the list: minimax numbers its calls from one, and using it
                        // as a slot left an unfilled call at zero, which the kernel then reported
                        // as a repaired identifier and a tool with no name - a wasted round trip
                        // and an error the model had to read. So an index is looked up, and a
                        // number never seen before starts a new call at the end
                        Some(index) => match calls.iter().position(|call| call.slot == Some(index))
                        {
                            Some(at) => at,
                            None => {
                                calls.push(PartialCall {
                                    slot: Some(index),
                                    ..PartialCall::default()
                                });
                                calls.len() - 1
                            }
                        },
                        None => match requested["id"].as_str().filter(|id| !id.is_empty()) {
                            Some(id) => match calls.iter().position(|call| call.id == id) {
                                Some(at) => at,
                                None => {
                                    calls.push(PartialCall::default());
                                    calls.len() - 1
                                }
                            },
                            None => match calls.is_empty() {
                                true => {
                                    calls.push(PartialCall::default());
                                    0
                                }
                                false => calls.len() - 1,
                            },
                        },
                    };
                    let call = &mut calls[at];

                    if let Some(id) = requested["id"].as_str() {
                        call.id = id.to_owned();
                    }
                    if let Some(name) = requested["function"]["name"].as_str() {
                        call.name.push_str(name);
                    }
                    if !requested["extra_content"].is_null() {
                        call.extra = requested["extra_content"].clone();
                    }
                    if let Some(fragment) = requested["function"]["arguments"].as_str() {
                        call.args.push_str(fragment);
                        deltas.tool_args(ToolCallId(call.id.clone()), fragment);
                    }
                }

                chunks.push(chunk);
            }
            if finish.as_deref() == Some("interrupted") {
                break;
            }
        }
        if chunks.is_empty() {
            // a request stopped before the server had said anything is not a broken response, and
            // reporting it as one would put a red line on the screen for doing what was asked
            if finish.as_deref() == Some("interrupted") || deltas.is_interrupted() {
                return Ok(interrupted());
            }

            // the response was not a stream at all; an error body is the usual reason
            let buffer = String::from_utf8_lossy(&buffer);
            let payload: Value = serde_json::from_str(&buffer).unwrap_or(Value::Null);
            return match payload.get("error").filter(|e| !e.is_null()) {
                Some(error) => Err(format!("{error}").into()),
                None => Err(format!("the stream carried no data: {buffer}").into()),
            };
        }

        Ok(ModelResponse {
            content: (!text.is_empty()).then_some(Content::text(text)),
            reasoning: (!reasoning.is_empty()).then_some(Content::text(reasoning)),
            tool_calls: calls
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
            raw: Some(json!({ "stream": chunks })),
        })
    }
}

/// Reads a whole answer - one JSON body, no fragments - into a turn.
///
/// note: the streamed path assembles the same thing from `delta` objects a piece at a time; this
/// one is handed `message` finished. What they must agree about is what they make of it, which is
/// why the two readers below are shared rather than written twice: a model whose arguments will
/// not parse, and a usage report whose reasoning has to be inferred, were each handled one way
/// here and another there.
fn whole(body: &Value) -> ModelResponse {
    let choice = &body["choices"][0];
    let message = &choice["message"];

    ModelResponse {
        content: message["content"]
            .as_str()
            .filter(|text| !text.is_empty())
            .map(Content::text),
        // note: the summary is on the body rather than on the message, and is read here for the
        // reason `summarised` reads it off a chunk: an endpoint that reports the thinking only as
        // a finished summary is an endpoint whose thinking is otherwise dropped. Not streamed,
        // this one is already the several summaries joined, so there is nothing to append
        reasoning: message["reasoning"]
            .as_str()
            .or_else(|| body["reasoning_summary"]["content"].as_str())
            .filter(|text| !text.is_empty())
            .map(Content::text),
        tool_calls: message["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|call| {
                // a model that produces invalid JSON gets to see that it did - the same answer
                // the streamed path gives. Handing it `{}` instead meant a call arrived with no
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
/// none by name is the case where a turn's cost is otherwise invisible. There is nowhere else to
/// find out where it went.
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
/// note: `None` unless there is a blob somewhere, and that is the whole rule. A plain string is
/// what every endpoint speaking this dialect accepts and some of the smaller ones accept nothing
/// else, so a turn of text has to go out exactly as it did before any of this existed.
///
/// note: [`Content::Blocks`] is the shape a turn takes when it is *both* - a sentence and the
/// screenshot it is about - and it is the one a caller building a multimodal client reaches for.
/// The blocks are read through [`Block::said`], so a turn's thinking and its calls stay where
/// they belong, which is the `tool_calls` array below and nowhere at all.
///
/// note: two shapes for a blob, chosen by media type - `image_url` for a picture and `file` for
/// everything else - because this dialect gives an attachment its own part and refuses one sent
/// as an image. There is a third, `input_audio`, and it is deliberately not here: nothing in this
/// workspace produces a recording, so it would be a shape written from documentation and pinned by
/// no test. A caller sending one gets the `file` part, which is the best guess available and is
/// wrong in a way the endpoint will say out loud.
fn parts_of(content: &Content) -> Option<Value> {
    fn part(content: &Content) -> Value {
        let data = |blob: &Blob| format!("data:{};base64,{}", blob.media_type, blob.data);

        match content.as_blob() {
            // note: a picture and a document are two different parts in this dialect, and the
            // media type is the only thing that says which. Sending a PDF as `image_url` is a 400
            // from every endpoint that implements the spec - the field means an image, not an
            // attachment - and it was the shape everything went out in until something that was
            // not a picture had to
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
        // `nachalnik-mcp` has always done and is better than a 400
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
    }

    /// The two shapes a rate limit actually arrived in, copied out of a real session.
    ///
    /// note: `concat!` rather than a raw string over several lines. A raw string keeps the
    /// backslash *and* the newline, so a fixture written that way is not JSON, `complaint` falls
    /// through to clipping it, and the test passes without ever reaching the code it is about.
    #[test]
    fn a_refused_request_is_reported_in_the_server_s_own_words_and_no_more() {
        let status = reqwest::StatusCode::TOO_MANY_REQUESTS;

        // a spent daily quota: one useful sentence, wrapped in the rate-limit headers and the
        // account's identifier, neither of which belongs in a file somebody will send on
        let daily = concat!(
            r#"{"error":{"message":"Rate limit exceeded: free-models-per-day. Add 10 credits"#,
            r#" to unlock 1000 free model requests per day","code":429,"metadata":{"headers":"#,
            r#"{"X-RateLimit-Remaining":"0"},"limit_source":"openrouter_free_tier_daily"}},"#,
            r#""user_id":"user_3GBJq3JdBGGCK0OiVeXg1v8GYfW"}"#,
        );
        assert!(
            serde_json::from_str::<Value>(daily).is_ok(),
            "the fixture has to be the shape the server actually sends"
        );
        let said = complaint(status, daily);
        assert!(said.contains("free-models-per-day"), "{said}");
        assert!(
            !said.contains("user_"),
            "the account is nobody's business: {said}"
        );
        assert!(!said.contains("X-RateLimit"), "{said}");
        assert!(said.len() < daily.len() / 2, "and it is shorter: {said}");

        // an upstream one, where the wrapper's own message names neither the provider nor the
        // problem, and the sentence worth reading is underneath it
        let upstream = concat!(
            r#"{"error":{"message":"Provider returned error","code":429,"metadata":{"raw":"#,
            r#""z-ai/glm-5.2:free is temporarily rate-limited upstream.","provider_name":"#,
            r#""Decart"}}}"#,
        );
        assert!(serde_json::from_str::<Value>(upstream).is_ok());
        let said = complaint(status, upstream);
        assert!(said.contains("Provider returned error"), "{said}");
        assert!(said.contains("temporarily rate-limited upstream"), "{said}");

        // something that is not JSON at all still says what happened
        let plain = complaint(status, "<html>gateway timeout</html>");
        assert!(
            plain.contains("429") && plain.contains("gateway timeout"),
            "{plain}"
        );
        assert!(
            !plain.contains('<'),
            "and without the markup round it: {plain}"
        );

        // a whole web page, which is what a base URL pointing at a site answers with. Its
        // words are the useful part and its stylesheet is not: 405 from  used to
        // put four lines of CSS in the conversation and the session log
        let page = concat!(
            r#"<!doctype html><html lang="en"><head><title>Example Domain</title>"#,
            r#"<link rel="icon" href="data:,"><style>body{background:#eee;width:60vw;"#,
            r#"font-family:system-ui,sans-serif}h1{font-size:1.5em}</style></head><body>"#,
            r#"<h1>Example Domain</h1><p>This domain is for use in illustrative examples.</p>"#,
            r#"</body></html>"#,
        );
        let said = complaint(reqwest::StatusCode::METHOD_NOT_ALLOWED, page);
        assert!(said.contains("This domain is for use"), "{said}");
        assert!(
            !said.contains("font-family"),
            "the stylesheet is not prose: {said}"
        );
        assert!(!said.contains('<'), "nor are the tags: {said}");
        assert!(said.len() < page.len() / 2, "and it is shorter: {said}");

        // and so does nothing at all
        assert!(complaint(status, "").contains("429"));
    }

    /// note: the two wordings pydantic gives the same kind of failure, which is why `loc` is read.
    /// A validator that raised the error usually names the parameter in the message; a type
    /// failure says "Input should be a valid boolean" and names nothing, and a refusal that does
    /// not say which of eight parameters it is about is one somebody has to guess at. Both are
    /// quoted from what `api.inceptionlabs.ai` answers.
    #[test]
    fn a_request_refused_by_a_list_of_failures_is_reported_by_the_sentences_in_it() {
        let status = reqwest::StatusCode::BAD_REQUEST;

        let named = concat!(
            r#"{"error":{"message":[{"type":"value_error","loc":["body","reasoning_effort"],"#,
            r#""msg":"Value error, reasoning_effort must be one of: 'instant', 'low', "#,
            r#"'medium', 'high'","input":"banana","ctx":{"error":"reasoning_effort must be "#,
            r#"one of: 'instant', 'low', 'medium', 'high'"}}],"#,
            r#""type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#,
        );
        assert!(
            serde_json::from_str::<Value>(named).is_ok(),
            "the fixture has to be the shape the server actually sends"
        );
        let said = complaint(status, named);
        assert!(said.contains("reasoning_effort must be one of"), "{said}");
        for envelope in ["loc", "value_error", "ctx", "invalid_request_error"] {
            assert!(!said.contains(envelope), "{envelope} is machinery: {said}");
        }

        // the same shape, for a failure whose own message names nothing. `loc` is the only thing
        // that says what it was about
        let unnamed = concat!(
            r#"{"error":{"message":[{"type":"bool_parsing","loc":["body","reasoning_summary"],"#,
            r#""msg":"Input should be a valid boolean, unable to interpret input","#,
            r#""input":"banana"}],"type":"invalid_request_error"}}"#,
        );
        let said = complaint(status, unnamed);
        assert!(
            said.contains("reasoning_summary") && said.contains("valid boolean"),
            "the parameter it is about is named: {said}"
        );

        // two at once are two sentences, and a list with nothing readable in it falls through to
        // the clip rather than reporting an empty refusal
        let two = concat!(
            r#"{"error":{"message":[{"loc":["body","a"],"msg":"first"},"#,
            r#"{"loc":["body","b"],"msg":"second"}]}}"#,
        );
        let said = complaint(status, two);
        assert!(
            said.contains("a: first") && said.contains("b: second"),
            "{said}"
        );
        assert!(complaint(status, r#"{"error":{"message":[]}}"#).contains("message"));
    }

    /// The shape a stream that fails halfway sends, which is not the shape a refused request
    /// sends. Copied out of a live session against `inception/mercury-2.5-preview`.
    #[test]
    fn an_error_that_arrives_mid_stream_is_read_the_same_way() {
        let midstream = concat!(
            r#"{"code":502,"message":"Upstream error from Inception: I'm sorry, but I can't"#,
            r#" share details of my architecture or training process.","metadata":"#,
            r#"{"error_type":"provider_unavailable"}}"#,
        );
        let value: Value =
            serde_json::from_str(midstream).expect("the shape it actually arrives in");

        // `message` at the top, with no `error` around it - read only the nested one and this
        // whole envelope went to the screen
        let sentence = said(&value).expect("there is a sentence in there");
        assert!(
            sentence.starts_with("Upstream error from Inception"),
            "{sentence}"
        );
        assert!(!sentence.contains("error_type"), "{sentence}");
        assert!(!sentence.contains('{'), "no envelope: {sentence}");

        // and the nested shape still reads, so one function serves both paths
        let nested: Value = serde_json::from_str(r#"{"error":{"message":"nested"}}"#).unwrap();
        assert_eq!(said(&nested).as_deref(), Some("nested"));

        // something with no sentence in it at all has nothing to hand back
        let empty: Value = serde_json::from_str(r#"{"code":502}"#).unwrap();
        assert_eq!(said(&empty), None);
    }
}
