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

/// The same protection, for the client that reads a snapshot *into* a session it is already
/// running rather than resuming from it. It keeps the kernel it has, so nothing has told that
/// kernel about the identifiers the items it is pushing already carry.
#[tokio::test]
async fn a_merged_snapshot_can_declare_the_identifiers_it_brings() {
    let snapshot = worked_session().await.snapshot();
    assert!(snapshot.used_calls.iter().any(|c| c.0 == "c1"));

    // a live session, and the saved conversation pushed into it item by item
    let (live, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("done"),
    ]);
    live.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    live.push_all(snapshot.items);

    let mut events = live.subscribe();
    assert_eq!(live.reserve_calls(snapshot.used_calls.clone()), 1);
    assert_eq!(
        live.reserve_calls(snapshot.used_calls),
        0,
        "and saying it twice reserves nothing twice"
    );
    let reserved = std::iter::from_fn(|| events.try_recv().ok())
        .filter(|event| matches!(event, Event::ToolCallsReserved { reserved: 1 }))
        .count();
    assert_eq!(reserved, 1, "the one that did something is on the log");

    // the provider now offers `c1` again, as one that numbers from zero every turn would
    live.push(ContextItem::user("again"));
    live.turn().await.unwrap();

    let repaired = std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(event, Event::ToolCallRepaired { reason, .. } if reason.contains("session"))
    });
    assert!(
        repaired,
        "an identifier the merged session brought was reused"
    );

    // which is the point: the request that follows does not carry one `tool_call_id` twice
    let sent: Vec<String> = live
        .preview_request()
        .unwrap()
        .messages
        .iter()
        .flat_map(|message| message.calls().map(|c| c.id.0.clone()).collect::<Vec<_>>())
        .collect();
    let mut unique = sent.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(sent.len(), unique.len(), "{sent:?}");
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
            input_tokens: Some(1_300),
            ..nachalnik::Usage::default()
        }),
        ..ModelResponse::text("hello")
    }])));
    // a thousand tokens of it, because a request smaller than that is one the counter is right to
    // learn nothing from; see `WORTH_LEARNING_FROM`
    kernel.push(ContextItem::user("a".repeat(4_000)));
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
    // resuming recounts, which it has always done - and now it recounts with the lesson applied,
    // so the items come back carrying the corrected figures rather than the ones they were pushed
    // with, before the counter had been told anything
    let stale: usize = kernel.items().iter().map(|item| item.tokens).sum();
    let recounted: usize = resumed.items().iter().map(|item| item.tokens).sum();
    assert!(
        recounted > stale,
        "the correction reaches the items a resume counts: {stale} -> {recounted}"
    );
    assert_eq!(
        recounted,
        kernel
            .items()
            .iter()
            .map(|item| kernel.counter().count_item(item))
            .sum::<usize>(),
        "which is exactly what a `recount` before saving would have produced"
    );

    // the budget, though, is counted over the projection with whatever the counter knows *now*,
    // so both kernels already agree about what the next request costs - the one that never
    // recounted its items included. A correction the budget only showed after a save and a
    // reload would be a correction nobody watching the status line ever saw
    assert_eq!(kernel.budget().used(), resumed.budget().used());
}

/// A resumed log numbers on from the one it carries on from, and so do the questions it asks.
///
/// note: both started again at 1, so a client keeping a `history_since` cursor across a resume
/// read nothing until the new log passed the old number, and a log kept across the resume had two
/// records under each number and two questions under one identifier.
#[tokio::test]
async fn a_resumed_session_numbers_on_from_where_it_stopped() {
    let asked = |kernel: &Kernel| {
        kernel
            .history()
            .into_iter()
            .filter_map(|record| match record.event {
                Event::PermissionRequested { request } => Some(request.id),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let (kernel, _) = common::inquisitive([ModelResponse::tool_calls(vec![call(
        "c1",
        "peek",
        json!({}),
    )])]);
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    kernel.push(ContextItem::user("have a look"));
    kernel.turn().await.unwrap();
    let before = asked(&kernel);
    assert_eq!(before.len(), 1, "the first session asked once");

    let resumed = Kernel::resume(Config::default(), kernel.snapshot());
    assert_eq!(
        resumed.history()[0].seq,
        kernel.last_seq() + 1,
        "the resume is the record after the last one"
    );

    resumed.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c2", "peek", json!({}))]),
    ])));
    resumed.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    resumed.push(ContextItem::user("again"));
    resumed.turn().await.unwrap();
    let after = asked(&resumed);
    assert_eq!(after.len(), 1, "the resumed session asked once");
    assert!(
        after[0] > before[0],
        "{:?} was asked before the resume, so {:?} cannot be its number",
        before[0],
        after[0]
    );
}

