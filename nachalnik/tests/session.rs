//! Tests for what survives the end of a process: snapshots, resumption and the log.

mod common;

use std::sync::Arc;

use common::permissive;
use nachalnik::{
    Config, ContextItem, ContextKind, ContextState, Event, Kernel, ModelResponse, Params, Record,
    Role, Snapshot, State, StopReason,
    selectors::Selector,
    test::{ConstTool, ScriptedProvider, call},
};
use serde_json::json;

/// A session with a turn, a tool exchange, a pruned item and a pinned one behind it.
async fn worked_session() -> Kernel {
    let (kernel, _) = permissive([
        ModelResponse {
            content: Some("looking".into()),
            reasoning: Some("i should look".into()),
            tool_calls: vec![
                call("c1", "peek", json!({})).with_extra(json!({ "signature": "abc" })),
            ],
            stop: StopReason::ToolUse,
            usage: None,
            raw: None,
        },
        ModelResponse::text("it says ok"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    kernel.push(ContextItem::file("src/a.rs", "fn a() {}").pinned());
    kernel.push(ContextItem::user("have a look"));
    kernel.turn().await.unwrap();

    let noise = kernel.push(ContextItem::file("src/big.rs", "z".repeat(400)));
    kernel.set_state([noise], ContextState::Excluded, Some("too big".into()));

    kernel
}

#[tokio::test]
async fn a_session_comes_back_exactly_as_it_was_left() {
    let kernel = worked_session().await;
    let before = kernel.items();
    let snapshot = kernel.snapshot();

    // over the wire, as a client persisting it would
    let json = serde_json::to_string(&snapshot).unwrap();
    let snapshot: Snapshot = serde_json::from_str(&json).unwrap();

    let resumed = Kernel::resume(Config::default(), snapshot);

    assert_eq!(resumed.session_name(), kernel.session_name());
    assert_eq!(resumed.items(), before, "every item, id and state included");
    assert_eq!(resumed.state(), State::Idle);

    // the things that are easy to lose: a pin, a reason, a signature, a piece of reasoning
    let pinned = resumed.items()[0].clone();
    assert_eq!(pinned.state, ContextState::Pinned);
    let excluded = resumed.items().last().unwrap().clone();
    assert_eq!(excluded.note.as_deref(), Some("too big"));

    let turn = resumed
        .items()
        .into_iter()
        .find(|item| item.source == "model")
        .unwrap();
    assert_eq!(
        turn.reasoning()
            .map(|r| r.to_text().into_owned())
            .as_deref(),
        Some("i should look")
    );
    let ContextKind::AssistantMessage { tool_calls, .. } = &turn.kind else {
        unreachable!()
    };
    assert_eq!(*tool_calls[0].extra, json!({ "signature": "abc" }));
}

#[tokio::test]
async fn a_resumed_session_carries_on_where_it_stopped() {
    let kernel = worked_session().await;
    let snapshot = kernel.snapshot();

    let resumed = Kernel::resume(Config::default(), snapshot);
    let provider = Arc::new(ScriptedProvider::new([ModelResponse::text("still here")]));
    resumed.set_provider(provider.clone());
    resumed.push(ContextItem::user("and now?"));

    assert!(matches!(
        resumed.turn().await.unwrap(),
        State::Finished { .. }
    ));

    // the whole conversation went back out, minus what was pruned
    let sent = &provider.requests()[0];
    assert!(sent.messages.iter().any(|m| m.role == Role::Tool));
    assert!(
        !sent.messages.iter().any(|m| m
            .content
            .as_ref()
            .is_some_and(|c| c.to_text().contains("zzz"))),
        "the pruned item stayed pruned"
    );
}

#[tokio::test]
async fn a_resumed_session_does_not_reuse_an_identifier() {
    let kernel = worked_session().await;
    let snapshot = kernel.snapshot();
    assert!(
        snapshot.used_calls.iter().any(|c| c.0 == "c1"),
        "{:?}",
        snapshot.used_calls
    );

    // the provider offers `c1` again, as one that numbers from zero every turn would
    let resumed = Kernel::resume(Config::default(), snapshot);
    resumed.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("done"),
    ])));
    resumed.set_policy(Arc::new(nachalnik::test::AllowAll));
    resumed.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    resumed.push(ContextItem::user("again"));

    let mut events = resumed.subscribe();
    resumed.turn().await.unwrap();

    let repaired = std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(event, Event::ToolCallRepaired { reason, .. } if reason.contains("session"))
    });
    assert!(repaired, "an identifier from before the restart was reused");

    // and the new item identifiers carry on rather than starting over
    let ids: Vec<_> = resumed.items().iter().map(|i| i.id.0).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(ids, sorted, "identifiers are unique and in order: {ids:?}");
}

#[tokio::test]
async fn a_fork_is_a_resume_under_another_name() {
    let kernel = worked_session().await;
    let snapshot = kernel.snapshot();

    let fork = Kernel::resume(
        Config {
            session_name: Some("a fork".into()),
            ..Default::default()
        },
        snapshot,
    );
    assert_eq!(fork.session_name(), "a fork");
    assert_ne!(fork.session_name(), kernel.session_name());
    assert_eq!(fork.items().len(), kernel.items().len());

    // and resuming is one event, not one per item
    let resumed = fork
        .history()
        .into_iter()
        .filter(|r| r.event.name() == "session.resumed")
        .count();
    assert_eq!(resumed, 1);
    assert_eq!(
        fork.history()
            .iter()
            .filter(|r| r.event.name() == "context.added")
            .count(),
        0
    );
}

