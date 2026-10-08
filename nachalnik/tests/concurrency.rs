//! Tests for what happens when more than one thing is going on at once.
//!
//! note: There are three different stories here and they are worth keeping apart. A *fleet* is
//! many kernels, one per agent, which share nothing and need nothing; a *client* is one kernel
//! that several threads read and edit while a turn runs; and a *turn* is the one place the kernel
//! itself does several things, which is why compaction is one locked operation rather than a
//! sequence of them.

mod common;

use std::sync::Arc;

use common::tool_results_text;
use nachalnik::{
    CompactionPlan, Config, ContextItem, ContextState, Event, Kernel, ModelResponse, State,
    test::{AllowAll, ConstTool, ScriptedProvider, call},
};
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_fleet_of_agents_shares_nothing_it_was_not_given() {
    // one tool object serving all of them, as a real fleet would
    let tool = Arc::new(ConstTool::new("peek", "ok"));

    let mut running = Vec::new();
    for n in 0..16 {
        let tool = tool.clone();
        running.push(tokio::spawn(async move {
            let kernel = Kernel::new(Config::default());
            kernel.set_provider(Arc::new(ScriptedProvider::new([
                ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
                ModelResponse::text(format!("agent {n} done")),
            ])));
            kernel.set_policy(Arc::new(AllowAll));
            kernel.add_tool(tool);
            kernel.push(ContextItem::user(format!("agent {n}")));

            let end = kernel.turn().await.unwrap();
            assert!(matches!(end, State::Finished { .. }), "{end:?}");

            let answer = kernel
                .last_response()
                .unwrap()
                .content
                .clone()
                .unwrap()
                .to_text()
                .into_owned();

            (n, kernel.items().len(), kernel.session_name(), answer)
        }));
    }

    let mut names = Vec::new();
    for handle in running {
        let (n, items, session, answer) = handle.await.unwrap();
        assert_eq!(items, 4, "agent {n} ended up with somebody else's context");
        assert_eq!(answer, format!("agent {n} done"));
        names.push(session);
    }

    names.sort();
    names.dedup();
    assert_eq!(names.len(), 16, "and each one is its own session");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_context_survives_being_edited_from_several_places() {
    let kernel = Kernel::new(Config::default());
    let ids: Vec<_> = (0..200)
        .map(|i| kernel.push(ContextItem::file(format!("src/f{i}.rs"), "x".repeat(64))))
        .collect();

    let mut hands = Vec::new();
    for chunk in ids.chunks(20) {
        let kernel = kernel.clone();
        let chunk = chunk.to_vec();
        hands.push(tokio::spawn(async move {
            for _ in 0..50 {
                kernel.set_state(chunk.clone(), ContextState::Excluded, Some("noise".into()));
                let _ = kernel.budget();
                let _ = kernel.project();
                let _ = kernel.preview_request();
                kernel.set_state(chunk.clone(), ContextState::Active, None);
            }
        }));
    }
    for hand in hands {
        hand.await.unwrap();
    }

    assert_eq!(kernel.items().len(), 200, "nothing was lost or duplicated");
    assert!(
        kernel.items().iter().all(|item| item.is_projected()),
        "every one of them ended where its own thread left it"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_log_tells_the_states_in_the_order_they_happened() {
    // one item, four threads flipping it, so that every operation contends with the others. The
    // states the log reports have to chain - each `from` being the previous `to` - because a log
    // whose entries were announced after the lock was dropped can be applied in one order and
    // recorded in the other, and its last word on an item then contradicts the item
    for _ in 0..50 {
        let kernel = Kernel::new(Config::default());
        let item = kernel.push(ContextItem::file("src/a.rs", "x"));

        let mut hands = Vec::new();
        for _ in 0..4 {
            let kernel = kernel.clone();
            hands.push(tokio::spawn(async move {
                for _ in 0..25 {
                    kernel.set_state([item], ContextState::Excluded, None);
                    kernel.set_state([item], ContextState::Active, None);
                }
            }));
        }
        for hand in hands {
            hand.await.unwrap();
        }

        let mut walked = ContextState::Active;
        let mut seen = 0;
        for record in kernel.history() {
            let Event::ContextChanged { id, from, to, .. } = record.event else {
                continue;
            };
            assert_eq!(id, item);
            assert_eq!(
                from, walked,
                "the log skipped a state, or reported two out of order"
            );
            walked = to;
            seen += 1;
        }

        assert!(seen > 0, "something has to have been recorded");
        assert_eq!(
            walked,
            kernel.item(item).unwrap().state,
            "and the last thing the log says about the item is what the item says"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_compaction_does_not_swallow_what_arrives_during_it() {
    for _ in 0..200 {
        let kernel = Kernel::new(Config::default());
        let doomed: Vec<_> = (0..40)
            .map(|i| kernel.push(ContextItem::file(format!("src/f{i}.rs"), "x".repeat(256))))
            .collect();

        let pushing = kernel.clone();
        let pusher = tokio::spawn(async move { pushing.push(ContextItem::user("me too")) });

        let report = kernel.apply_compaction(CompactionPlan {
            remove: doomed.clone(),
            elide: Vec::new(),
            summary: None,
            reason: "an overzealous compactor".into(),
        });
        let late = pusher.await.unwrap();

        // the compaction is one locked operation, so it either sees the new item or it does not,
        // and either way it removes exactly what it was asked to and leaves the rest alone
        assert_eq!(report.removed.len(), 40);
        assert!(kernel.item(late).is_some(), "the push landed and stayed");
        assert!(kernel.item(late).unwrap().is_projected());
        assert!(
            doomed
                .iter()
                .all(|id| !kernel.item(*id).unwrap().is_projected())
        );
    }
}

/// A tool that takes a while, and says when it ran.
struct Slow(std::time::Duration);

#[nachalnik::async_trait]
impl nachalnik::Tool for Slow {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("slow", "takes its time")
    }

    async fn invoke(
        &self,
        call: &nachalnik::ToolCall,
        _output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
        tokio::time::sleep(self.0).await;

        Ok(nachalnik::ToolOutput::new(format!("{} ran", call.id)))
    }
}

/// A tool that says it has started and then waits to be let go, and says when it ran.
///
/// note: a seam rather than a sleep, for the reason [`Held`] gives. An interrupt timed by a sleep
/// that overshot the calls lands after they have all finished, and a test about what an interrupt
/// does to running calls passes without one having run.
struct Gated {
    started: tokio::sync::mpsc::UnboundedSender<()>,
    gate: Arc<tokio::sync::Semaphore>,
}

#[nachalnik::async_trait]
impl nachalnik::Tool for Gated {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("slow", "takes its time")
    }

    async fn invoke(
        &self,
        call: &nachalnik::ToolCall,
        _output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
        let _ = self.started.send(());
        self.gate.acquire().await?.forget();

        Ok(nachalnik::ToolOutput::new(format!("{} ran", call.id)))
    }
}

/// [`three_slow_calls`] with [`Gated`] calls: the kernel, a signal per call that starts, and the
/// gate that lets them finish.
fn three_gated_calls(
    parallel: bool,
) -> (
    Kernel,
    tokio::sync::mpsc::UnboundedReceiver<()>,
    Arc<tokio::sync::Semaphore>,
) {
    let (started, starts) = tokio::sync::mpsc::unbounded_channel();
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let tool = Gated {
        started,
        gate: gate.clone(),
    };

    (three_calls(parallel, Arc::new(tool)), starts, gate)
}

fn three_slow_calls(parallel: bool) -> Kernel {
    three_calls(
        parallel,
        Arc::new(Slow(std::time::Duration::from_millis(150))),
    )
}

/// Three calls to `slow`, whichever tool is answering to the name.
fn three_calls(parallel: bool, tool: Arc<dyn nachalnik::Tool>) -> Kernel {
    let kernel = Kernel::new(Config {
        parallel_tool_calls: parallel,
        ..Default::default()
    });
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![
            call("c1", "slow", json!({})),
            call("c2", "slow", json!({})),
            call("c3", "slow", json!({})),
        ]),
        ModelResponse::text("done"),
    ])));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.add_tool(tool);
    kernel.push(ContextItem::user("go"));

    kernel
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_tools_at_once_take_as_long_as_one() {
    let started = std::time::Instant::now();
    let parallel = three_slow_calls(true);
    parallel.turn().await.unwrap();
    let together = started.elapsed();

    let started = std::time::Instant::now();
    let serial = three_slow_calls(false);
    serial.turn().await.unwrap();
    let in_turn = started.elapsed();

    assert!(
        together < in_turn / 2,
        "three 150ms calls took {together:?} together and {in_turn:?} in turn"
    );

    // and the context is the same either way, because what varies is when they ran, not what
    // the model is told afterwards
    let shape = |kernel: &Kernel| {
        kernel
            .items()
            .iter()
            .map(|item| (item.label.clone(), item.content.to_text().into_owned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(shape(&parallel), shape(&serial));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_still_lands_in_its_own_place() {
    let kernel = Kernel::new(Config {
        parallel_tool_calls: true,
        ..Default::default()
    });
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![
            call("c1", "quick", json!({})),
            call("c2", "slow", json!({})),
            call("c3", "refused", json!({})),
        ]),
        ModelResponse::text("done"),
    ])));
    kernel.set_policy(Arc::new(
        nachalnik::test::Table::new(nachalnik::Verdict::Allow).rule(
            nachalnik::Capability::net("reach"),
            nachalnik::Verdict::Deny,
        ),
    ));
    kernel.add_tool(Arc::new(Slow(std::time::Duration::from_millis(100))));
    kernel.add_tool(Arc::new(ConstTool::new("quick", "instant")));
    kernel.add_tool(Arc::new(
        ConstTool::new("refused", "ran anyway")
            .with_capabilities([nachalnik::Capability::net("reach")]),
    ));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.unwrap();

    // the slow one finished last and is still recorded second, and the refusal, which had
    // nothing to run, is still recorded third
    let answered: Vec<_> = kernel
        .items()
        .iter()
        .filter_map(|item| match &item.kind {
            nachalnik::ContextKind::ToolResult { call, is_error, .. } => Some((
                call.0.clone(),
                *is_error,
                item.content.to_text().into_owned(),
            )),
            _ => None,
        })
        .collect();
    let ids: Vec<_> = answered.iter().map(|(id, ..)| id.as_str()).collect();
    assert_eq!(ids, ["c1", "c2", "c3"]);
    let (_, is_error, text) = &answered[2];
    assert!(
        *is_error && text.starts_with("the call was not permitted"),
        "{answered:?}"
    );
}

