//! OpenRouter's Batch API, for a model named by its `:batch` variant.
//!
//! note: a `:batch` endpoint is not reachable through chat completions at all - OpenRouter refuses
//! one there with a 404 - and it answers asynchronously, within a day. So a request to one goes out
//! as a batch of one: submitted to `/batches`, polled until it ends, and read as the whole answer
//! it carries. What the conversation gets is the `ModelResponse` a request asked for whole would
//! give, later.
//!
//! note: OpenRouter's alone. The suffix is its naming, and on another host `:batch` is part of a
//! model's name like any other, so an address that is not OpenRouter's is sent to chat completions
//! as it always was.

use std::{
    sync::atomic::AtomicUsize,
    time::{Duration, Instant},
};

use nachalnik::{BoxError, ModelResponse};
use serde_json::{Value, json};

use super::{
    OpenAiCompatible,
    wire::{Streamed, completion, not_a_completion, whole},
};
use crate::waiting::{Asking, Sent, backed_off, interrupted, sent};

/// What the one request is called inside its batch.
const ONE: &str = "request";

/// How long the first wait between two looks at a batch is; each one after doubles, up to
/// [`LONGEST`].
///
/// note: shorter under test, where the batch is a local server's and the waits are all there is
/// to wait for.
const FIRST: Duration = match cfg!(test) {
    true => Duration::from_millis(5),
    false => Duration::from_secs(2),
};

/// The longest wait between two looks at a batch.
///
/// note: a batch may take the whole of its day, and a look is a request of its own. Half a minute
/// is soon enough for one that finishes in minutes and few enough looks for one that does not.
const LONGEST: Duration = match cfg!(test) {
    true => Duration::from_millis(20),
    false => Duration::from_secs(30),
};

/// How long after a batch is accepted a look at it may be refused before the refusal is believed.
///
/// note: OpenRouter answers a look at a batch it accepted a moment ago with a 404 naming it, and
/// the same look a few seconds later with the batch. Until one look has seen it, a refused look is
/// taken for that; after this long, or once one has, a refusal is the answer.
const UNSEEN: Duration = Duration::from_secs(60);

/// Whether a request for this model at this address goes through the Batch API.
pub(super) fn batched(endpoint: &str, model: &str) -> bool {
    crate::is_openrouter(endpoint) && model.ends_with(":batch")
}

/// The Batch API's body for one chat-completions request.
///
/// note: the chat body with three fields taken out. A batch refuses `stream: true`, the answer
/// arrives whole so `stream_options` asks for nothing, and the model is the batch's: a body that
/// named one would have to name the same one, and leaving it out is how a body inherits it.
///
/// note: `endpoint` and `model` go before `requests`, which the API requires because it reads the
/// body as a stream. `serde_json` writes a map's keys in the order they went in with its
/// `preserve_order` feature and sorted by name without it, and these three come out in the
/// order the API needs either way.
pub(super) fn submission(mut body: Value, model: &str) -> Value {
    if let Some(fields) = body.as_object_mut() {
        for field in ["stream", "stream_options", "model"] {
            fields.remove(field);
        }
    }

    json!({
        "endpoint": "/v1/chat/completions",
        "model": model,
        "requests": [{ "custom_id": ONE, "body": body }],
    })
}

