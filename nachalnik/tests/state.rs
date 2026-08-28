//! Tests for the state machine: who is allowed to drive the loop, and what happens if they
//! stop halfway.

mod common;

use std::{sync::Arc, time::Duration};

use common::{drain, inquisitive, permissive};
use nachalnik::{
    BoxError, Config, ContextItem, ContextState, DeltaSink, Error, Event, Grant, Kernel, ModelInfo,
    ModelRequest, ModelResponse, Provider, State, async_trait,
    test::{AllowAll, ConstTool, ScriptedProvider, call},
};
use parking_lot::Mutex;
use serde_json::json;
use tokio::sync::oneshot;

/// A provider that does not answer until it is let go.
struct Blocking {
    gate: Mutex<Option<oneshot::Receiver<()>>>,
    calls: Mutex<usize>,
}

impl Blocking {
    fn new(gate: oneshot::Receiver<()>) -> Self {
        Self {
            gate: Mutex::new(Some(gate)),
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl Provider for Blocking {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("blocking", "blocking")
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        _deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        *self.calls.lock() += 1;
        let gate = self.gate.lock().take();
        if let Some(gate) = gate {
            let _ = gate.await;
        }

        Ok(ModelResponse::text("finally"))
    }
}

/// Returns the name of each state the kernel moved into.
fn transitions(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::StateChanged { to, .. } => Some(to.name()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_fresh_kernel_is_idle() {
    let kernel = Kernel::new(Config::default());
    assert_eq!(kernel.state(), State::Idle);
    assert!(!kernel.state().is_busy());

    // and it refuses to invent a provider
    kernel.push(ContextItem::user("hi"));
    assert!(matches!(kernel.step().await, Err(Error::NoProvider)));
    assert_eq!(kernel.state(), State::Idle, "nothing was claimed");
}

#[tokio::test]
async fn the_machine_walks_a_predictable_path() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "echo", json!({}))]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("echo", "hi")));
    kernel.push(ContextItem::user("go"));

    let mut events = kernel.subscribe();
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));

    assert_eq!(
        transitions(&drain(&mut events)),
        [
            "requesting", // the request
            "ready",      // the model asked for a tool, and the policy allowed it
            "executing",  // the next step runs it
            "idle",       // its result is recorded
            "requesting", // and back to the model
            "finished",
        ]
    );
}

#[tokio::test]
async fn ready_is_a_checkpoint_before_anything_runs() {
    let (kernel, provider) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "shell", json!({ "cmd": "rm -rf /" }))]),
        ModelResponse::text("ok"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("shell", "it ran!")));
    kernel.push(ContextItem::user("clean up"));

    // one step: the model has said what it wants, and nothing has happened
    let state = kernel.step().await.unwrap();
    assert_eq!(
        state,
        State::Ready {
            calls: vec!["c1".into()]
        }
    );
    assert_eq!(provider.requests().len(), 1);

    let pending = kernel.pending_calls();
    assert_eq!(pending.len(), 1);
    assert_eq!(*pending[0].args, json!({ "cmd": "rm -rf /" }));
    assert!(
        !kernel.items().iter().any(|i| i.label == "shell"),
        "no result exists yet, because the tool has not run"
    );

    // the user gets to change their mind
    assert_eq!(kernel.cancel_pending_calls("absolutely not"), 1);
    assert_eq!(kernel.state(), State::Idle);
    assert!(
        kernel
            .items()
            .iter()
            .any(|i| i.content.to_text().contains("absolutely not")),
        "and the model is told why"
    );
}