#[tokio::test]
async fn one_at_a_time_means_one_finishes_before_the_next_starts() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![
            call("c1", "quick", json!({})),
            call("c2", "quick", json!({})),
        ]),
        ModelResponse::text("done"),
    ])));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.add_tool(Arc::new(ConstTool::new("quick", "instant")));
    kernel.push(ContextItem::user("go"));

    let mut events = kernel.subscribe();
    kernel.turn().await.unwrap();

    // a client watching this should see a call finish, not a batch of them
    let order: Vec<_> = std::iter::from_fn(|| events.try_recv().ok())
        .map(|event| event.name().to_owned())
        .filter(|name| name.starts_with("tool."))
        .collect();
    assert_eq!(
        order,
        [
            "tool.requested",
            "tool.requested",
            "tool.started",
            "tool.output",
            "tool.finished",
            "tool.started",
            "tool.output",
            "tool.finished",
        ]
    );
}

// ------------------------------------------------------- what an interrupt can still stop, and not

/// note: `Slow` never looks at [`OutputSink::is_interrupted`], and that is the point of using it
/// here. What is being pinned is what the *kernel* can do about a tool that does not cooperate,
/// which is the guarantee a caller has to reason about when the tool is somebody else's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_in_turn_an_interrupt_stops_the_calls_that_had_not_started() {
    let (kernel, mut starts, gate) = three_gated_calls(false);

    let interrupting = {
        let kernel = kernel.clone();
        tokio::spawn(async move {
            // inside the first call, and nowhere near the second
            starts.recv().await.unwrap();
            kernel.interrupt();
            gate.add_permits(3);
        })
    };
    kernel.turn().await.unwrap();
    interrupting.await.unwrap();

    let results = tool_results_text(&kernel);
    assert_eq!(
        results.len(),
        3,
        "every call is recorded either way: {results:?}"
    );
    assert!(
        results[0].contains("c1 ran"),
        "the one already running finishes: {results:?}"
    );
    assert!(
        results[1].contains("interrupted") && results[2].contains("interrupted"),
        "the queue is emptied: {results:?}"
    );
}

