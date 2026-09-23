//! What comes back from a request: a stream read an event at a time, and a server's own account of
//! why it would not answer.
//!
//! note: shared by both dialects, like [`waiting`](crate::waiting). What an event *says* differs
//! between them, and each answers that through [`Events`]; getting events off a socket - a chunk
//! that splits a character, a last line with no newline after it, a body that was never a stream,
//! a stream that stops or goes quiet - is the same problem in both, and solved twice it gets fixed
//! in one copy at a time.

use nachalnik::{BoxError, DeltaSink};
use serde_json::Value;

use crate::{
    markup::unmarked,
    refused,
    waiting::{
        Asking, HEARTBEAT, LARGEST, PATIENCE, Silence, Vigil, gone_quiet, stalled, too_large,
    },
};

/// What one dialect makes of the events in a stream.
pub(crate) trait Events {
    /// Takes one event, handing whatever it carries to `deltas` as it goes.
    fn event(&mut self, event: &Value, deltas: &DeltaSink);

    /// Whether the server has already said why the turn ended.
    fn finished(&self) -> bool;
}

/// How a stream that carried something came to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stopped {
    /// The server ended it.
    Ended,
    /// Somebody asked it to stop.
    Interrupted,
    /// The body stopped arriving before the server had said the turn was over.
    CutOff,
}

/// A response, read to its end.
pub(crate) enum Read {
    /// Every event the server sent, verbatim, and how the stream stopped.
    Events(Vec<Value>, Stopped),
    /// Somebody asked to stop before anything had arrived.
    Interrupted,
    /// A body in which nothing was an event and which was not an error object either: the whole
    /// of it, for the dialect to read as a whole answer if it is one.
    Unstreamed(String),
}