/// A snapshot written before the numbering was kept still resumes, and numbers from the start.
#[tokio::test]
async fn a_snapshot_written_before_the_numbering_was_kept_still_resumes() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("hello"));

    let mut json = serde_json::to_value(kernel.snapshot()).unwrap();
    let fields = json.as_object_mut().unwrap();
    fields.remove("last_seq");
    fields.remove("next_permission");
    let snapshot: Snapshot = serde_json::from_value(json).unwrap();

    let resumed = Kernel::resume(Config::default(), snapshot);
    assert_eq!(resumed.history()[0].seq, 1);
    assert_eq!(resumed.items().len(), 1);
}

/// A snapshot whose items were written before they said what was unpriced, or why they were
/// there, still resumes.
#[tokio::test]
async fn a_snapshot_written_before_items_said_what_was_unpriced_still_resumes() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("hello"));

    let mut json = serde_json::to_value(kernel.snapshot()).unwrap();
    for item in json["items"].as_array_mut().unwrap() {
        let fields = item.as_object_mut().unwrap();
        fields.remove("uncounted");
        fields.remove("included_because");
    }
    let snapshot: Snapshot = serde_json::from_value(json).unwrap();

    let resumed = Kernel::resume(Config::default(), snapshot);
    assert_eq!(resumed.items().len(), 1);
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

/// Every seam that can be swapped says so, and says what was swapped.
///
/// note: four of the six were silent - the projector, the counter and the compactor entirely, and
/// the policy about *which* policy - so a session log could not answer "what was projecting these
/// requests?" for a session where somebody had changed it half way. That is the question a log is
/// for.
#[tokio::test]
async fn replacing_a_seam_is_an_event_that_names_it() {
    let kernel = Kernel::new(Config::default());

    kernel.set_policy(Arc::new(nachalnik::test::AllowAll));
    kernel.set_projector(Arc::new(nachalnik::LinearProjector::default()));
    kernel.set_counter(Arc::new(nachalnik::BytesPerToken::default()));
    kernel.set_compactor(Some(Arc::new(
        nachalnik::test::LargestFirstCompactor::default(),
    )));
    kernel.set_compactor(None);

    let names: Vec<String> = kernel.with_history(|session| {
        session
            .records()
            .map(|record| record.event.name().to_owned())
            .collect()
    });
    for expected in [
        "policy.changed",
        "projector.changed",
        "counter.changed",
        "compactor.changed",
    ] {
        assert!(names.contains(&expected.to_owned()), "{names:?}");
    }

    // ... and what they name is what was plugged in, rather than that something was
    let events: Vec<Event> = kernel.with_history(|session| {
        session
            .records()
            .map(|record| record.event.clone())
            .collect()
    });
    let policy = events
        .iter()
        .find_map(|event| match event {
            Event::PolicyChanged { from, to } => Some((from.clone(), to.clone())),
            _ => None,
        })
        .expect("the policy was replaced");
    assert!(policy.0.contains("AskAlways"), "{policy:?}");
    assert!(policy.1.contains("AllowAll"), "{policy:?}");

    // removing the compactor is a change like any other: from then on nothing is ever dropped, and
    // a log that only recorded the setting would not say when that stopped being true
    let removed = events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::CompactorChanged { from, to } => Some((from.clone(), to.clone())),
            _ => None,
        })
        .expect("it was removed");
    assert!(removed.0.is_some() && removed.1.is_none(), "{removed:?}");
}