/// An interrupt that lands between two calls is recorded before every call it stopped and after
/// every call that ran, so no call starts after `turn.interrupted` in the record.
///
/// note: a race rather than a seam, because the window is inside the kernel - between reading the
/// flag and announcing the next start - and nothing a test supplies is called there. So the
/// interrupt is fired from a thread of its own the moment the stream shows a call starting, across
/// many batches of calls that answer at once, which puts it everywhere in the loop sooner or later.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_in_turn_nothing_starts_after_the_interrupt_that_stopped_it() {
    const CALLS: usize = 32;

    for round in 0..500 {
        let kernel = Kernel::new(Config::default());
        let calls = (0..CALLS)
            .map(|n| call(&format!("c{n}"), "quick", json!({})))
            .collect();
        kernel.set_provider(Arc::new(ScriptedProvider::new([
            ModelResponse::tool_calls(calls),
            ModelResponse::text("done"),
        ])));
        kernel.set_policy(Arc::new(AllowAll));
        kernel.add_tool(Arc::new(ConstTool::new("quick", "ok")));
        kernel.push(ContextItem::user("go"));

        // after a different call each round, so the interrupt meets the loop at a different place
        let after = 1 + round % (CALLS - 2);
        let mut events = kernel.subscribe();
        let interrupting = {
            let kernel = kernel.clone();
            std::thread::spawn(move || {
                let mut seen = 0;
                while let Ok(event) = events.blocking_recv() {
                    if matches!(event, Event::ToolStarted { .. }) {
                        seen += 1;
                        if seen == after {
                            kernel.interrupt();
                            return;
                        }
                    }
                }
            })
        };

        // a step at a time, because `turn` would spend the interrupt before the calls run
        while !matches!(kernel.step().await.unwrap(), State::Idle) {}
        interrupting.join().unwrap();

        let records = kernel.history();
        let Some(interrupted) = records
            .iter()
            .position(|record| matches!(record.event, Event::Interrupted))
        else {
            continue;
        };
        let late = records[interrupted..]
            .iter()
            .filter(|record| matches!(record.event, Event::ToolStarted { .. }))
            .count();
        assert_eq!(
            late, 0,
            "round {round}: {late} calls started after the interrupt that stopped the rest"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_together_there_is_no_queue_left_for_an_interrupt_to_empty() {
    let (kernel, mut starts, gate) = three_gated_calls(true);

    let interrupting = {
        let kernel = kernel.clone();
        tokio::spawn(async move {
            // all three running, none finished
            for _ in 0..3 {
                starts.recv().await.unwrap();
            }
            kernel.interrupt();
            gate.add_permits(3);
        })
    };
    kernel.turn().await.unwrap();
    interrupting.await.unwrap();

    // all three were spawned before the first one answered, so the interrupt arrives with nothing
    // left to not-start. This is the guarantee `parallel_tool_calls` trades away, and it is worth
    // a test rather than a sentence because it is the one somebody will assume they still have
    let results = tool_results_text(&kernel);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(
        results.iter().all(|said| said.contains("ran")),
        "every call had already started: {results:?}"
    );
}

/// A tool that panics has failed, and is answered like any failing tool, however the calls run.
///
/// note: the call is answered with an error the model is shown, `tool.panicked` says why, and
/// the calls beside it and the rest of the turn go on. Unwound instead, the turn ended with no
/// answer to the call and no record of what happened to it, and a client awaiting the step on a
/// task of its own never heard back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_panicking_tool_is_a_failing_tool_run_in_turn_or_together() {
    for parallel in [false, true] {
        let kernel = three_calls(parallel, Arc::new(Panics));

        let end = kernel.turn().await.expect("the turn survives the tool");
        assert!(matches!(end, State::Finished { .. }), "{end:?}");

        let results = tool_results_text(&kernel);
        assert_eq!(results.len(), 3, "every call is answered: {results:?}");
        assert!(
            results[0].contains("ran") && results[2].contains("ran"),
            "the calls beside it go on: {results:?}"
        );
        assert!(
            results[1].contains("crashed") && results[1].contains("c2 panicked"),
            "the model is told, with what the panic said: {results:?}"
        );

        let about_c2: Vec<&str> = kernel
            .history()
            .iter()
            .filter_map(|record| match &record.event {
                Event::ToolStarted { call, .. }
                | Event::ToolPanicked { call, .. }
                | Event::ToolFinished { call, .. }
                    if call.0 == "c2" =>
                {
                    Some(record.event.name())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            about_c2,
            ["tool.started", "tool.panicked", "tool.finished"],
            "parallel: {parallel}"
        );
    }
}

/// A tool that panics on the second call and answers the others.
struct Panics;

#[nachalnik::async_trait]
impl nachalnik::Tool for Panics {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("slow", "panics on c2")
    }

    async fn invoke(
        &self,
        call: &nachalnik::ToolCall,
        _output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
        match call.id.0.as_str() {
            "c2" => panic!("c2 panicked"),
            id => Ok(nachalnik::ToolOutput::new(format!("{id} ran"))),
        }
    }
}

/// Run together, a call whose task the runtime cancelled is recorded as not having run.
///
/// note: the calls are spawned onto whichever runtime the step is polled in, and one that has
/// shut down cancels what it is given. That is neither the tool failing nor the tool panicking,
/// so it is not a reason to unwind the step: the model is told the call did not finish.
#[test]
fn run_together_a_call_its_runtime_cancelled_is_recorded_as_not_having_run() {
    let gone = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let shut_down = gone.handle().clone();
    drop(gone);

    let kernel = three_calls(true, Arc::new(ConstTool::new("slow", "ran")));
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(async {
            let _spawning_there = shut_down.enter();
            kernel.turn().await.unwrap();
        });

    let results = tool_results_text(&kernel);
    assert_eq!(results.len(), 3, "{results:?}");
    assert!(
        results
            .iter()
            .all(|said| said.contains("did not run to completion")),
        "{results:?}"
    );
}

/// A policy that refuses everything and panics while explaining itself.
///
/// note: `why` is asked only for a call this policy refused, and it is asked before the kernel
/// starts polling the tool - so the panic is inside the call's task and outside the
/// `catch_unwind` a tool's own panic is caught at. The tool itself is never reached; what this
/// reaches is the `JoinSet` the kernel is joining, which is where a panic has to be handed on.
struct ExplainsByPanicking;

#[nachalnik::async_trait]
impl nachalnik::PermissionPolicy for ExplainsByPanicking {
    async fn evaluate(&self, _request: &nachalnik::PermissionRequest) -> nachalnik::Verdict {
        nachalnik::Verdict::Deny
    }

    fn why(&self, _request: &nachalnik::PermissionRequest) -> Option<String> {
        panic!("this policy cannot say why")
    }
}

/// A panic inside a call unwinds the step, run together or one at a time.
///
/// note: the kernel's own panics cannot be reached from outside - a claimed call has always had
/// its grant - so a seam that reaches them is what makes this worth a test. A policy asked to
/// explain a refusal answers with a panic instead of a sentence: run one at a time that panic is
/// raised on the caller's own task, and run together it is raised inside a spawned task. The
/// step has to hand the second on as the first, because the alternative is worse than losing the
/// panic - the call is answered with `the call did not run to completion`, which says the call was
/// cut short rather than that something inside it panicked, and a client awaiting the step on a
/// task of its own is handed a state in place of the panic that ended it.
#[test]
fn a_panic_in_a_call_unwinds_the_step_however_the_calls_run() {
    for parallel in [false, true] {
        let kernel = Kernel::new(Config {
            parallel_tool_calls: parallel,
            ..Default::default()
        });
        kernel.set_provider(Arc::new(ScriptedProvider::new([
            ModelResponse::tool_calls(vec![call("c1", "slow", json!({}))]),
            ModelResponse::text("done"),
        ])));
        kernel.set_policy(Arc::new(ExplainsByPanicking));
        kernel.add_tool(Arc::new(ConstTool::new("slow", "ran")));
        kernel.push(ContextItem::user("go"));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(kernel.turn())
        }));

        match unwound {
            Err(payload) => {
                let said = payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or_default();
                assert!(
                    said.contains("cannot say why"),
                    "parallel: {parallel}, the step unwound with something else: {said}"
                );
            }
            Ok(Ok(state)) => panic!(
                "parallel: {parallel}, the step came back {state:?} rather than unwinding, and \
                 the call was answered: {:?}",
                tool_results_text(&kernel)
            ),
            Ok(Err(e)) => panic!(
                "parallel: {parallel}, the step failed with {e} rather than unwinding: {:?}",
                tool_results_text(&kernel)
            ),
        }
    }
}

