//! Tests against a real model, over the real wire.
//!
//! They are skipped unless an API key is in the environment, the same variable the `compare` and
//! `panel` examples read:
//!
//! ```text
//! OPENROUTER_API_KEY=sk-or-... cargo test --test live -- --nocapture
//! ```
//!
//! ```text
//! NACHALNIK_TEST_MODEL=liquid/lfm-2.5-2.6b:free   # the default; anything with tool support
//! NACHALNIK_TEST_MODEL_B=...                      # a second model, for the swap test
//! NACHALNIK_BASE_URL=...                          # any OpenAI-compatible server
//! ```
//!
//! Google AI Studio speaks the same dialect, and has a free tier of its own:
//!
//! ```text
//! NACHALNIK_API_KEY=... \
//! NACHALNIK_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai \
//! NACHALNIK_TEST_MODEL=gemini-3.5-flash-lite \
//! NACHALNIK_TEST_MODEL_B=gemini-3.5-flash \
//!   cargo test --test live -- --test-threads=1 --nocapture
//! ```
//!
//! A local server works as well, and costs nothing:
//!
//! ```text
//! NACHALNIK_API_KEY=ollama \
//! NACHALNIK_BASE_URL=http://localhost:11434/v1 \
//! NACHALNIK_TEST_MODEL=granite4.2:3b \
//! NACHALNIK_TEST_MODEL_B=llama3.2 \
//!   cargo test --test live -- --test-threads=1
//! ```
//!
//! note: These tests need a model that can read its own tool results, which is a real bar and
//! not every small model clears it. Measured on the same machine, `granite4.2:3b` passes all
//! eighteen; `llama3.2` passes fifteen and fails three, because it calls the tool, is handed the
//! answer in the request, and then reports a different one it made up. That is a fair result for
//! a test suite whose subject is whether a real model's answers survive the round trip - the
//! failure is in the model, and the way to tell is that the projected messages in the panic
//! output contain the tool result the model claims it never saw.
//!
//! note: What these check is what a scripted provider cannot: that the requests the kernel
//! builds are accepted by a real API, that a real model's answers survive the round trip
//! through the context, and that the loop still works when a tool result is pruned, truncated
//! or compacted out from under it. They are deliberately assertion-light about *prose* and
//! assertion-heavy about structure.
//!
//! note: They run one at a time and retry on rate limits and on a busy upstream, so that neither
//! a free-tier limit nor somebody else's traffic is ever the thing under test. A whole run costs about twenty requests, and a key that has run out of
//! free requests for the day makes them skip rather than fail - the difference between the two
//! kinds of rate limit is in [`out_of_quota`].

use std::{
    env,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};

use nachalnik::{
    BoxError, BytesPerToken, Calibrating, Capability, Config, Content, ContextItem, ContextKind,
    ContextState, Delta, Event, Grant, Kernel, OutputSink, Params, Record, Role, State, StopReason,
    Tool, ToolCall, ToolOutput, ToolSpec, async_trait,
    selectors::Selector,
    test::{AllowAll, DenyAll, LargestFirstCompactor},
};
use nachalnik_utils::{OpenAiCompatible, out_of_quota};
use serde_json::{Value, json};
use tokio::sync::broadcast::Receiver;

/// A small, free, tool-capable model.
const DEFAULT_MODEL: &str = "liquid/lfm-2.5-2.6b:free";

/// Live tests take turns, so that the free tier's rate limit is not what is being measured.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn serialize() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

// ------------------------------------------------------------------------------- the provider

// note: `nachalnik-utils::OpenAiCompatible`, which is a workspace member that is never published
// and exists for exactly this: the provider these tests talk through, and the one the examples
// talk through, used to be two five-hundred-line copies of each other that had to be fixed twice.
// It records every request it sends, which is what most of the assertions below are about.

// ---------------------------------------------------------------------------------- the tools

/// A tool whose output cannot be guessed by a model, so seeing it in an answer proves the
/// result made the round trip.
struct Secret {
    output: String,
    limit: Option<usize>,
    ran: AtomicUsize,
}