/// Reads a stream to its end, handing each event to `events` as it arrives.
///
/// note: what stops this is the server, an interrupt, or the stall watch - and the first two of
/// those are answers rather than failures, which is why what comes back is what had arrived rather
/// than an error. A turn that was cut off mid-answer has been generated and billed for; throwing it
/// away is the one thing worse than reporting it short.
pub(crate) async fn read(
    response: &mut reqwest::Response,
    asking: &Asking<'_>,
    limit: Option<usize>,
    events: &mut impl Events,
) -> Result<Read, BoxError> {
    // bytes rather than a `String`, because a chunk boundary is not a character boundary: the
    // tail of a split character waits in here for the rest of itself, and only whole lines are
    // decoded
    let mut buffer: Vec<u8> = Vec::new();
    // every byte, until one of them is part of an event: a body that never was a stream is read
    // whole, rather than as whatever followed its last newline
    let mut unstreamed: Vec<u8> = Vec::new();
    let mut seen: Vec<Value> = Vec::new();
    let mut stopped = Stopped::Ended;
    let mut vigil = Vigil::new();

    loop {
        // the timeout is what makes a model that says nothing at all interruptible; without it
        // this sits in `chunk` until the server feels like talking
        let ended = match tokio::time::timeout(HEARTBEAT, response.chunk()).await {
            Ok(Ok(Some(bytes))) => {
                if vigil.heard() {
                    asking.say(format!("{} is answering again", asking.model));
                }
                if seen.is_empty() {
                    if unstreamed.len() + bytes.len() > LARGEST {
                        return Err(too_large(asking.model));
                    }
                    unstreamed.extend_from_slice(&bytes);
                }
                buffer.extend_from_slice(&bytes);
                false
            }
            // a last line with no newline after it is still a line, and the end of the body is
            // what ends it
            Ok(Ok(None)) => {
                buffer.push(b'\n');
                true
            }
            // the body stopped arriving in the middle of an answer. Everything parsed so far is
            // kept and the socket is abandoned, as the interrupt does for the other way a stream
            // ends early. Not retried: every attempt is billed, and an answer that reliably
            // outruns an upstream's patience would be paid for four times and fail anyway
            Ok(Err(e)) => {
                // nothing arrived at all, so there is nothing to keep and the transport's own
                // account is the most useful thing there is
                if seen.is_empty() {
                    return Err(e.into());
                }
                // a turn whose finish already arrived is a complete answer that lost its trailing
                // bytes, and calling that cut off would be inventing a fault
                if !events.finished() {
                    asking.say(format!(
                        "{} was cut off mid-answer ({e}); what had arrived is kept",
                        asking.model
                    ));
                    stopped = Stopped::CutOff;
                }
                break;
            }
            Err(_) => {
                if asking.deltas.is_interrupted() {
                    stopped = Stopped::Interrupted;
                    break;
                }
                match vigil.waited() {
                    Silence::Enough => return Err(stalled(asking.model, PATIENCE)),
                    Silence::Worth(seconds) => asking.say(gone_quiet(asking.model, seconds)),
                    Silence::Ordinary => {}
                }
                continue;
            }
        };

        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            // checked before each event rather than after, so that an event is never read and
            // then thrown away
            if asking.deltas.is_interrupted() {
                stopped = Stopped::Interrupted;
                break;
            }

            let line = String::from_utf8_lossy(&buffer[..end]).trim().to_owned();
            buffer.drain(..=end);

            // `[DONE]` and comments are not JSON, and are skipped with everything else that is not
            let Some(event) = line
                .strip_prefix("data:")
                .and_then(|data| serde_json::from_str::<Value>(data.trim()).ok())
            else {
                continue;
            };
            // an upstream failure - a rate limit, a dead provider - reported as an event rather
            // than as a status, sometimes after the answer has started
            if let Some(error) = event.get("error").filter(|error| !error.is_null()) {
                return Err(refused(failure(error), limit));
            }

            events.event(&event, asking.deltas);
            if seen.is_empty() {
                unstreamed = Vec::new();
            }
            seen.push(event);
        }

        // what is left is a line not yet ended, and one that never ends is not read for ever: what
        // arrived before it is kept, as for a stream cut off
        if buffer.len() > LARGEST {
            if seen.is_empty() {
                return Err(too_large(asking.model));
            }
            asking.say(format!(
                "{} sent more than {} MiB without ending a line; what had arrived is kept",
                asking.model,
                LARGEST >> 20
            ));
            stopped = Stopped::CutOff;
            break;
        }
        // and asked after every chunk as well as in the quiet and between lines, since a body
        // that trickles without ending a line is none of those
        if asking.deltas.is_interrupted() {
            stopped = Stopped::Interrupted;
        }
        if ended || stopped == Stopped::Interrupted {
            break;
        }
    }

    if !seen.is_empty() {
        return Ok(Read::Events(seen, stopped));
    }
    // a request stopped before the server had said anything is not a broken response, and
    // reporting it as one would put a red line on the screen for doing what was asked
    if stopped == Stopped::Interrupted || asking.deltas.is_interrupted() {
        return Ok(Read::Interrupted);
    }

    // the usual reason a good status carries no stream: a failure, reported as a body
    let body = String::from_utf8_lossy(&unstreamed).into_owned();
    if let Ok(payload) = serde_json::from_str::<Value>(&body)
        && let Some(error) = payload.get("error").filter(|error| !error.is_null())
    {
        return Err(refused(failure(error), limit));
    }

    Ok(Read::Unstreamed(body))
}

/// The error for a body that was neither a stream nor anything a dialect could read whole.
pub(crate) fn not_a_stream(body: &str) -> BoxError {
    let short: String = unmarked(body).chars().take(300).collect();
    match short.is_empty() {
        true => "the stream carried no data".into(),
        false => format!("the stream carried no data: {short}").into(),
    }
}

/// What to say about an error object: the sentence in it, or failing that the start of it.
pub(crate) fn failure(error: &Value) -> String {
    said(error).unwrap_or_else(|| error.to_string().chars().take(300).collect())
}