/// A payload survives a snapshot, nested where a real client puts one, with its `meta` intact.
///
/// note: three things that each break silently and none of which any other test here reaches. A
/// `Content::Blob` inside a `Content::Blocks` is the one shape a client attaching a file actually
/// produces - a sentence and the payload - and serde has to carry the nesting. `Blob::meta` is
/// `skip_serializing_if = "is_null"`, so a blob with nothing on it and a blob with a filename on
/// it take two different paths through the same `Deserialize`. And the count has to come back
/// too: a resumed context that reported a picture as free would be the abstention lost at exactly
/// the moment nobody would look for it.
#[tokio::test]
async fn a_payload_survives_a_snapshot_with_what_was_known_about_it() {
    use nachalnik::{Blob, Block, Content};

    let (kernel, _) = permissive([ModelResponse::text("unreached")]);
    let named = Blob::new("application/pdf", "JVBERi0=").with_meta(json!({ "name": "q3.pdf" }));
    kernel.push(
        ContextItem::file(
            "q3.pdf",
            Content::blocks([
                Block::text(Content::Blob(Arc::new(named))),
                Block::text(Content::text("(attached from q3.pdf)")),
            ]),
        )
        .pinned(),
    );
    // and one with nothing known about it, which is the other serde path
    kernel.push(ContextItem::user(Content::blob("image/png", "iVBORw0=")));

    let before = kernel.items();
    let budget = kernel.budget();
    assert_eq!(
        budget.uncounted, 2,
        "neither is priced by the default counter"
    );

    let json = serde_json::to_string(&kernel.snapshot()).unwrap();
    let resumed = Kernel::resume(Config::default(), serde_json::from_str(&json).unwrap());

    assert_eq!(
        resumed.items(),
        before,
        "every item, payload and state included"
    );

    // the payload itself, through the nesting
    let items = resumed.items();
    let carried = items[0].content.blobs();
    let blob = carried.first().expect("the blob is still in there");
    assert_eq!(&*blob.media_type, "application/pdf");
    assert_eq!(&*blob.data, "JVBERi0=");
    // what the producer knew, which is what a provider reads to name the attachment part
    assert_eq!(
        blob.meta.get("name").and_then(|n| n.as_str()),
        Some("q3.pdf")
    );
    // and the one with no meta comes back with none rather than with something invented
    assert!(
        resumed.items()[1]
            .content
            .as_blob()
            .expect("a blob")
            .meta
            .is_null(),
        "an empty meta is absent on the wire and null on the way back"
    );

    // the abstention is a property of the content, so it is recounted rather than restored - and
    // it has to come out the same
    assert_eq!(resumed.budget().uncounted, 2);
    assert!(!resumed.budget().fully_counted());
}

/// A snapshot whose items name a call `used_calls` does not list still has that call reserved.
///
/// note: `resume` reserved only what the list said, and a snapshot merged or written by hand can
/// leave a call out of it - so a provider handing that identifier back went unrepaired, and the
/// request after it carried one `tool_call_id` twice.
#[tokio::test]
async fn a_call_the_items_name_is_reserved_whatever_the_list_says() {
    let kernel = worked_session().await;
    let mut snapshot = kernel.snapshot();
    snapshot.used_calls.clear();
    assert!(
        snapshot.problems().iter().any(|it| it.contains("`c1`")),
        "{:?}",
        snapshot.problems()
    );

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

    let repaired = std::iter::from_fn(|| events.try_recv().ok())
        .any(|event| matches!(event, Event::ToolCallRepaired { .. }));
    assert!(
        repaired,
        "an identifier the items already hold was handed out again"
    );
}