/// A provider whose `info` waits the first time it is asked, so that a second setter can be let
/// in while the first is still describing what it installs.
///
/// note: the first time, because that is the swap installing it: `set_provider` asks the provider
/// it has just installed what it is, under the lock and before announcing it, and names the one it
/// replaced as the log last did, without asking it.
struct SlowInfo {
    name: &'static str,
    asked: std::sync::atomic::AtomicUsize,
    gate: parking_lot::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// Told each time `info` is asked, with how many times that is.
    told: std::sync::mpsc::Sender<usize>,
}

impl SlowInfo {
    fn new(
        name: &'static str,
        gate: Option<std::sync::mpsc::Receiver<()>>,
        told: std::sync::mpsc::Sender<usize>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            asked: std::sync::atomic::AtomicUsize::new(0),
            gate: parking_lot::Mutex::new(gate),
            told,
        })
    }
}

#[nachalnik::async_trait]
impl nachalnik::Provider for SlowInfo {
    fn info(&self) -> nachalnik::ModelInfo {
        let asked = self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _ = self.told.send(asked + 1);
        if asked == 0
            && let Some(gate) = self.gate.lock().take()
        {
            let _ = gate.recv();
        }

        nachalnik::ModelInfo::new(self.name, self.name)
    }

    async fn respond(
        &self,
        _request: nachalnik::ModelRequest,
        _deltas: nachalnik::DeltaSink,
    ) -> std::result::Result<ModelResponse, nachalnik::BoxError> {
        Ok(ModelResponse::text("never asked"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_swapping_a_component_are_logged_in_the_order_they_applied() {
    let (open, gate) = std::sync::mpsc::channel();
    let (told_second, second_asked) = std::sync::mpsc::channel();
    let kernel = Kernel::new(Config::default());
    let (told, _) = std::sync::mpsc::channel();
    kernel.set_provider(SlowInfo::new("first", None, told));
    let mut events = kernel.subscribe();

    // the swap to `second` waits inside the call describing it, which is made under the lock.
    // Announcing after the lock is let go would let the next swap in here: it would apply second
    // and be logged first, and the log's last word on the provider would name the one that is not
    // installed
    let second = {
        let kernel = kernel.clone();
        std::thread::spawn(move || {
            kernel.set_provider(SlowInfo::new("second", Some(gate), told_second))
        })
    };
    // `second` asked is its swap, held under the lock
    second_asked.recv().unwrap();

    let third = {
        let kernel = kernel.clone();
        let (told, _) = std::sync::mpsc::channel();
        std::thread::spawn(move || kernel.set_provider(SlowInfo::new("third", None, told)))
    };
    // nothing marks a thread as waiting on a lock, so a moment is what puts `third` there
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    open.send(()).unwrap();

    second.join().unwrap();
    third.join().unwrap();

    let named: Vec<(String, String)> = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            Event::ModelChanged { from, to } => Some((
                from.map(|it| it.model).unwrap_or_default(),
                to.map(|it| it.model).unwrap_or_default(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        named,
        [
            ("first".to_owned(), "second".to_owned()),
            ("second".to_owned(), "third".to_owned())
        ],
        "the log follows the swaps, not the other way about"
    );
    assert_eq!(kernel.model_info().unwrap().model, "third");
}

// ------------------------------------------------------------------------ held at one moment

/// A counter that counts the way the default does, except that the first content it is asked
/// about mentioning `word` waits to be let go - holding whatever lock the kernel counts under.
///
/// note: a seam rather than a sleep, so that the moment a test is about is one the kernel is
/// really in: the thread counting is stopped there, and everything the test does next happens
/// while it is.
struct Held {
    word: &'static str,
    reached: parking_lot::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    gate: parking_lot::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

impl Held {
    /// The counter, the signal that it has been reached, and the key that lets it go.
    fn new(
        word: &'static str,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (reached, signal) = std::sync::mpsc::channel();
        let (open, gate) = std::sync::mpsc::channel();

        (
            Arc::new(Self {
                word,
                reached: parking_lot::Mutex::new(Some(reached)),
                gate: parking_lot::Mutex::new(Some(gate)),
            }),
            signal,
            open,
        )
    }
}

impl nachalnik::TokenCounter for Held {
    fn count(&self, content: &nachalnik::Content) -> usize {
        if content.to_text().contains(self.word)
            && let Some(reached) = self.reached.lock().take()
        {
            let _ = reached.send(());
            if let Some(gate) = self.gate.lock().take() {
                let _ = gate.recv();
            }
        }

        nachalnik::BytesPerToken::default().count(content)
    }
}

/// Runs one step on a thread of its own, and hands back where its answer will arrive.
///
/// note: its own thread and runtime, because a step that waits on a lock blocks the thread it is
/// on - and a test that awaited it on its own would wait with it rather than notice it.
fn stepped_elsewhere(
    kernel: &Kernel,
) -> std::sync::mpsc::Receiver<Result<State, nachalnik::Error>> {
    let (said, answer) = std::sync::mpsc::channel();
    let kernel = kernel.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        let _ = said.send(runtime.block_on(kernel.step()));
    });

    answer
}

/// A kernel with two calls waiting on a decision.
async fn two_calls_waiting() -> Kernel {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![
            call("c1", "quick", json!({})),
            call("c2", "quick", json!({})),
        ]),
        ModelResponse::text("done"),
    ])));
    kernel.add_tool(Arc::new(ConstTool::new("quick", "instant")));
    kernel.push(ContextItem::user("go"));
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Deciding { .. }
    ));

    kernel
}