impl Secret {
    fn new(output: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            output: output.into(),
            limit: None,
            ran: AtomicUsize::new(0),
        })
    }

    fn with_limit(output: impl Into<String>, limit: usize) -> Arc<Self> {
        Arc::new(Self {
            output: output.into(),
            limit: Some(limit),
            ran: AtomicUsize::new(0),
        })
    }

    fn ran(&self) -> usize {
        self.ran.load(SeqCst)
    }
}

#[async_trait]
impl Tool for Secret {
    fn spec(&self) -> ToolSpec {
        let spec = ToolSpec::new("secret", "returns today's secret code word")
            .with_capabilities([Capability::Read]);

        match self.limit {
            Some(limit) => spec.with_output_limit(limit),
            None => spec,
        }
    }

    async fn invoke(&self, _call: &ToolCall, output: OutputSink) -> Result<ToolOutput, BoxError> {
        self.ran.fetch_add(1, SeqCst);
        output.push("looking it up");

        Ok(ToolOutput::new(self.output.clone()))
    }
}

// -------------------------------------------------------------------------------- the fixtures

/// A kernel wired to a live provider, or `None` when there is no API key to use.
async fn live() -> Option<(Kernel, Arc<OpenAiCompatible>)> {
    live_with(Config::default()).await
}

/// The same, for a test that needs the kernel configured differently.
async fn live_with(config: Config) -> Option<(Kernel, Arc<OpenAiCompatible>)> {
    let model = env::var("NACHALNIK_TEST_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    // no key means no live tests, which is how this suite skips itself. `streaming(false)` because
    // most of these are about what goes out rather than how it comes back; the one that is about
    // streaming turns it on through `params`, the same way a user would
    let provider = Arc::new(
        OpenAiCompatible::from_env(OpenAiCompatible::client(), &model)
            .ok()?
            .labelled("openrouter")
            .streaming(false),
    );
    provider.probe().await;

    let kernel = Kernel::new(config);
    kernel.set_provider(provider.clone());
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_params(params(500));

    Some((kernel, provider))
}

/// Runs a turn; skips the test if the key has no free requests left today, and fails on any
/// other provider error.
macro_rules! turn {
    ($kernel:expr) => {
        match $kernel.turn().await {
            Ok(state) => state,
            Err(e) if out_of_quota(&e.to_string()) => {
                eprintln!("skipped: {e}");
                return;
            }
            Err(e) => panic!("{e}"),
        }
    };
}

/// Returns the fixtures, or skips the test.
macro_rules! live {
    () => {
        match live().await {
            Some(fixtures) => fixtures,
            None => {
                eprintln!("skipped: set OPENROUTER_API_KEY to run the live tests");
                return;
            }
        }
    };
}

/// Returns the fixtures with a configured kernel, or skips the test.
macro_rules! live_with {
    ($config:expr) => {
        match live_with($config).await {
            Some(fixtures) => fixtures,
            None => {
                eprintln!("skipped: set OPENROUTER_API_KEY to run the live tests");
                return;
            }
        }
    };
}

/// A reasoning model needs room to think before it says anything at all.
fn params(max_tokens: u64) -> Params {
    let mut params = Params::new();
    params.insert("max_tokens".into(), json!(max_tokens));
    params.insert("temperature".into(), json!(0));

    params
}

fn drain(events: &mut Receiver<Event>) -> Vec<Event> {
    let mut received = Vec::new();
    while let Ok(event) = events.try_recv() {
        received.push(event);
    }

    received
}

/// The text of the most recent answer, lowercased.
fn answer(kernel: &Kernel) -> String {
    kernel
        .last_response()
        .and_then(|response| response.content.clone())
        .map(|content| content.to_text().to_lowercase())
        .unwrap_or_default()
}

fn results(kernel: &Kernel) -> Vec<Arc<ContextItem>> {
    "kind:tool_result"
        .parse::<Selector>()
        .unwrap()
        .matches(&kernel.items())
        .into_iter()
        .filter_map(|id| kernel.item(id))
        .collect()
}

// ----------------------------------------------------------------------------------- the tests

#[tokio::test]
async fn a_turn_reaches_a_real_model_and_comes_back() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.push(ContextItem::user("Reply with the single word: pong"));

    // the request that is about to be sent, captured before it is
    let previewed = kernel.preview_request().unwrap();
    let state = turn!(kernel);

    let State::Finished { item, stop } = state else {
        panic!("no tools were offered, so there was nothing to decide: {state:?}")
    };

    // what was previewed is exactly what went out
    assert_eq!(provider.requests(), vec![previewed]);
    assert!(provider.attempts() >= 1, "the provider was really called");

    let response = kernel.last_response().unwrap();
    assert_eq!(response.stop, StopReason::EndTurn);
    assert_eq!(
        stop, response.stop,
        "the state carries the model's own reason"
    );
    assert!(!answer(&kernel).is_empty(), "{response:?}");
    assert!(response.raw.is_some(), "the provider's own payload is kept");

    // real numbers, reported by the provider, next to the kernel's own estimate
    let usage = response.usage.expect("usage");
    assert!(usage.input_tokens.unwrap_or(0) > 0, "{usage:?}");
    assert!(usage.output_tokens.unwrap_or(0) > 0, "{usage:?}");
    assert!(kernel.budget().context_tokens > 0);

    // and the answer is in the context, attributed to the model
    let recorded = kernel.item(item).unwrap();
    assert_eq!(recorded.source, "model");
    assert_eq!(recorded.kind.name(), "assistant_message");
    assert_eq!(
        recorded.content.to_text().to_lowercase(),
        answer(&kernel),
        "the recorded turn is the answer, not a summary of it"
    );
}