#[tokio::test]
async fn deciding_reports_what_is_still_outstanding() {
    let (kernel, _) = inquisitive([ModelResponse::tool_calls(vec![
        call("c1", "read", json!({})),
        call("c2", "shell", json!({})),
    ])]);
    kernel.add_tool(Arc::new(ConstTool::new("read", "contents")));
    kernel.add_tool(Arc::new(ConstTool::new("shell", "output")));
    kernel.push(ContextItem::user("go"));

    let State::Deciding { calls } = kernel.step().await.unwrap() else {
        panic!("the default policy asks about everything")
    };
    assert_eq!(calls.len(), 2);
    assert_eq!(kernel.pending_permissions().len(), 2);

    // stepping while it is your move changes nothing at all
    assert_eq!(kernel.step().await.unwrap(), State::Deciding { calls });
    assert_eq!(
        kernel.items().len(),
        2,
        "just the user's message and the turn"
    );

    let requests = kernel.pending_permissions();
    let State::Deciding { calls } = kernel.decide(requests[0].id, Grant::Allow).unwrap() else {
        panic!("one question is still open")
    };
    assert_eq!(calls.len(), 1);

    let state = kernel.decide(requests[1].id, Grant::Deny).unwrap();
    assert!(matches!(state, State::Ready { .. }), "{state:?}");
    assert!(kernel.decide(requests[1].id, Grant::Allow).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_step_is_refused_while_one_is_in_flight() {
    let (open, gate) = oneshot::channel();
    let provider = Arc::new(Blocking::new(gate));

    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider.clone());
    kernel.push(ContextItem::user("hi"));

    let stepping = kernel.clone();
    let first = tokio::spawn(async move { stepping.step().await });

    // wait for the request to actually be in flight
    for _ in 0..1_000 {
        if kernel.state().is_busy() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(kernel.state(), State::Requesting);

    assert!(
        matches!(kernel.step().await, Err(Error::Busy)),
        "a kernel driven twice would pay twice and record twice"
    );

    open.send(()).unwrap();
    assert!(matches!(
        first.await.unwrap().unwrap(),
        State::Finished { .. }
    ));

    assert_eq!(*provider.calls.lock(), 1);
    assert_eq!(
        kernel.items().len(),
        2,
        "one user message, one turn: {:?}",
        kernel.items()
    );
}

#[tokio::test]
async fn a_dropped_step_does_not_wedge_the_kernel() {
    let (_open, gate) = oneshot::channel();
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(Blocking::new(gate)));
    kernel.push(ContextItem::user("hi"));

    let mut events = kernel.subscribe();

    // the client goes away in the middle of the request
    tokio::select! {
        biased;
        _ = kernel.step() => panic!("the provider never answers"),
        _ = std::future::ready(()) => {}
    }

    assert_eq!(kernel.state(), State::Idle);
    assert_eq!(transitions(&drain(&mut events)), ["requesting", "idle"]);
    assert_eq!(kernel.items().len(), 1, "nothing was recorded");

    // and it still works
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse::text("hi")])));
    assert!(matches!(
        kernel.step().await.unwrap(),
        State::Finished { .. }
    ));
}

#[tokio::test]
async fn a_failed_request_leaves_the_context_alone() {
    let kernel = Kernel::new(Config::default());
    // an empty script: the provider will fail
    kernel.set_provider(Arc::new(ScriptedProvider::new([])));
    kernel.push(ContextItem::user("hi"));

    let mut events = kernel.subscribe();
    assert!(matches!(kernel.step().await, Err(Error::Provider(_))));

    assert_eq!(kernel.state(), State::Idle, "so stepping again retries");
    assert_eq!(kernel.items().len(), 1);
    let events = drain(&mut events);
    assert_eq!(common::count(&events, "model.failed"), 1);
    assert_eq!(transitions(&events), ["requesting", "idle"]);
}

#[tokio::test]
async fn the_request_budget_ends_a_turn_without_losing_the_thread() {
    let kernel = Kernel::new(Config {
        max_requests_per_turn: Some(1),
        ..Default::default()
    });
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "echo", json!({}))]),
        ModelResponse::tool_calls(vec![call("c2", "echo", json!({}))]),
        ModelResponse::text("done"),
    ])));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.add_tool(Arc::new(ConstTool::new("echo", "hi")));
    kernel.push(ContextItem::user("go"));

    assert_eq!(
        kernel.turn().await.unwrap(),
        State::Idle,
        "one request spent"
    );
    assert_eq!(kernel.turn().await.unwrap(), State::Idle);
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));
}