impl OpenAiCompatible {
    /// Submits a rendered batch, waits for it to end, and reads the answer out of it.
    ///
    /// note: `sent` does both halves, so a busy server is waited out on the submission and on
    /// every look the same way a chat request waits one out, and an interrupt is heard in either.
    /// Only the submission is counted in [`OpenAiCompatible::attempts`]: a look at a batch is not a
    /// request for an answer.
    ///
    /// note: an interrupt stops the waiting and not the batch. The API has no way to cancel one,
    /// so it goes on and is billed if it completes, and the notice says which batch it was.
    pub(super) async fn batch(
        &self,
        body: &Value,
        asking: &Asking<'_>,
        limit: Option<usize>,
    ) -> Result<ModelResponse, BoxError> {
        let endpoint = self.endpoint();
        let submitting = || {
            self.attributed(
                self.client
                    .post(format!("{endpoint}/batches"))
                    .bearer_auth(&self.api_key),
            )
            .json(body)
        };
        let mut batch = match sent(
            asking,
            &self.attempts,
            limit,
            false,
            submitting,
            &mut Streamed::default(),
        )
        .await?
        {
            Sent::Whole(batch) => batch,
            _ => return Ok(interrupted()),
        };
        let Some(id) = batch["id"].as_str().map(str::to_owned) else {
            return Err(format!(
                "the Batch API took the request and named no batch: {}",
                crate::markup::quoted(&batch.to_string())
            )
            .into());
        };

        let looks = AtomicUsize::new(0);
        let (mut wait, mut said, began) = (FIRST, String::new(), Instant::now());
        let mut seen = false;
        loop {
            match batch["status"].as_str().unwrap_or_default() {
                "completed" => return self.answered(batch, &id),
                status @ ("failed" | "expired" | "cancelled") => {
                    return Err(ended(&batch, &id, status));
                }
                // said when it changes rather than at every look: a batch can be in progress for
                // hours, and a line every half minute would be most of what the session said
                status => {
                    if status != said {
                        asking.say(format!(
                            "{} answers through a batch, `{id}`, which is {}; waiting for it",
                            asking.model,
                            match status {
                                "" => "in a state nobody named",
                                status => status,
                            }
                        ));
                        said = status.to_owned();
                    }
                }
            }
            if !backed_off(wait, asking.deltas).await {
                return Ok(abandoned(asking, &id, began));
            }
            wait = (wait * 2).min(LONGEST);

            let looking = || {
                self.attributed(
                    self.client
                        .get(format!("{endpoint}/batches/{id}"))
                        .bearer_auth(&self.api_key),
                )
            };
            batch = match sent(
                asking,
                &looks,
                limit,
                false,
                looking,
                &mut Streamed::default(),
            )
            .await
            {
                Ok(Sent::Whole(batch)) => {
                    seen = true;
                    batch
                }
                Ok(_) => return Ok(abandoned(asking, &id, began)),
                Err(_) if !seen && began.elapsed() < UNSEEN => continue,
                Err(e) => return Err(format!("batch `{id}`: {e}").into()),
            };
        }
    }

    /// The answer a completed batch holds for its one request.
    ///
    /// note: the batch's usage stands in for the answer's where the answer carries none. A batch
    /// of one request cost what that request cost, and a turn recorded with its cost unknown is a
    /// turn nobody can count against a ceiling.
    fn answered(&self, mut batch: Value, id: &str) -> Result<ModelResponse, BoxError> {
        let Some(result) = batch["results"]
            .as_array_mut()
            .and_then(|results| results.iter_mut().find(|result| result["custom_id"] == ONE))
        else {
            return Err(format!("batch `{id}` completed with no answer in it").into());
        };
        if let Some(error) = result["error"].as_object() {
            let why = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("it gave no reason");
            return Err(format!("batch `{id}` completed, and its request failed: {why}").into());
        }

        let mut body = result["response"]["body"].take();
        if !completion(&body) {
            return Err(not_a_completion(&body));
        }
        if body["usage"].is_null() && !batch["usage"].is_null() {
            body["usage"] = batch["usage"].take();
        }

        Ok(whole(body, self.thinking_in_content))
    }
}

/// Why a batch ended without an answer, in its own words where it gave some.
fn ended(batch: &Value, id: &str, status: &str) -> BoxError {
    match batch["error"]["message"].as_str() {
        Some(why) => format!("batch `{id}` {status}: {why}").into(),
        None => format!("batch `{id}` {status}, and said nothing about why").into(),
    }
}