#[tokio::test]
async fn the_context_limit_is_the_providers_own_number() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    let info = kernel.model_info().unwrap();
    let limit = info
        .context_limit
        .expect("the provider reports a context length");
    assert!(limit >= 4_096, "{limit} is implausibly small");
    assert_eq!(kernel.budget().limit, Some(limit));
}

#[tokio::test]
async fn a_system_instruction_is_obeyed() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.push(ContextItem::system(
        "You always answer with exactly one lowercase word, and nothing else.",
    ));
    kernel.push(ContextItem::user("What colour is a ripe banana?"));

    turn!(kernel);

    let sent = &provider.requests()[0];
    assert_eq!(sent.messages[0].role, Role::System, "the role is mapped");
    assert_eq!(sent.messages[1].role, Role::User);
    assert!(
        answer(&kernel).contains("yellow"),
        "the instruction and the question both arrived: {:?}",
        answer(&kernel)
    );
}

#[tokio::test]
async fn a_reference_is_labelled_so_the_model_knows_what_it_is_reading() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.push(ContextItem::file(
        "recipe.txt",
        "The secret ingredient is tarragon.",
    ));
    kernel.push(ContextItem::user(
        "Which file did I give you? Answer with the file name only.",
    ));

    turn!(kernel);

    assert_eq!(
        provider.requests()[0].messages[0]
            .content
            .as_ref()
            .unwrap()
            .to_text(),
        "recipe.txt:\nThe secret ingredient is tarragon.",
    );
    assert!(
        answer(&kernel).contains("recipe"),
        "the label reached the model: {:?}",
        answer(&kernel)
    );
}

#[tokio::test]
async fn streaming_fragments_add_up_to_the_answer() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    let mut params = params(500);
    params.insert("stream".into(), json!(true));
    params.insert("stream_options".into(), json!({ "include_usage": true }));
    kernel.set_params(params);
    kernel.push(ContextItem::user("Count from one to five, in words."));

    let mut events = kernel.subscribe();
    turn!(kernel);

    let mut streamed = String::new();
    let mut reasoned = 0;
    for event in drain(&mut events) {
        match event {
            Event::ModelDelta {
                delta: Delta::Text(fragment),
            } => streamed.push_str(&fragment),
            Event::ModelDelta {
                delta: Delta::Reasoning(_),
            } => reasoned += 1,
            _ => {}
        }
    }

    let response = kernel.last_response().unwrap();
    assert!(!streamed.is_empty(), "fragments arrived as events");
    assert_eq!(
        Some(Content::text(streamed)),
        response.content,
        "the fragments are the answer, in order"
    );
    assert!(response.usage.is_some(), "usage survives a stream");
    eprintln!("  ({reasoned} reasoning fragments)");
}