#[tokio::test]
async fn the_context_can_be_rewritten_between_two_steps() {
    let (kernel, provider) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "chatty", json!({}))]),
        ModelResponse::text("fine, moving on"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("chatty", "x".repeat(20_000))));
    let question = kernel.push(ContextItem::user("investigate"));

    assert!(matches!(kernel.step().await.unwrap(), State::Ready { .. }));
    assert!(matches!(kernel.step().await.unwrap(), State::Idle));

    // the tool result is enormous and useless, so out it goes
    let noisy = kernel
        .items()
        .iter()
        .find(|i| i.label == "chatty")
        .map(|i| i.id)
        .unwrap();
    kernel.set_state([noisy], ContextState::Excluded, Some("garbage".into()));
    kernel
        .replace(question, "never mind, investigate later")
        .unwrap();

    assert!(matches!(
        kernel.step().await.unwrap(),
        State::Finished { .. }
    ));
    let last = provider.requests().pop().unwrap();
    assert_eq!(
        last.messages.len(),
        1,
        "the orphaned call went with it: {:?}",
        last.messages
    );
    assert_eq!(
        last.messages[0].content.as_ref().unwrap().to_text(),
        "never mind, investigate later"
    );
}

#[test]
fn the_loop_is_send() {
    fn assert_send<T: Send>(_: T) {}

    let kernel = Kernel::new(Config::default());
    assert_send(kernel.step());
    assert_send(kernel.turn());
    assert_send(kernel.clone());
}

/// A policy that asks about everything, and takes its time over the second call.
struct SlowAsk;

#[async_trait]
impl nachalnik::PermissionPolicy for SlowAsk {
    async fn evaluate(&self, request: &nachalnik::PermissionRequest) -> nachalnik::Verdict {
        if request.call.0 == "b" {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        nachalnik::Verdict::Ask
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_question_can_be_answered_the_moment_it_is_asked() {
    // the whole client story is subscribe-and-answer, so a `permission.requested` that names a
    // request `decide` does not know about yet would make it a lie - and it used to, for as long
    // as the policy was still being consulted about the rest of the batch
    let (kernel, _) = inquisitive([ModelResponse::tool_calls(vec![
        call("a", "echo", json!({ "value": "one" })),
        call("b", "echo", json!({ "value": "two" })),
    ])]);
    kernel.set_policy(Arc::new(SlowAsk));
    kernel.add_tool(Arc::new(ConstTool::new("echo", "ok")));
    kernel.push(ContextItem::user("go"));

    let mut events = kernel.subscribe();
    let answering = kernel.clone();
    let answered = tokio::spawn(async move {
        let mut outcomes = Vec::new();
        while let Ok(event) = events.recv().await {
            if let Event::PermissionRequested { request } = event {
                outcomes.push(answering.decide(request.id, Grant::Allow));
                if outcomes.len() == 2 {
                    break;
                }
            }
        }

        outcomes
    });

    kernel.step().await.unwrap();
    let outcomes = answered.await.unwrap();

    assert_eq!(outcomes.len(), 2);
    for outcome in &outcomes {
        assert!(
            outcome.is_ok(),
            "a client answering the question it was just handed was refused: {outcome:?}"
        );
    }
    // and answering both of them is what makes the calls runnable
    assert!(matches!(kernel.state(), State::Ready { .. }));
}

/// A tool that hits the interrupt button halfway through its own work.
struct Impatient(std::sync::OnceLock<Kernel>);

#[async_trait]
impl nachalnik::Tool for Impatient {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("slow", "takes a while, and gets interrupted")
    }

    async fn invoke(
        &self,
        _call: &nachalnik::ToolCall,
        _output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, BoxError> {
        self.0.get().expect("the kernel was set").interrupt();

        Ok(nachalnik::ToolOutput::new("the output"))
    }
}

#[tokio::test]
async fn an_interrupt_stops_the_loop_without_losing_the_step_in_flight() {
    let (kernel, provider) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "slow", json!({}))]),
        ModelResponse::text("never asked"),
    ]);
    let tool = Arc::new(Impatient(std::sync::OnceLock::new()));
    assert!(tool.0.set(kernel.clone()).is_ok(), "set once");
    kernel.add_tool(tool);
    kernel.push(ContextItem::user("go"));

    let mut events = kernel.subscribe();

    // the button is hit while the tool is running: that tool finishes and is recorded, and the
    // loop stops instead of going back to the model
    assert_eq!(kernel.turn().await.unwrap(), State::Idle);
    assert_eq!(provider.requests().len(), 1, "no second request was sent");
    assert!(
        !kernel.is_interrupted(),
        "the flag is cleared by the turn that acts on it"
    );

    let names: Vec<_> = drain(&mut events)
        .iter()
        .map(|e| e.name().to_owned())
        .collect();
    assert!(names.contains(&"turn.interrupted".to_owned()));
    assert!(
        names.contains(&"tool.finished".to_owned()),
        "the step already under way still recorded its result: {names:?}"
    );

    // nothing was thrown away, so the next turn simply carries on
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));
    assert_eq!(provider.requests().len(), 2);

    // the tool holds the kernel that holds the tool; this is the documented way out
    kernel.remove_tool("slow");
}