#[tokio::test]
async fn the_log_is_only_given_up_when_it_is_asked_for() {
    let kernel = worked_session().await;
    let all = kernel.history();
    assert!(all.len() > 4);

    let cut = all[2].seq;
    let taken = kernel.drain_history(cut);

    assert_eq!(
        taken,
        all[..3].to_vec(),
        "oldest first, up to and including"
    );
    assert_eq!(
        kernel.history(),
        all[3..].to_vec(),
        "and the rest is still there"
    );
    assert_eq!(
        kernel.last_seq(),
        all.last().unwrap().seq,
        "draining does not renumber anything"
    );

    // taking nothing takes nothing
    assert!(kernel.drain_history(0).is_empty());

    // what came out is what a client would write to a file, one line each
    let lines: Vec<String> = taken
        .iter()
        .map(|record| serde_json::to_string(record).unwrap())
        .collect();
    let restored: Vec<Record> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(restored, taken);
}

#[tokio::test]
async fn a_snapshot_is_not_the_log_and_says_so() {
    let kernel = worked_session().await;

    // the point of keeping both: the log names the items, the snapshot carries them
    let added = kernel
        .history()
        .into_iter()
        .find(|r| r.event.name() == "context.added")
        .unwrap();
    let json = serde_json::to_string(&added).unwrap();
    assert!(
        !json.contains("fn a() {}"),
        "an event carrying its item's contents would not be affordable to keep: {json}"
    );

    let snapshot = kernel.snapshot();
    let json = serde_json::to_string(&snapshot).unwrap();
    assert!(json.contains("fn a() {}"), "but a snapshot has to");

    // a counter swapped between the snapshot and the resume gives honest numbers, not stale ones
    let resumed = Kernel::resume(Config::default(), snapshot);
    resumed.set_counter(Arc::new(nachalnik::BytesPerToken { bytes_per_token: 1 }));
    let file = "file:src/a.rs"
        .parse::<Selector>()
        .unwrap()
        .matches(&resumed.items())[0];
    assert_eq!(resumed.item(file).unwrap().tokens, 9);
}

#[tokio::test]
async fn what_the_counter_learned_survives_a_restart() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse {
        usage: Some(nachalnik::Usage {
            input_tokens: Some(900),
            ..nachalnik::Usage::default()
        }),
        ..ModelResponse::text("hello")
    }])));
    kernel.push(ContextItem::user("a".repeat(400)));
    kernel.turn().await.unwrap();

    let learned = kernel.counter().calibration().expect("the default learns");
    assert_eq!(learned.observations, 1);
    assert!(learned.scale > 1.0);

    // through `serde`, because that is how a session actually comes back
    let snapshot = kernel.snapshot();
    let snapshot: Snapshot =
        serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
    let resumed = Kernel::resume(Config::default(), snapshot);

    assert_eq!(
        resumed.counter().calibration(),
        Some(learned),
        "a session long enough to resume has already paid for this lesson"
    );
    // resuming recounts, which it has always done - and now it recounts with the lesson applied.
    // The figures that come back are therefore *not* the ones the session was closed on: they are
    // what the session would have shown had it called `recount` before saving, which is nearer to
    // what the provider actually charged than the stale estimate was
    let (before, after) = (kernel.budget().used(), resumed.budget().used());
    assert!(
        after > before,
        "the correction reaches the items a resume counts: {before} -> {after}"
    );
    let recounted: usize = kernel
        .items()
        .iter()
        .map(|item| kernel.counter().count_item(item))
        .sum();
    assert_eq!(
        after, recounted,
        "which is exactly what a `recount` before saving would have produced"
    );
}

#[tokio::test]
async fn a_snapshot_written_before_the_counter_learned_anything_still_resumes() {
    // the field is `serde(default)`: a snapshot from an older version has no `calibration` key at
    // all, and resuming has to mean "nothing learned" rather than a parse error
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("hello"));

    let mut json = serde_json::to_value(kernel.snapshot()).unwrap();
    json.as_object_mut().unwrap().remove("calibration");
    let snapshot: Snapshot = serde_json::from_value(json).unwrap();
    assert_eq!(snapshot.calibration, None);

    let resumed = Kernel::resume(Config::default(), snapshot);
    assert_eq!(resumed.items().len(), 1);
    assert_eq!(
        resumed.counter().calibration(),
        Some(nachalnik::Calibration::default()),
        "which is exactly what it had"
    );
}

#[tokio::test]
async fn the_log_can_be_asked_a_question_without_being_copied() {
    let kernel = worked_session().await;

    let copied = kernel.history();
    let (counted, last) = kernel.with_history(|session| {
        (
            session.records().count(),
            session.records().last().map(|r| r.seq),
        )
    });

    assert_eq!(counted, copied.len());
    assert_eq!(last, Some(kernel.last_seq()));
    assert!(counted > 1, "there is a session here to ask about");
}

#[tokio::test]
async fn parameters_survive_a_restart() {
    let kernel = worked_session().await;
    let mut params = Params::new();
    params.insert("temperature".into(), json!(0.0));
    kernel.set_params(params.clone());

    let resumed = Kernel::resume(Config::default(), kernel.snapshot());
    assert_eq!(resumed.params(), params);
}