#[tokio::test]
async fn a_model_calls_a_tool_and_sees_its_result() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    let secret = Secret::new("The secret code word is APRICOT.");
    kernel.add_tool(secret.clone());
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));

    let state = turn!(kernel);
    assert!(
        matches!(state, State::Finished { .. }),
        "the policy allowed everything: {state:?}"
    );

    assert_eq!(secret.ran(), 1, "the tool ran exactly once");
    assert_eq!(provider.requests().len(), 2, "call, then result");

    // the second request carries the assistant's call and the tool's answer
    let second = &provider.requests()[1];
    let assistant = second
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .expect("the model's turn");
    assert_eq!(assistant.tool_calls.len(), 1);
    let result = second
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the tool's result");
    assert_eq!(
        result.tool_call_id,
        Some(assistant.tool_calls[0].id.clone())
    );
    assert_eq!(result.name.as_deref(), Some("secret"));

    assert!(
        answer(&kernel).contains("apricot"),
        "the model read the result: {:?}",
        answer(&kernel)
    );

    // and the exchange is in the context, in order
    let kinds: Vec<_> = kernel.items().iter().map(|i| i.kind.name()).collect();
    assert_eq!(
        kinds,
        [
            "user_message",
            "assistant_message",
            "tool_result",
            "assistant_message"
        ]
    );
}

#[tokio::test]
async fn asking_pauses_the_loop_and_the_answer_resumes_it() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();
    // the default policy asks about everything
    kernel.set_policy(Arc::new(nachalnik::AskAlways));

    let secret = Secret::new("The secret code word is APRICOT.");
    kernel.add_tool(secret.clone());
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));

    let State::Deciding { calls } = turn!(kernel) else {
        panic!("a real model asked for a tool, so a real decision is needed")
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(
        secret.ran(),
        0,
        "nothing ran while it was being asked about"
    );
    assert_eq!(provider.requests().len(), 1);

    let request = &kernel.pending_permissions()[0];
    assert_eq!(request.tool, "secret");
    assert_eq!(request.capabilities, vec![Capability::Read]);

    assert!(matches!(
        kernel.decide(request.id, Grant::Allow).unwrap(),
        State::Ready { .. }
    ));
    assert!(matches!(turn!(kernel), State::Finished { .. }));
    assert_eq!(secret.ran(), 1);
    assert!(answer(&kernel).contains("apricot"), "{:?}", answer(&kernel));
}

#[tokio::test]
async fn a_refused_call_never_runs_and_the_model_carries_on() {
    let _serial = serialize().await;
    let (kernel, _) = live!();
    kernel.set_policy(Arc::new(DenyAll));

    let secret = Secret::new("The secret code word is APRICOT.");
    kernel.add_tool(secret.clone());
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));

    let state = turn!(kernel);

    assert_eq!(
        secret.ran(),
        0,
        "a model cannot talk its way past the policy"
    );
    let refusal = results(&kernel);
    assert_eq!(refusal.len(), 1);
    assert!(
        refusal[0].content.to_text().contains("not permitted"),
        "{:?}",
        refusal[0]
    );
    assert!(
        matches!(
            refusal[0].kind,
            ContextKind::ToolResult { is_error: true, .. }
        ),
        "the model is told, as an error"
    );
    assert!(
        matches!(state, State::Finished { .. }),
        "and it answered anyway: {state:?}"
    );
    assert!(!answer(&kernel).contains("apricot"));
}

#[tokio::test]
async fn pruning_a_tool_exchange_still_produces_a_request_the_api_accepts() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.add_tool(Secret::new("The secret code word is APRICOT."));
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));
    turn!(kernel);

    // out goes the tool result, leaving the model's call without an answer
    let noisy: Vec<_> = results(&kernel).iter().map(|i| i.id).collect();
    assert_eq!(
        kernel
            .set_state(noisy, ContextState::Excluded, Some("noise".into()))
            .len(),
        1
    );
    let repairs = kernel.project().repairs;
    assert_eq!(repairs.len(), 1, "{repairs:?}");

    kernel.push(ContextItem::user("What did I just ask you about?"));
    let state = turn!(kernel);

    // the API accepted the repaired projection - this is the assertion a mock cannot make
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    let last = provider.requests().pop().unwrap();
    assert!(
        !last.messages.iter().any(|m| m.role == Role::Tool),
        "the pruned result is gone: {:?}",
        last.messages
    );
    assert!(
        last.messages.iter().all(|m| m.tool_calls.is_empty()),
        "and so is the call it answered: {:?}",
        last.messages
    );
    assert!(!answer(&kernel).is_empty());
}