/// An interrupt heard while a batch was still running, said, and answered as one.
fn abandoned(asking: &Asking<'_>, id: &str, began: Instant) -> ModelResponse {
    asking.say(format!(
        "stopped waiting for batch `{id}` after {}s; it goes on at OpenRouter, where there is no \
         way to cancel it, and is billed if it completes",
        began.elapsed().as_secs()
    ));

    interrupted()
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc};

    use nachalnik::{Config, ContextItem, Kernel};
    use parking_lot::Mutex;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    /// Answers the Batch API's paths in order - the submission, then each look at the batch as
    /// `looks` says it goes - and keeps the body the submission carried.
    async fn batch_api(looks: Vec<(u16, Value)>) -> (SocketAddr, Arc<Mutex<Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let at = listener.local_addr().expect("its address");
        let submitted = Arc::new(Mutex::new(Value::Null));
        let keeping = submitted.clone();

        tokio::spawn(async move {
            let mut looks = looks.into_iter();
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut read = Vec::new();
                let mut chunk = [0u8; 8192];
                let (head, body) = loop {
                    if let Some(end) = read.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&read[..end]).into_owned();
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|n| n.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if read.len() >= end + 4 + length {
                            break (head, read[end + 4..end + 4 + length].to_vec());
                        }
                    }
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break (String::new(), Vec::new()),
                        Ok(n) => read.extend_from_slice(&chunk[..n]),
                    }
                };

                let (status, answer) = match head.starts_with("POST /api/v1/batches ") {
                    true => {
                        *keeping.lock() = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        (
                            202,
                            json!({ "id": "b1", "object": "batch", "status": "validating" }),
                        )
                    }
                    false => looks.next().unwrap_or((500, json!({}))),
                };
                let answer = answer.to_string();
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                            answer.len()
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            }
        });

        (at, submitted)
    }

    /// A provider for a `:batch` model that believes it is talking to OpenRouter and is talking
    /// to `at`; see `tests/attribution.rs`, which does the same for the headers.
    fn batching(at: SocketAddr) -> Arc<OpenAiCompatible> {
        crate::install_crypto();
        let client = reqwest::Client::builder()
            .resolve("openrouter.ai", at)
            .build()
            .expect("a client that resolves one name itself");

        Arc::new(
            OpenAiCompatible::new(
                "z-ai/glm-5.3-flash:batch",
                format!("http://openrouter.ai:{}/api/v1", at.port()),
                "k",
            )
            .with_client(client),
        )
    }

    fn completed(content: &str) -> Value {
        json!({
            "id": "b1",
            "status": "completed",
            "usage": { "prompt_tokens": 20, "completion_tokens": 7, "total_tokens": 27 },
            "results": [{
                "id": "batch_req_1",
                "custom_id": ONE,
                "response": {
                    "status_code": 200,
                    "body": {
                        "object": "chat.completion",
                        "choices": [{
                            "index": 0,
                            "message": { "role": "assistant", "content": content },
                            "finish_reason": "stop",
                        }],
                    },
                },
                "error": null,
            }],
            "error": null,
        })
    }

    /// A `:batch` model is asked through the Batch API, waited for, and answered from its result.
    ///
    /// note: the first look is refused with the 404 OpenRouter gives a batch it accepted a moment
    /// ago, which is what a live run met on its first look and failed on.
    #[tokio::test]
    async fn a_batch_model_is_asked_through_the_batch_api_and_waited_for() {
        let (at, submitted) = batch_api(vec![
            (
                404,
                json!({ "error": { "code": 404, "message": "Batch job b1 not found." } }),
            ),
            (200, json!({ "id": "b1", "status": "in_progress" })),
            (200, completed("answered, eventually")),
        ])
        .await;
        let provider = batching(at);
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(provider.clone());
        kernel.push(ContextItem::user("go"));

        kernel.step().await.expect("the batch was answered");

        let answer = kernel
            .items()
            .last()
            .expect("an answer")
            .content
            .to_text()
            .into_owned();
        assert_eq!(answer, "answered, eventually");
        let submitted = submitted.lock().clone();
        assert_eq!(submitted["endpoint"], "/v1/chat/completions");
        assert_eq!(submitted["model"], "z-ai/glm-5.3-flash:batch");
        let request = &submitted["requests"][0];
        assert_eq!(request["custom_id"], ONE);
        assert_eq!(request["body"]["messages"][0]["content"], "go");
        // what a batch refuses or supplies itself is not in the body it runs
        for field in ["stream", "stream_options", "model"] {
            assert!(request["body"].get(field).is_none(), "{field}: {submitted}");
        }
        // the batch's own figures, since the answer carried none
        let usage = kernel
            .last_response()
            .and_then(|r| r.usage)
            .expect("a cost");
        assert_eq!(usage.input_tokens, Some(20));
        // and the looks are not requests for an answer
        assert_eq!(provider.attempts(), 1);
    }

    /// A batch that fails says which batch and why, rather than being taken for an answer.
    #[tokio::test]
    async fn a_failed_batch_says_which_and_why() {
        let (at, _) = batch_api(vec![(
            200,
            json!({ "id": "b1", "status": "failed", "error": null }),
        )])
        .await;
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(batching(at));
        kernel.push(ContextItem::user("go"));

        let failed = kernel
            .step()
            .await
            .expect_err("a failed batch is no answer");

        assert!(failed.to_string().contains("batch `b1` failed"), "{failed}");
    }

    /// Only OpenRouter's `:batch` is its Batch API; elsewhere the suffix is part of a name.
    #[test]
    fn only_openrouters_batch_variant_is_batched() {
        assert!(batched(
            "https://openrouter.ai/api/v1",
            "z-ai/glm-5.3-flash:batch"
        ));
        assert!(!batched(
            "https://openrouter.ai/api/v1",
            "z-ai/glm-5.3-flash"
        ));
        assert!(!batched("http://localhost:11434/v1", "qwen3:batch"));
    }
}