/// Cancelling is logged the way refusing each call and then running them would be: the
/// refusals, the machine claimed, the results, and only then idle.
///
/// note: the machine went idle first and the refusals followed it, which is the reverse of
/// `decide` - so a log read in order said nothing was waiting before it said what had become of
/// what was.
#[tokio::test]
async fn cancelling_is_logged_as_refusing_and_then_running() {
    let kernel = two_calls_waiting().await;
    let before = kernel.history().len();

    assert_eq!(kernel.cancel_pending_calls("changed my mind"), 2);

    let said: Vec<String> = kernel.history()[before..]
        .iter()
        .map(|record| match &record.event {
            Event::StateChanged { to, .. } => format!("state {}", to.name()),
            event => event.name().to_owned(),
        })
        .collect();
    let at = |what: &str| -> Vec<usize> {
        said.iter()
            .enumerate()
            .filter(|(_, name)| *name == what)
            .map(|(at, _)| at)
            .collect()
    };
    let (decided, executing, finished, idle) = (
        at("permission.decided"),
        at("state executing"),
        at("tool.finished"),
        at("state idle"),
    );

    assert_eq!((decided.len(), finished.len()), (2, 2), "{said:?}");
    assert_eq!((executing.len(), idle.len()), (1, 1), "{said:?}");
    for at in &decided {
        assert!(
            matches!(
                kernel.history()[before + at].event,
                Event::PermissionDecided {
                    grant: nachalnik::Grant::Deny,
                    source: nachalnik::GrantSource::Cancellation,
                    ..
                }
            ),
            "a cancelled call is logged as refused, and refused by cancelling"
        );
    }
    assert!(decided.iter().all(|at| *at < executing[0]), "{said:?}");
    assert!(finished.iter().all(|at| *at > executing[0]), "{said:?}");
    assert!(finished.iter().all(|at| *at < idle[0]), "{said:?}");
}