/// The other half of the test above, and the reason [`ContextState::Elided`] exists: pruning a
/// tool result forces the projector to take the call down with it, so the model is handed a
/// conversation in which it never asked for anything. Eliding leaves the call answered.
///
/// note: the assertion a mock cannot make is that a *real* API accepts a tool message whose
/// content is a marker rather than the tool's own output - and, separately, that a real model
/// reads it as "this happened and I cannot see it" rather than as the answer itself. The second
/// one is checked the only way prose can be: the code word was in the result, the marker replaced
/// it, so a model repeating the code word is reading something that is no longer in the request.
#[tokio::test]
async fn eliding_a_tool_result_keeps_the_call_and_the_api_accepts_it() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    kernel.add_tool(Secret::new("The secret code word is APRICOT."));
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));
    turn!(kernel);
    assert!(
        answer(&kernel).to_lowercase().contains("apricot"),
        "the model read the result the first time: {}",
        answer(&kernel)
    );

    // the content goes, the call keeps its answer
    let noisy: Vec<_> = results(&kernel).iter().map(|i| i.id).collect();
    assert_eq!(
        kernel
            .set_state(
                noisy,
                ContextState::Elided,
                Some("removed from view by the user".into())
            )
            .len(),
        1
    );
    let projection = kernel.project();
    assert!(
        projection.repairs.is_empty(),
        "nothing had to be repaired: {:?}",
        projection.repairs
    );

    kernel.push(ContextItem::user(
        "What is the code word? If you cannot see it any more, say NOT VISIBLE and nothing else.",
    ));
    let state = turn!(kernel);

    // the API accepted a tool message carrying a marker instead of the tool's output
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    let last = provider.requests().pop().unwrap();
    let tool_messages: Vec<_> = last
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .collect();
    assert_eq!(
        tool_messages.len(),
        1,
        "the result is still there, as a message: {:?}",
        last.messages
    );
    assert!(
        tool_messages[0]
            .content
            .as_ref()
            .is_some_and(|c| c.to_text().contains("removed from view by the user")),
        "carrying the marker: {:?}",
        tool_messages[0].content
    );
    assert!(
        !tool_messages[0]
            .content
            .as_ref()
            .is_some_and(|c| c.to_text().contains("APRICOT")),
        "and not the content: {:?}",
        tool_messages[0].content
    );
    assert!(
        last.messages.iter().any(|m| !m.tool_calls.is_empty()),
        "and the call that asked for it is still on the record: {:?}",
        last.messages
    );

    // and the model read the marker for what it is rather than inventing the answer
    let said = answer(&kernel);
    assert!(!said.is_empty());
    assert!(
        !said.to_lowercase().contains("apricot"),
        "the code word is not in the request any more, so it should not be in the answer: {said}"
    );
}

#[tokio::test]
async fn a_truncated_tool_result_is_still_a_valid_request() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    let noisy = format!(
        "{}\nThe secret code word is APRICOT.",
        "noise ".repeat(4_000)
    );
    kernel.add_tool(Secret::with_limit(noisy, 200));
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me what it returned.",
    ));

    let state = turn!(kernel);
    assert!(matches!(state, State::Finished { .. }), "{state:?}");

    // the pair: the whole of what the tool said, archived, and the copy the model was shown
    let recorded = results(&kernel);
    assert_eq!(recorded.len(), 2, "a limit shortens; it does not destroy");

    let (whole, shown) = (&recorded[0], &recorded[1]);
    assert_eq!(whole.state, ContextState::Archived);
    assert!(whole.content.to_text().contains("APRICOT"));
    assert!(whole.content.to_text().len() > 20_000);

    assert!(shown.content.to_text().len() < 400, "it was truncated");
    assert!(shown.content.to_text().contains("bytes truncated"));
    assert!(
        shown
            .note
            .as_deref()
            .unwrap_or_default()
            .contains("truncated"),
        "and the item says so: {:?}",
        shown.note
    );

    // and only the short one crossed the wire, once
    let sent = provider.requests().pop().unwrap();
    let tool_messages: Vec<_> = sent
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .collect();
    assert_eq!(tool_messages.len(), 1, "the archived copy is not sent");
    assert!(tool_messages[0].content.as_ref().unwrap().to_text().len() < 400);
}