/// Two items sharing an identifier, or one with none, are given one of their own on resume, and
/// the snapshot says so first.
#[test]
fn an_identifier_two_items_share_is_given_a_new_one() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("one"));
    kernel.push(ContextItem::user("two"));
    kernel.push(ContextItem::user("three"));
    let mut snapshot = kernel.snapshot();
    snapshot.items[1].id = snapshot.items[0].id;
    snapshot.items[2].id = nachalnik::ContextId(0);
    let problems = snapshot.problems();
    assert!(
        problems.iter().any(|it| it.contains("both numbered")),
        "{problems:?}"
    );
    assert!(
        problems.iter().any(|it| it.contains("no identifier")),
        "{problems:?}"
    );

    let resumed = Kernel::resume(Config::default(), snapshot);
    let ids: Vec<_> = resumed.items().iter().map(|item| item.id.0).collect();
    let mut distinct = ids.clone();
    distinct.dedup();
    assert_eq!(ids, distinct, "every item is its own: {ids:?}");
    assert!(!ids.contains(&0), "{ids:?}");
    let pushed = resumed.push(ContextItem::user("four"));
    assert!(
        !ids.contains(&pushed.0),
        "and the next is new: {ids:?} and {pushed}"
    );
}

/// Items out of order are named as a problem, because resuming puts them back in order - which
/// changes the conversation the model is shown.
#[test]
fn items_out_of_order_are_named_before_resume_sorts_them() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("one"));
    kernel.push(ContextItem::user("two"));
    let mut snapshot = kernel.snapshot();
    assert!(snapshot.problems().is_empty(), "{:?}", snapshot.problems());
    snapshot.items.swap(0, 1);
    let problems = snapshot.problems();
    assert!(
        problems.iter().any(|it| it.contains("comes after")),
        "{problems:?}"
    );

    let resumed = Kernel::resume(Config::default(), snapshot);
    let ids: Vec<_> = resumed.items().iter().map(|item| item.id.0).collect();
    assert_eq!(ids, [1, 2], "the repair the problem warned of");
}

/// A `next_item` an item has already reached is named as a problem, because resuming moves it.
#[test]
fn a_next_item_the_items_have_passed_is_named() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("one"));
    kernel.push(ContextItem::user("two"));
    let mut snapshot = kernel.snapshot();
    for stale in [0, 1, 2] {
        snapshot.next_item = stale;
        let problems = snapshot.problems();
        assert!(
            problems.iter().any(|it| it.contains("`next_item`")),
            "{stale}: {problems:?}"
        );
    }

    let resumed = Kernel::resume(Config::default(), snapshot);
    assert_eq!(resumed.push(ContextItem::user("three")).0, 3);
}

/// Numbers at the top of what a `u64` holds are named as a problem, and resuming one anyway is
/// not a panic.
#[test]
fn a_snapshot_numbered_at_the_top_is_named_and_does_not_panic() {
    let kernel = Kernel::new(Config::default());
    kernel.push(ContextItem::user("hello"));
    let mut snapshot = kernel.snapshot();
    snapshot.last_seq = u64::MAX;
    snapshot.next_item = u64::MAX;
    assert_eq!(snapshot.problems().len(), 2, "{:?}", snapshot.problems());

    let resumed = Kernel::resume(Config::default(), snapshot);
    resumed.push(ContextItem::user("and on"));
    assert_eq!(resumed.items().len(), 2);
}

/// What the counter learned comes back exactly, whatever the ratio.
///
/// note: the scale is a float, and one read back from JSON is not always the one that was
/// written, so a resumed session counted on a scale a digit off the one it was saved with. A sweep
/// rather than one figure, because which ratios survive the round trip is a matter of luck.
#[tokio::test]
async fn what_the_counter_learned_comes_back_exactly_whatever_the_ratio() {
    for reported in 1_200..1_400 {
        let kernel = Kernel::new(Config::default());
        kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse {
            usage: Some(nachalnik::Usage {
                input_tokens: Some(reported),
                ..nachalnik::Usage::default()
            }),
            ..ModelResponse::text("hello")
        }])));
        // an estimate of 1,003 rather than a round thousand, whose ratios are short decimals that
        // survive the trip whatever happens to them
        kernel.push(ContextItem::user("a".repeat(4_012)));
        kernel.turn().await.unwrap();
        let learned = kernel.counter().calibration().expect("the default learns");

        let snapshot: Snapshot =
            serde_json::from_str(&serde_json::to_string(&kernel.snapshot()).unwrap()).unwrap();
        let resumed = Kernel::resume(Config::default(), snapshot);
        assert_eq!(
            resumed.counter().calibration(),
            Some(learned),
            "reported {reported}"
        );
    }
}