#[tokio::test]
async fn an_interrupt_starts_nothing_new() {
    let (kernel, provider) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "shell", json!({ "cmd": "rm -rf /" }))]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("shell", "it ran!")));
    kernel.push(ContextItem::user("clean up"));

    assert!(matches!(kernel.step().await.unwrap(), State::Ready { .. }));
    assert!(!kernel.interrupt(), "the first ask is the one that lands");
    assert!(kernel.interrupt(), "and the second changes nothing");

    // a turn asked to stop does not run the tool it was about to run, and says where it stopped
    assert!(matches!(kernel.turn().await.unwrap(), State::Ready { .. }));
    assert_eq!(provider.requests().len(), 1);
    assert!(
        !kernel.items().iter().any(|i| i.label == "shell"),
        "the pending call is still pending, not run"
    );

    // which leaves the user exactly where `Ready` is for
    assert_eq!(kernel.cancel_pending_calls("changed my mind"), 1);
}

#[tokio::test]
async fn an_interrupt_does_not_abort_a_request_already_in_flight() {
    let (open, gate) = oneshot::channel();
    let provider = Arc::new(Blocking::new(gate));
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider.clone());
    kernel.push(ContextItem::user("hi"));

    let stepping = kernel.clone();
    let stepped = tokio::spawn(async move { stepping.step().await });

    for _ in 0..1_000 {
        if kernel.state().is_busy() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    kernel.interrupt();

    // the answer still arrives and is still recorded: stopping is not the same as pulling the
    // plug, and the way to abandon a request is to drop the future driving it
    open.send(()).unwrap();
    assert!(matches!(
        stepped.await.unwrap().unwrap(),
        State::Finished { .. }
    ));
    assert_eq!(kernel.items().len(), 2);
}

/// A streaming provider that stops as soon as it is told to, and hands back what it had.
///
/// note: This is the whole contract for in-flight cancellation - the kernel offers the fact
/// through the sink the provider already holds, and the provider decides what to do about it.
struct Watchful {
    /// Opened once the test has had a chance to press the button.
    gate: Mutex<Option<oneshot::Receiver<()>>>,
    /// Signals that the request is genuinely under way.
    started: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait]
impl Provider for Watchful {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("watchful", "watchful")
    }

    async fn respond(
        &self,
        _request: ModelRequest,
        deltas: DeltaSink,
    ) -> Result<ModelResponse, BoxError> {
        let mut text = String::new();

        for word in ["one", "two", "three", "four", "five"] {
            if deltas.is_interrupted() {
                return Ok(ModelResponse {
                    stop: nachalnik::StopReason::Other("interrupted".to_owned()),
                    ..ModelResponse::text(text)
                });
            }
            deltas.text(word);
            text.push_str(word);

            // after the first fragment, let the test interrupt and wait until it has
            let started = self.started.lock().take();
            if let Some(started) = started {
                let _ = started.send(());
            }
            let gate = self.gate.lock().take();
            if let Some(gate) = gate {
                let _ = gate.await;
            }
        }

        Ok(ModelResponse::text(text))
    }
}

#[tokio::test]
async fn a_provider_that_watches_stops_a_request_in_flight() {
    let (open, gate) = oneshot::channel();
    let (report, started) = oneshot::channel();
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(Watchful {
        gate: Mutex::new(Some(gate)),
        started: Mutex::new(Some(report)),
    }));
    kernel.push(ContextItem::user("count to five"));

    let stepping = kernel.clone();
    let stepped = tokio::spawn(async move { stepping.step().await });

    started.await.expect("the request got going");
    kernel.interrupt();
    open.send(()).expect("the provider is waiting");

    let state = stepped.await.unwrap().unwrap();

    // it stopped, and what it had already said is in the context rather than thrown away
    assert!(matches!(state, State::Finished { .. }), "{state:?}");
    let answer = kernel.last_response().unwrap();
    assert_eq!(answer.content.clone().unwrap().to_text(), "one");
    assert_eq!(
        answer.stop,
        nachalnik::StopReason::Other("interrupted".to_owned())
    );
    assert!(
        kernel.items().iter().any(|i| i.content.to_text() == "one"),
        "the partial turn is an ordinary item, to keep or to prune"
    );
}