/// What to say about a request the server refused: its own sentence, rather than its envelope.
///
/// note: a spent quota comes back as an envelope of JSON - the message, the remedy, the rate-limit
/// headers, and the account's `user_id` - and all of it would go into the transcript and into the
/// session log, which is a file people send each other. What a reader needs is the sentence.
/// Nobody needs an identifier for their account written into it.
pub(crate) fn complaint(status: reqwest::StatusCode, body: &str) -> String {
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

/// The sentence inside an error object, wherever the server put it.
///
/// note: two shapes, and one endpoint can send both. A refused request nests it under `error`; a
/// stream that fails halfway sends the object on its own, with `message` at the top. Google's
/// nests it too, beside a `status` and a list of `details` that are not prose.
fn said(value: &Value) -> Option<String> {
    let error = match value.get("error").filter(|error| !error.is_null()) {
        Some(nested) => nested,
        None => value,
    };
    let message = match error["message"].as_str() {
        Some(message) => message.trim().to_owned(),
        // a third shape, and the one where the sentence matters most: see `refusals`
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

/// The sentences inside a rejected request's list of what was wrong with it, each named by the
/// parameter it is about.
///
/// note: a validated endpoint answers a bad parameter with a *list* rather than a sentence -
/// Inception's `message` is the pydantic shape, `[{"type": "value_error", "loc": ["body",
/// "reasoning_effort"], "msg": "...", "input": "banana", "ctx": {...}}]` - and the answer to what
/// a request got wrong is where the words are most worth having: which parameter, and what it
/// takes.
///
/// note: `loc` is prepended only where the message does not already name the field. Pydantic's
/// own wording varies on exactly that point - a `value_error` raised by a validator usually names
/// it, a type failure says "Input should be a valid boolean" and names nothing - and a message
/// that does not say which parameter it means is a message somebody has to guess at.
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

#[cfg(test)]
mod tests {
    use super::*;

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

        // a whole web page, which is what a base URL pointing at a site answers with. Its words
        // are the useful part and its stylesheet is not
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

    /// A refusal for length comes back as the numbers in it, not only as the sentence.
    ///
    /// note: tested through the whole seam, because the reading has to survive what this crate
    /// does to a body on the way - and what it does is put the status in front of it, which is a
    /// number in the message that is not a token count. The fixture is what `openrouter.ai`
    /// answers, and it sends the same complaint both as a status and as an error object inside a
    /// perfectly good 200.
    #[test]
    fn a_refusal_for_length_keeps_the_numbers_that_say_what_to_do_about_it() {
        let refused_as = |said: String| {
            let error = crate::refused(said, Some(262_144));
            nachalnik::TooLong::of(&*error).map(|too_long| too_long.overrun)
        };

        let body = concat!(
            r#"{"error":{"message":"The request is 286315 tokens long and exceeds this model's "#,
            r#"context length of 262144 tokens.","type":"invalid_request_error","param":"","#,
            r#""code":"context_length_exceeded"}}"#,
        );
        assert!(
            serde_json::from_str::<Value>(body).is_ok(),
            "the fixture has to be the shape the server actually sends"
        );

        let overrun = refused_as(complaint(reqwest::StatusCode::BAD_REQUEST, body))
            .expect("a refusal for length");
        assert_eq!(overrun.tokens, 286_315);
        assert_eq!(overrun.limit, Some(262_144));

        // the same complaint arriving as an error object rather than as a status, which is the
        // other seam and the other reader
        let object: Value = serde_json::from_str(body).unwrap();
        let overrun = refused_as(failure(&object["error"]))
            .expect("a refusal for length, wherever the server put it");
        assert_eq!(overrun.tokens, 286_315);

        // and everything else is still a sentence
        assert_eq!(
            refused_as(complaint(
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":{"message":"rate-limited upstream"}}"#
            )),
            None
        );
    }

    /// note: the two wordings pydantic gives the same kind of failure, which is why `loc` is read.
    /// A validator that raised the error usually names the parameter in the message; a type
    /// failure says "Input should be a valid boolean" and names nothing. Both are quoted from what
    /// `api.inceptionlabs.ai` answers.
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
        // whole envelope goes to the screen
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
