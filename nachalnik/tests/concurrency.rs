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

fn three_slow_calls(parallel: bool) -> Kernel {
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
    kernel.add_tool(Arc::new(Slow(std::time::Duration::from_millis(150))));
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
            call("c3", "quick", json!({})),
        ]),
        ModelResponse::text("done"),
    ])));
    kernel.set_policy(Arc::new(nachalnik::test::Table::new(
        nachalnik::Verdict::Allow,
    )));
    kernel.add_tool(Arc::new(Slow(std::time::Duration::from_millis(100))));
    kernel.add_tool(Arc::new(ConstTool::new("quick", "instant")));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.unwrap();

    // the slow one finished last and is still recorded second
    let answered: Vec<_> = kernel
        .items()
        .iter()
        .filter_map(|item| match &item.kind {
            nachalnik::ContextKind::ToolResult { call, .. } => Some(call.0.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(answered, ["c1", "c2", "c3"]);
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