/// No step begins while cancelled calls are still being answered.
///
/// note: the counter stops the kernel inside recording the first refusal, which is where a step
/// used to be able to start: the machine had already gone idle, so a request could be built with
/// calls in it that had no results yet. The machine is still claimed there now, and a step is
/// told it is busy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_step_begins_while_cancelled_calls_are_being_answered() {
    let kernel = two_calls_waiting().await;
    let (held, reached, open) = Held::new("cancelled");
    kernel.set_counter(held);

    let cancelling = {
        let kernel = kernel.clone();
        std::thread::spawn(move || kernel.cancel_pending_calls("changed my mind"))
    };
    reached
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the first refusal is being recorded");

    let answer = stepped_elsewhere(&kernel).recv_timeout(std::time::Duration::from_secs(2));
    open.send(()).expect("the counter is still waiting");
    assert_eq!(cancelling.join().expect("no panic"), 2);

    assert!(
        matches!(answer, Ok(Err(nachalnik::Error::Busy))),
        "a step began while the cancelled calls had no results: {answer:?}"
    );
}

/// An interrupt is announced before any step can spend it.
///
/// note: the session is held from outside, through `with_history`, so the announcement waits on
/// it with the flag already up. A step that could read the flag meanwhile would spend an
/// interrupt the log had not reported - and that is what a step did, because the flag was set
/// outside the lock every step reads it under. It waits now, and spends it once it is on record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupt_is_announced_before_a_step_can_spend_it() {
    let kernel = Kernel::new(Config::default());
    let (held, holding) = std::sync::mpsc::channel();
    let (open, gate) = std::sync::mpsc::channel::<()>();
    let holder = {
        let kernel = kernel.clone();
        std::thread::spawn(move || {
            kernel.with_history(|_| {
                let _ = held.send(());
                let _ = gate.recv();
            })
        })
    };
    holding.recv().expect("the session is held");

    let interrupting = {
        let kernel = kernel.clone();
        std::thread::spawn(move || kernel.interrupt())
    };
    let waited = std::time::Instant::now();
    while !kernel.is_interrupted() {
        assert!(
            waited.elapsed() < std::time::Duration::from_secs(5),
            "the flag never went up"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let stepping = stepped_elsewhere(&kernel);
    let early = stepping.recv_timeout(std::time::Duration::from_millis(300));
    open.send(()).expect("the session is still held");
    holder.join().expect("no panic");
    assert!(
        !interrupting.join().expect("no panic"),
        "the first interrupt"
    );
    let answer = stepping
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the step comes back once the interrupt is on record");

    assert!(
        early.is_err(),
        "a step spent an interrupt the log did not have yet: {early:?}"
    );
    assert_eq!(answer.expect("a step"), State::Idle);
    assert!(
        kernel
            .history()
            .iter()
            .any(|record| matches!(record.event, Event::Interrupted)),
        "and it is on record"
    );
    assert!(!kernel.is_interrupted(), "and the step spent it");
}

/// A push that lands while a turn is being recorded does not split the turn from the answer to a
/// call nobody can run.
///
/// note: the answer joins the checkpoint the turn was recorded under, and that number used to be
/// read again after the turn had let go of the context - by when a push from another thread could
/// have taken the next one. One `undo` then took the answer and the push and left the turn with a
/// call nobody answered. The race is narrow, so it is run many times over; a kernel that gets it
/// right cannot fail this, and one that gets it wrong failed every run of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_push_mid_turn_does_not_split_a_turn_from_its_answer() {
    for _ in 0..500 {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(ScriptedProvider::new([
            ModelResponse::tool_calls(vec![call("c1", "nosuch", json!({}))]),
        ])));
        kernel.set_policy(Arc::new(AllowAll));
        kernel.push(ContextItem::user("go"));

        // a push the moment the turn is recorded, which is the window
        let mut events = kernel.subscribe();
        let other = kernel.clone();
        let pusher = std::thread::spawn(move || {
            while let Ok(event) = events.blocking_recv() {
                if matches!(event, Event::ContextAdded { .. }) {
                    other.push(ContextItem::user("from elsewhere"));
                    return;
                }
            }
        });
        kernel.step().await.expect("the step ran");
        pusher.join().expect("the pusher finished");

        let items = kernel.items();
        let turn = items[1].id;
        let answer = items
            .iter()
            .find(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
            .expect("the unknown call was answered")
            .id;

        assert!(kernel.undo().unwrap());
        assert_eq!(
            kernel.item(turn).is_some(),
            kernel.item(answer).is_some(),
            "one undo took the answer to a call and left the turn that asked it, or the reverse"
        );
    }
}