#[tokio::test]
async fn compaction_before_a_request_produces_a_request_the_api_accepts() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    // a compactor that always fires, so the real context limit does not have to be reached
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.0,
        target: 0.0,
    })));
    kernel.push(ContextItem::tool_result(
        "call_0".into(),
        "cargo",
        format!("{}\n", "warning: unused variable\n".repeat(200)),
        false,
    ));
    kernel.push(ContextItem::user("Say the word: ready"));

    let mut events = kernel.subscribe();
    let state = turn!(kernel);
    assert!(matches!(state, State::Finished { .. }), "{state:?}");

    let report = drain(&mut events)
        .into_iter()
        .find_map(|event| match event {
            Event::Compacted { report } => Some(report),
            _ => None,
        })
        .expect("it compacted");
    assert_eq!(report.removed.len(), 1);
    assert!(report.summary.is_some());
    assert!(report.tokens_after < report.tokens_before);

    // the request went out without the removed item, and with the summary in its place
    let sent = &provider.requests()[0];
    assert!(
        !sent.messages.iter().any(|m| m
            .content
            .as_ref()
            .unwrap()
            .to_text()
            .contains("unused variable")),
        "{:?}",
        sent.messages
    );
    assert!(!answer(&kernel).is_empty());
}

#[tokio::test]
async fn a_parameter_the_user_set_reaches_the_model() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();

    // one token is not enough to answer anything, and the server is the one that says so
    kernel.set_params(params(1));
    kernel.push(ContextItem::user("Write a paragraph about tarragon."));

    turn!(kernel);

    assert_eq!(provider.requests()[0].params["max_tokens"], json!(1));
    assert_eq!(
        kernel.last_response().unwrap().stop,
        StopReason::Length,
        "the limit was applied by the provider, not by the kernel"
    );
    assert!(
        matches!(
            kernel.state(),
            State::Finished {
                stop: StopReason::Length,
                ..
            }
        ),
        "and a truncated turn does not look like a clean one: {:?}",
        kernel.state()
    );
}

#[tokio::test]
async fn the_model_can_be_swapped_mid_session() {
    let _serial = serialize().await;
    let (kernel, first) = live!();

    kernel.push(ContextItem::user("Reply with the single word: one"));
    turn!(kernel);
    let before = answer(&kernel);

    // a second model if one was named, otherwise a second provider for the same one
    let model = env::var("NACHALNIK_TEST_MODEL_B")
        .or_else(|_| env::var("NACHALNIK_TEST_MODEL"))
        .unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let second = Arc::new(
        OpenAiCompatible::from_env(OpenAiCompatible::client(), &model)
            .expect("the key that got us this far")
            .labelled("openrouter")
            .streaming(false),
    );
    second.probe().await;

    let mut events = kernel.subscribe();
    let previous = kernel.set_provider(second.clone()).unwrap();
    assert_eq!(
        previous.info(),
        nachalnik::Provider::info(&*first),
        "the old one is handed back"
    );
    assert!(drain(&mut events).iter().any(|event| matches!(
        event,
        Event::ModelChanged { to: Some(info), .. } if info.model == model
    )));

    kernel.push(ContextItem::user("Now reply with the single word: two"));
    turn!(kernel);

    assert_eq!(
        first.requests().len(),
        1,
        "the first one was not used again"
    );
    assert_eq!(second.requests().len(), 1, "the second one was");
    assert!(
        second.requests()[0].messages.len() >= 3,
        "and it saw the earlier turn: {:?}",
        second.requests()[0].messages
    );
    assert!(!answer(&kernel).is_empty());
    assert_ne!(answer(&kernel), before);
}

