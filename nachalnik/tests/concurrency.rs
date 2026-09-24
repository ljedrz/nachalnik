//! Tests for what happens when more than one thing is going on at once.
//!
//! note: There are three different stories here and they are worth keeping apart. A *fleet* is
//! many kernels, one per agent, which share nothing and need nothing; a *client* is one kernel
//! that several threads read and edit while a turn runs; and a *turn* is the one place the kernel
//! itself does several things, which is why compaction is one locked operation rather than a
//! sequence of them.

use std::sync::Arc;

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

/// Every tool result the kernel recorded, in order.
fn tool_results_text(kernel: &Kernel) -> Vec<String> {
    kernel
        .items()
        .iter()
        .filter(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .collect()
}

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

/// A provider whose `info` waits the second time it is asked, so that a second setter can be let
/// in while the first is still describing what it replaced.
///
/// note: the second time, because the first is the install: `set_provider` asks the incoming
/// provider what it is before it takes the lock. What has to wait is the call describing the
/// provider being replaced, which is the one made under it.
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
        if asked == 1
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
    let (told_first, first_asked) = std::sync::mpsc::channel();
    let (told_third, third_asked) = std::sync::mpsc::channel();
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(SlowInfo::new("first", Some(gate), told_first));
    let mut events = kernel.subscribe();

    // the swap that replaces `first` waits inside the call describing it, which is made under the
    // lock. Announcing after the lock is let go would let the next swap in here: it would apply
    // second and be logged first, and the log's last word on the provider would name the one that
    // is not installed
    let second = {
        let kernel = kernel.clone();
        let (told, _) = std::sync::mpsc::channel();
        std::thread::spawn(move || kernel.set_provider(SlowInfo::new("second", None, told)))
    };
    // `first` asked the second time is the swap to `second`, held under the lock
    while first_asked.recv().unwrap() < 2 {}

    let third = {
        let kernel = kernel.clone();
        std::thread::spawn(move || kernel.set_provider(SlowInfo::new("third", None, told_third)))
    };
    // `third` asked once is its swap about to take the lock. Nothing marks a thread as waiting on
    // one, so a moment more is what puts it there
    third_asked.recv().unwrap();
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