/// The parameters one of them.
fn numbered(n: u64) -> nachalnik::Params {
    json!({ "n": n }).as_object().expect("an object").clone()
}

/// A snapshot holds the parameters in force at the sequence it names.
///
/// note: the parameters were read before the context lock and the sequence under it, so a
/// `set_params` in between was named by a sequence the snapshot did not reflect: resumed from it
/// and replayed against its log, the session carried parameters the log says had been replaced.
#[test]
fn a_snapshot_holds_the_parameters_its_sequence_names() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("hello"));

    let writer = std::thread::spawn({
        let kernel = kernel.clone();
        move || (1..=20_000).for_each(|n| _ = kernel.set_params(numbered(n)))
    });
    let mut snapshots = Vec::new();
    while !writer.is_finished() {
        snapshots.push(kernel.snapshot());
    }
    writer.join().expect("the writer panicked");

    let changes: Vec<_> = kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            Event::ModelParamsChanged { params } => Some((record.seq, params)),
            _ => None,
        })
        .collect();
    for snapshot in snapshots {
        let applied = changes.partition_point(|(seq, _)| *seq <= snapshot.last_seq);
        let in_force = applied
            .checked_sub(1)
            .map(|last| changes[last].1.clone())
            .unwrap_or_default();
        assert_eq!(
            snapshot.params, in_force,
            "a snapshot at record {} holds other parameters than the log",
            snapshot.last_seq
        );
    }
}