/// A tool that notices it is not wanted and returns what it has.
struct Diligent;

#[async_trait]
impl nachalnik::Tool for Diligent {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("count", "counts, unless told not to")
    }

    async fn invoke(
        &self,
        _call: &nachalnik::ToolCall,
        output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, BoxError> {
        let mut done = String::new();
        for n in 0..10 {
            if output.is_interrupted() {
                return Ok(nachalnik::ToolOutput::new(format!("{done}[stopped]")));
            }
            done.push_str(&n.to_string());
        }

        Ok(nachalnik::ToolOutput::new(done))
    }
}

/// A tool that presses the button, so that the calls after it can be seen not to run.
struct Stopper(std::sync::OnceLock<Kernel>);

#[async_trait]
impl nachalnik::Tool for Stopper {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("stop", "asks the loop to stop, from inside it")
    }

    async fn invoke(
        &self,
        _call: &nachalnik::ToolCall,
        _output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, BoxError> {
        self.0.get().expect("the kernel was set").interrupt();

        Ok(nachalnik::ToolOutput::new("asked"))
    }
}

#[tokio::test]
async fn a_tool_that_watches_stops_partway_and_still_answers_its_call() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![
            call("c1", "stop", json!({})),
            call("c2", "count", json!({})),
        ]),
        ModelResponse::text("never asked"),
    ]);
    let stopper = Arc::new(Stopper(std::sync::OnceLock::new()));
    assert!(stopper.0.set(kernel.clone()).is_ok(), "set once");
    kernel.add_tool(stopper);
    kernel.add_tool(Arc::new(Diligent));
    kernel.push(ContextItem::user("go"));

    // the first call presses the button; the second one starts anyway, because the kernel only
    // skips the calls it has not begun - and this one notices for itself
    assert!(matches!(kernel.step().await.unwrap(), State::Ready { .. }));
    assert_eq!(kernel.step().await.unwrap(), State::Idle);

    let results: Vec<String> = kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(
        results.len(),
        2,
        "every call is answered, however it ended: {results:?}"
    );

    kernel.remove_tool("stop");
}

#[tokio::test]
async fn an_interrupt_does_not_start_the_calls_that_had_not_begun() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![
            call("c1", "stop", json!({})),
            call("c2", "shell", json!({})),
            call("c3", "shell", json!({})),
        ]),
        ModelResponse::text("never asked"),
    ]);
    let stopper = Arc::new(Stopper(std::sync::OnceLock::new()));
    assert!(stopper.0.set(kernel.clone()).is_ok(), "set once");
    kernel.add_tool(stopper);
    kernel.add_tool(Arc::new(ConstTool::new("shell", "it ran!")));
    kernel.push(ContextItem::user("go"));

    assert!(matches!(kernel.step().await.unwrap(), State::Ready { .. }));
    assert_eq!(kernel.step().await.unwrap(), State::Idle);

    let results: Vec<String> = kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect();

    assert_eq!(
        results.len(),
        3,
        "a call with no result at all answers nobody"
    );
    assert_eq!(results[0], "asked");
    assert!(
        results[1..].iter().all(|r| r.contains("interrupted")),
        "the calls that had not begun did not run: {results:?}"
    );
    assert!(
        !results.iter().any(|r| r == "it ran!"),
        "and really did not run: {results:?}"
    );

    kernel.remove_tool("stop");
}

#[tokio::test]
async fn a_step_spends_one_attempt_acknowledging_an_interrupt() {
    let (kernel, provider) =
        permissive([ModelResponse::text("first"), ModelResponse::text("second")]);
    kernel.push(ContextItem::user("go"));

    kernel.interrupt();
    assert_eq!(
        kernel.step().await.unwrap(),
        State::Idle,
        "the step that acknowledges it does nothing else"
    );
    assert!(provider.requests().is_empty(), "and sends nothing");
    assert!(
        !kernel.is_interrupted(),
        "an interrupt cannot outlive the transition that acted on it"
    );

    // which is what stops it from cancelling something nobody meant it to
    assert!(matches!(
        kernel.step().await.unwrap(),
        State::Finished { .. }
    ));
    assert_eq!(provider.requests().len(), 1);
}