#[tokio::test]
async fn a_real_session_survives_a_round_trip() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    kernel.add_tool(Secret::new("The secret code word is APRICOT."));
    kernel.push(ContextItem::user(
        "Use the secret tool, then tell me the code word it returned.",
    ));
    turn!(kernel);
    kernel.finish();

    let history = kernel.history();
    assert!(history.len() > 12, "{} records", history.len());
    assert_eq!(history[0].event.name(), "session.started");
    assert!(history.windows(2).all(|w| w[0].seq < w[1].seq));

    // the transitions a real tool round makes
    let states: Vec<_> = history
        .iter()
        .filter_map(|record| match &record.event {
            Event::StateChanged { to, .. } => Some(to.name()),
            _ => None,
        })
        .collect();
    assert_eq!(
        states,
        [
            "requesting",
            "ready",
            "executing",
            "idle",
            "requesting",
            "finished"
        ]
    );

    // and every record of it is exportable
    for record in &history {
        let json = serde_json::to_string(record).unwrap();
        let restored: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(&restored, record, "{json}");
    }
}

#[tokio::test]
async fn the_payload_that_was_recorded_is_the_one_that_went_out() {
    let _serial = serialize().await;
    let (kernel, provider) = live_with!(Config {
        record_payloads: true,
        ..Default::default()
    });

    kernel.push(ContextItem::user("Reply with the single word: pong"));

    // what a client would show somebody before letting the request go
    let previewed = kernel
        .preview_payload()
        .unwrap()
        .expect("this provider renders its own payload");

    turn!(kernel);

    let recorded: Vec<Value> = kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            Event::ModelPayload { payload } => Some(payload),
            _ => None,
        })
        .collect();

    assert_eq!(recorded.len(), 1, "one request, one payload");
    assert_eq!(
        recorded[0], previewed,
        "a preview that has quietly stopped matching is worse than none"
    );

    // and the payload really is an account of the request the provider was handed
    let sent = &provider.requests()[0];
    assert_eq!(
        recorded[0]["messages"].as_array().map(Vec::len),
        Some(sent.messages.len())
    );
    assert_eq!(
        recorded[0]["messages"][0]["content"].as_str(),
        sent.messages[0]
            .content
            .as_ref()
            .map(|c| c.to_text())
            .as_deref()
    );
}

#[tokio::test]
async fn a_step_abandoned_mid_request_leaves_the_kernel_usable() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    kernel.push(ContextItem::user(
        "Count from one to twenty, one number per line.",
    ));

    // a real request, dropped while the bytes are still moving. This is the case a scripted
    // provider cannot produce: there is no way to mock a socket that is genuinely half-read
    let abandoned = tokio::time::timeout(Duration::from_millis(100), kernel.step()).await;
    if abandoned.is_ok() {
        eprintln!("skipped: the request finished before it could be abandoned");
        return;
    }

    assert!(
        !kernel.state().is_busy(),
        "a dropped step must not leave the kernel stuck in {}",
        kernel.state()
    );
    assert_eq!(kernel.state(), State::Idle);
    assert!(
        kernel.last_response().is_none(),
        "nothing is recorded from a request that never came back"
    );
    assert_eq!(
        kernel.items().len(),
        1,
        "and the context is where it was: just the question"
    );

    // the point of not wedging is that the next one works
    let state = turn!(kernel);
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    assert!(!answer(&kernel).is_empty());
}

#[tokio::test]
async fn an_interrupted_turn_stops_between_requests() {
    let _serial = serialize().await;
    let (kernel, provider) = live!();
    let mut events = kernel.subscribe();

    kernel.push(ContextItem::user("Reply with the single word: pong"));
    kernel.interrupt();

    // interrupting is checked before each request a turn would make, so a turn interrupted
    // before it starts makes none at all
    let state = turn!(kernel);

    assert_eq!(state, State::Idle);
    assert!(
        provider.requests().is_empty(),
        "an interrupted turn does not talk to the model"
    );
    assert!(
        drain(&mut events)
            .iter()
            .any(|event| event.name() == "turn.interrupted"),
        "and it says so rather than looking like a turn that did nothing"
    );
    assert!(!kernel.is_interrupted(), "the flag is spent, not sticky");

    // the context is untouched, so carrying on is one call
    let state = turn!(kernel);
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    assert_eq!(provider.requests().len(), 1);
}