/// A snapshot's calibration is the one its items were counted with.
///
/// note: a correction was applied to the counter and the context recounted after it, under a lock
/// of its own, so a snapshot in between held the new scale and figures counted on the old one.
#[test]
fn a_snapshots_calibration_is_the_one_its_items_were_counted_with() {
    let kernel = Kernel::new(Config::default());
    kernel.set_counter(Arc::new(nachalnik::Calibrating::new(
        nachalnik::BytesPerToken::default(),
    )));
    kernel.push(ContextItem::user(
        "a sentence long enough to be counted ".repeat(40),
    ));

    let scale = |reported| nachalnik::Calibration {
        scale: reported as f64 / 1000.0,
        observations: 1,
        estimated: 1000,
        reported,
    };
    // what each correction counts the context as, measured with nothing else going on
    let mut pairs = Vec::new();
    for reported in [1000, 2000] {
        kernel.recalibrate(scale(reported));
        let snapshot = kernel.snapshot();
        pairs.push((snapshot.calibration, snapshot.items[0].tokens));
    }
    assert_ne!(pairs[0].1, pairs[1].1, "the two corrections count alike");

    let writer = std::thread::spawn({
        let kernel = kernel.clone();
        move || {
            for n in 0..5_000 {
                kernel.recalibrate(scale([1000, 2000][n % 2]));
            }
        }
    });
    let mut snapshots = Vec::new();
    while !writer.is_finished() {
        snapshots.push(kernel.snapshot());
    }
    writer.join().expect("the writer panicked");

    for snapshot in snapshots {
        let pair = (snapshot.calibration, snapshot.items[0].tokens);
        assert!(pairs.contains(&pair), "{pair:?} is neither of {pairs:?}");
    }
}

/// A request is built from one moment's tools and parameters.
///
/// note: the tools, the context and the parameters were read under three locks, one after
/// another, so a client that added a tool and then set the parameters could have a request go out
/// with the parameters and without the tool - which no prefix of the log describes. Here tool `n`
/// is always added before parameters `n`, so a request holding `k` tools holds `k - 1` or `k`.
#[test]
fn a_request_is_built_from_one_moments_tools_and_parameters() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("hello"));

    let writer = std::thread::spawn({
        let kernel = kernel.clone();
        move || {
            for n in 1..=3_000 {
                kernel.add_tool(Arc::new(ConstTool::new(format!("t{n}"), "ok")));
                kernel.set_params(numbered(n));
            }
        }
    });
    let mut requests = Vec::new();
    while !writer.is_finished() {
        requests.push(kernel.preview_request().expect("a request"));
    }
    writer.join().expect("the writer panicked");

    for request in requests {
        let tools = request.tools.len() as u64;
        let params = request
            .params
            .get("n")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        assert!(
            params == tools || params + 1 == tools,
            "a request with {tools} tools carried parameters {params}"
        );
    }
}