#[tokio::test]
async fn a_calibrating_counter_is_told_what_a_real_request_cost() {
    let _serial = serialize().await;
    let (kernel, _) = live!();
    let counter = Arc::new(Calibrating::new(BytesPerToken::default()));
    kernel.set_counter(counter.clone());

    // big enough to have a systematic error in it: a counter that learned from a ten-token
    // request would be learning from noise, and this one declines to
    kernel.push(ContextItem::file(
        "haystack.txt",
        "the quick brown fox jumps over the lazy dog. ".repeat(400),
    ));
    kernel.push(ContextItem::user("Reply with the single word: pong"));

    let guessed = kernel.budget().used();
    turn!(kernel);
    let charged = kernel
        .last_response()
        .unwrap()
        .usage
        .and_then(|usage| usage.input_tokens)
        .expect("the provider reports what it charged for") as usize;
    assert!(charged > 256, "the request has to be worth learning from");

    // it was told exactly what the kernel estimated and exactly what that cost - no rounding, no
    // averaging, nothing interpreted on the way. This is the part a scripted provider cannot
    // check: that the second number is a real one
    let learned = counter.calibration();
    assert_eq!(learned.observations, 1);
    assert_eq!(
        learned.estimated, guessed as u64,
        "the estimate it hears about is the one it made"
    );
    assert_eq!(learned.reported, charged as u64);

    // note: this deliberately does not assert that the estimate got *closer*. Whether it does
    // depends on how wrong the underlying counter happened to be for that one request - against a
    // short prompt `bytes / 4` can be exactly right, and a correction cannot improve on exact.
    // That the ratio converges is arithmetic, and it is pinned in `tests/tokens.rs`; what is
    // being checked here is that the arithmetic is fed real numbers.
    kernel.recount();
    kernel.push(ContextItem::user("And again, one word: pong"));
    let corrected = kernel.budget().used();
    turn!(kernel);

    let learned = counter.calibration();
    assert_eq!(learned.observations, 2);
    assert!(
        (0.1..=10.0).contains(&learned.scale),
        "a correction drawn from real requests should be a sane one, not {}",
        learned.scale
    );
    assert!(
        corrected > 0,
        "and it still produces a budget rather than zeroing one"
    );
}

#[tokio::test]
async fn an_interrupt_stops_a_stream_that_is_watching() {
    let _serial = serialize().await;
    let (kernel, _) = live!();

    // this provider streams when it is asked to, and an answer that arrives in one piece has no
    // middle to be interrupted in
    let mut params = params(500);
    params.insert("stream".into(), json!(true));
    params.insert("stream_options".into(), json!({ "include_usage": true }));
    kernel.set_params(params);

    let mut events = kernel.subscribe();
    kernel.push(ContextItem::user(
        "Count from one to two hundred, one number per line, and write nothing else.",
    ));

    // press the button the moment the first fragment arrives, from a task that is not the one
    // driving the loop - which is the only interesting case, and the reason `interrupt` exists
    let watcher = {
        let kernel = kernel.clone();
        tokio::spawn(async move {
            while let Ok(event) = events.recv().await {
                match event {
                    Event::ModelDelta { .. } => {
                        kernel.interrupt();
                        return true;
                    }
                    Event::ModelFinished { .. } | Event::ModelFailed { .. } => return false,
                    _ => {}
                }
            }

            false
        })
    };

    let state = turn!(kernel);
    if !watcher.await.unwrap_or(false) {
        eprintln!("skipped: the whole answer arrived before a fragment could be seen");
        return;
    }

    let State::Finished { stop, .. } = &state else {
        panic!("no tools were offered: {state:?}")
    };
    assert_eq!(
        stop,
        &StopReason::Other("interrupted".to_owned()),
        "the provider was watching, so it stopped rather than reading to the end"
    );

    // what had arrived is an ordinary item: partial, but real, and there to keep or to prune
    let answer = answer(&kernel);
    assert!(!answer.is_empty(), "it kept what it had");
    assert!(
        !answer.contains("200"),
        "and it really did stop early: {answer:?}"
    );
    assert_eq!(kernel.items().len(), 2);

    // the loop is at rest, and one transition attempt clears the outstanding interrupt
    assert!(!kernel.state().is_busy());
    kernel.step().await.unwrap();
    assert!(!kernel.is_interrupted());
}
