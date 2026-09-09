//! What gets run: tool calls matched to tools, executed, shortened where a limit says so, and
//! handed back.
//!
//! note: a failing tool is not a kernel error here, and neither is an unknown one - both become
//! an error tool result the model is shown, because that is information the model needs and a
//! loop that stopped would be a loop the model cannot recover from. The output-limit tests are
//! the other half of the same idea: what the model is shown is shortened, and the whole of it is
//! archived beside the short copy rather than thrown away.

use std::sync::Arc;

use nachalnik::{
    Capability, Config, ContextItem, ContextKind, ContextState, Event, Kernel, ModelResponse, Role,
    State, StopReason,
    test::{AllowAll, BrokenTool, ConstTool, EchoTool, ScriptedProvider, call},
};
use serde_json::json;

use crate::{
    common::{count, drain, names, permissive},
    tool_results,
};

#[tokio::test]
async fn tool_calls_are_executed_and_handed_back_to_the_model() {
    let (kernel, provider) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "echo", json!({ "value": "x" }))]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(EchoTool::new("echo", [Capability::Read])));
    kernel.push(ContextItem::user("echo x"));

    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools.len(), 1, "the tool was offered");
    assert_eq!(requests[0].tools[0].capabilities, vec![Capability::Read]);

    let second = &requests[1];
    assert_eq!(second.messages.len(), 3);
    assert_eq!(second.messages[1].role, Role::Assistant);
    assert_eq!(second.messages[1].tool_calls.len(), 1);
    assert_eq!(second.messages[2].role, Role::Tool);
    assert_eq!(
        second.messages[2].tool_call_id.as_ref().unwrap().0,
        "c1",
        "the result is matched to its call"
    );
    assert_eq!(
        second.messages[2].content.as_ref().unwrap().to_text(),
        r#"{"value":"x"}"#
    );
    assert_eq!(tool_results(&kernel).len(), 1);
}

#[tokio::test]
async fn a_truncated_turn_does_not_look_like_a_finished_one() {
    // a reasoning model that spends its whole output budget thinking says nothing at all, and
    // the only sign is the stop reason
    let (kernel, _) = permissive([ModelResponse {
        content: None,
        reasoning: Some("hmm, let me think about th".into()),
        tool_calls: Vec::new(),
        stop: StopReason::Length,
        usage: None,
        raw: None,
    }]);
    kernel.push(ContextItem::user("write me a paragraph"));

    let state = kernel.turn().await.unwrap();
    let State::Finished { item, stop } = state else {
        panic!("no tools were involved: {state:?}")
    };
    assert_eq!(stop, StopReason::Length);
    assert_eq!(kernel.state(), State::Finished { item, stop });

    // the empty turn is recorded, because it happened - and it costs what the model spent
    // thinking, because that is what it cost
    let turn = kernel.item(item).unwrap();
    assert_eq!(turn.content.to_text(), "");
    assert_eq!(
        turn.reasoning()
            .map(|r| r.to_text().into_owned())
            .as_deref(),
        Some("hmm, let me think about th"),
        "the reasoning is kept on the turn that produced it"
    );
    assert_eq!(
        turn.tokens, 7,
        "and it is counted, rather than reported as free"
    );

    // it is still left out of the next request, because most providers reject a turn with no
    // content - and the projector says that the reasoning went with it
    let projection = kernel.project();
    assert_eq!(projection.skipped.len(), 1);
    assert_eq!(projection.skipped[0].id, item);
    assert!(
        projection.skipped[0]
            .reason
            .contains("its reasoning goes with it")
    );
    assert_eq!(projection.messages.len(), 1);
}

#[tokio::test]
async fn unusable_tool_call_identifiers_are_repaired_and_announced() {
    let (kernel, provider) = permissive([
        ModelResponse {
            content: None,
            reasoning: None,
            tool_calls: vec![
                call("dup", "echo", json!({ "value": "one" })),
                // the same identifier twice, and then none at all: both happen in the wild,
                // the second one whenever a streamed call's first fragment carries no id
                call("dup", "echo", json!({ "value": "two" })),
                call("", "echo", json!({ "value": "three" })),
            ],
            stop: StopReason::ToolUse,
            usage: None,
            raw: None,
        },
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(EchoTool::new("echo", [])));
    kernel.push(ContextItem::user("echo three things"));

    let mut events = kernel.subscribe();
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));

    let repairs: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::ToolCallRepaired { call, was, reason } => Some((call.0, was, reason)),
            _ => None,
        })
        .collect();
    assert_eq!(
        repairs,
        [
            (
                "call_1".to_owned(),
                "dup".to_owned(),
                "the provider used the identifier twice in one response".to_owned()
            ),
            (
                "call_2".to_owned(),
                String::new(),
                "the provider left the identifier empty".to_owned()
            ),
        ]
    );

    // the model's turn and the results agree, which is the point of the exercise
    let assistant = kernel
        .items()
        .into_iter()
        .find(|item| item.source == "model")
        .unwrap();
    let ContextKind::AssistantMessage { tool_calls, .. } = &assistant.kind else {
        unreachable!()
    };
    let requested: Vec<_> = tool_calls.iter().map(|c| c.id.0.clone()).collect();
    assert_eq!(requested, ["dup", "call_1", "call_2"]);

    let answered: Vec<_> = tool_results(&kernel)
        .iter()
        .map(|item| match &item.kind {
            ContextKind::ToolResult { call, .. } => call.0.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(answered, requested);

    // so the request that goes out is a valid one, with nothing repaired away
    let second = &provider.requests()[1];
    assert_eq!(second.messages[1].tool_calls.len(), 3);
    assert_eq!(
        second
            .messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count(),
        3
    );
    assert!(kernel.project().repairs.is_empty());
}

#[tokio::test]
async fn an_unknown_tool_is_an_error_result_not_a_crash() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "teleport", json!({}))]),
        ModelResponse::text("sorry"),
    ]);
    kernel.push(ContextItem::user("teleport me"));

    let mut events = kernel.subscribe();
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));
    assert!(names(&mut events).contains(&"tool.unknown".to_owned()));

    let results = tool_results(&kernel);
    assert_eq!(results.len(), 1);
    assert!(results[0].content.to_text().contains("no tool named"));
}

#[tokio::test]
async fn a_failing_tool_is_reported_to_the_model() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "broken", json!({}))]),
        ModelResponse::text("ok"),
    ]);
    kernel.add_tool(Arc::new(BrokenTool::new("broken")));
    kernel.push(ContextItem::user("try it"));

    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));
    let results = tool_results(&kernel);
    assert!(results[0].content.to_text().contains("this tool is broken"));
    assert!(matches!(
        results[0].kind,
        nachalnik::ContextKind::ToolResult { is_error: true, .. }
    ));
}

#[tokio::test]
async fn a_running_tool_reports_its_progress() {
    let (kernel, _) = permissive([ModelResponse::tool_calls(vec![call(
        "c1",
        "chatty",
        json!({}),
    )])]);
    kernel.add_tool(Arc::new(ConstTool::new("chatty", "half a result")));
    kernel.push(ContextItem::user("go"));

    let mut events = kernel.subscribe();
    kernel.step().await.unwrap();
    kernel.step().await.unwrap();

    let events = drain(&mut events);
    assert_eq!(count(&events, "tool.output"), 1);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolOutput { chunk, .. } if chunk == "half a result"
    )));
    assert_eq!(
        kernel
            .history()
            .iter()
            .filter(|r| r.event.name() == "tool.output")
            .count(),
        0,
        "progress is broadcast, not recorded"
    );
}

#[tokio::test]
async fn output_limits_are_enforced_and_admitted() {
    let (kernel, _) = permissive([ModelResponse::tool_calls(vec![
        call("c1", "chatty", json!({})),
        call("c2", "verbose", json!({})),
    ])]);
    kernel.add_tool(Arc::new(
        ConstTool::new("chatty", "x".repeat(1_000)).with_output_limit(100),
    ));
    kernel.add_tool(Arc::new(ConstTool::new("verbose", "y".repeat(1_000))));
    kernel.push(ContextItem::user("talk"));

    let mut events = kernel.subscribe();
    kernel.step().await.unwrap();
    assert!(matches!(kernel.step().await.unwrap(), State::Idle));

    // a truncated output is recorded twice: the whole of it, archived, and the truncated copy
    // the model is shown
    let results = tool_results(&kernel);
    assert_eq!(results.len(), 3, "two results, one of them a pair");

    let (whole, shown) = (results[0].clone(), results[1].clone());
    assert_eq!(whole.state, ContextState::Archived);
    assert!(!whole.is_projected(), "the model is not shown it");
    assert_eq!(
        whole.content.to_text().len(),
        1_000,
        "and not a byte of it was thrown away"
    );

    let truncated = shown.content.to_text().into_owned();
    // the limit is a limit: the admission of truncation is paid for out of the same budget
    assert_eq!(truncated.len(), 100);
    assert!(truncated.starts_with("xxxx"));
    assert!(
        truncated.contains("949 bytes truncated by an output limit"),
        "{truncated}"
    );
    // note: `included_because` and not `note`. A note says why an item is in its current state
    // and is replaced whenever that changes; this one is `Active`, and being a shortened copy is
    // a fact about what it holds rather than about any state it happens to be in. Kept in the
    // note it was destroyed the first time anybody cycled the row, which took the only sentence
    // saying which item held the whole
    assert_eq!(
        shown.included_because.as_deref(),
        Some(&*format!(
            "949 bytes were truncated by the output limit; the whole output is item {}",
            whole.id
        ))
    );
    assert_eq!(
        whole.included_because.as_deref(),
        Some("the whole of a tool output an output limit shortened")
    );

    assert_eq!(
        results[2].content.to_text().len(),
        1_000,
        "the untruncated one"
    );

    let finished: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|e| match e {
            Event::ToolFinished {
                truncated, whole, ..
            } => Some((truncated, whole)),
            _ => None,
        })
        .collect();
    assert_eq!(finished, vec![(Some(949), Some(whole.id)), (None, None)]);

    // and getting the whole of it back to the model is a state change like any other
    kernel.set_state([shown.id], ContextState::Excluded, Some("too short".into()));
    kernel.set_state([whole.id], ContextState::Active, None);
    let sent = kernel.preview_request().unwrap();
    assert!(
        sent.messages.iter().any(|m| m
            .content
            .as_ref()
            .is_some_and(|c| c.to_text().len() == 1_000)),
        "the whole output can be put back in front of the model"
    );

    // one undo takes the pair back together, rather than leaving half a tool call behind
    assert!(kernel.undo());
    assert!(kernel.undo());
    assert_eq!(kernel.item(whole.id).unwrap().state, ContextState::Archived);
    assert!(kernel.item(shown.id).unwrap().is_projected());
}

/// What a truncated result *is* outlives every state it passes through.
///
/// note: the bug this closes. `note` is documented as why an item is in its current state, and it
/// is replaced whenever that changes - correctly, since a reason for being excluded stops being
/// true the moment something is put back. The pair an output limit leaves behind was keeping a
/// fact about its *content* in there: which item held the whole. A live session cycled both rows
/// with `space` while looking at them, and the pointer between the two halves was gone - the only
/// sentence saying that item 54 was a short copy of item 53, destroyed by looking at it.
///
/// note: so it lives in `included_because` now, which is why the item is in the context at all
/// and which no state change touches. The archived half keeps a note as well, because "the model
/// was shown a truncated copy" really is why *that* one is archived.
#[tokio::test]
async fn what_a_shortened_result_is_survives_being_looked_at() {
    let kernel = Kernel::new(Config {
        default_tool_output_limit: Some(100),
        ..Config::default()
    });
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "big", json!({}))]),
        ModelResponse::text("done"),
    ])));
    kernel.add_tool(Arc::new(ConstTool::new("big", "y".repeat(1_000))));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();

    let results = tool_results(&kernel);
    assert_eq!(results.len(), 2, "the whole and the copy that was shown");
    let (whole, shown) = (results[0].id, results[1].id);
    assert!(
        kernel
            .item(shown)
            .unwrap()
            .included_because
            .as_deref()
            .is_some_and(|why| why.contains(&format!("whole output is item {whole}"))),
        "the copy has to say where the whole is"
    );

    // `space` on a row cycles it: all of it, a marker, nothing, all of it again. Twice round both
    for _ in 0..2 {
        for state in [
            ContextState::Elided,
            ContextState::Excluded,
            ContextState::Active,
        ] {
            kernel.set_state([whole, shown], state, None);
        }
    }

    for id in [whole, shown] {
        let item = kernel.item(id).unwrap();
        assert!(
            item.included_because.is_some(),
            "[{id}] no longer knows what it is"
        );
        assert_eq!(
            item.note, None,
            "[{id}] is active, so it has no state left to explain"
        );
    }
    assert!(
        kernel
            .item(shown)
            .unwrap()
            .included_because
            .as_deref()
            .is_some_and(|why| why.contains(&format!("whole output is item {whole}"))),
        "and the pointer between the halves is still there"
    );
}

#[tokio::test]
async fn the_whole_output_can_be_refused() {
    let kernel = Kernel::new(Config {
        default_tool_output_limit: Some(50),
        keep_truncated_output: false,
        ..Default::default()
    });
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "verbose", json!({}))]),
    ])));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.add_tool(Arc::new(ConstTool::new("verbose", "y".repeat(1_000))));
    kernel.push(ContextItem::user("talk"));

    kernel.step().await.unwrap();
    kernel.step().await.unwrap();

    // a tool that can produce more than you are willing to hold is the reason this exists; the
    // truncation is still reported, it is just no longer reversible
    let results = tool_results(&kernel);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].content.to_text().len(), 50);
    assert!(
        results[0]
            .included_because
            .as_deref()
            .is_some_and(|n| n.contains("truncated"))
    );
    assert!(
        !results[0]
            .included_because
            .as_deref()
            .unwrap()
            .contains("whole output is")
    );
}

#[tokio::test]
async fn the_default_output_limit_applies_to_tools_without_one() {
    let kernel = Kernel::new(Config {
        default_tool_output_limit: Some(50),
        ..Default::default()
    });
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "verbose", json!({}))]),
    ])));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.add_tool(Arc::new(ConstTool::new("verbose", "y".repeat(1_000))));
    kernel.push(ContextItem::user("talk"));

    kernel.step().await.unwrap();
    kernel.step().await.unwrap();

    let results = tool_results(&kernel);
    assert_eq!(
        results[0].content.to_text().len(),
        1_000,
        "the whole of it, archived"
    );

    let truncated = results[1].content.to_text().into_owned();
    assert_eq!(truncated.len(), 50);
    assert!(
        truncated.contains("bytes truncated by an output limit"),
        "{truncated}"
    );
}

#[tokio::test]
async fn an_identifier_is_never_reused_across_turns() {
    // a provider that numbers its calls from zero on every turn is not hypothetical, and a set
    // of identifiers scoped to one response cannot see it happening
    let (kernel, provider) = permissive([
        ModelResponse::tool_calls(vec![call("call_0", "peek", json!({}))]),
        ModelResponse::tool_calls(vec![call("call_0", "peek", json!({}))]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    kernel.push(ContextItem::user("go"));

    let mut events = kernel.subscribe();
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));

    let repairs: Vec<_> = drain(&mut events)
        .into_iter()
        .filter_map(|event| match event {
            Event::ToolCallRepaired { call, reason, .. } => Some((call.0, reason)),
            _ => None,
        })
        .collect();
    assert_eq!(
        repairs,
        [(
            "call_0_1".to_owned(),
            "the provider reused an identifier from earlier in the session".to_owned()
        )]
    );

    // the request that goes out never names the same call twice
    let last = provider.requests().last().unwrap().clone();
    let ids: Vec<_> = last
        .messages
        .iter()
        .flat_map(|m| m.tool_calls.iter().map(|c| c.id.0.clone()))
        .collect();
    assert_eq!(ids, ["call_0", "call_0_1"]);
    assert_eq!(
        last.messages
            .iter()
            .filter(|m| m.role == Role::Tool)
            .count(),
        2
    );

    // and pruning one half of one exchange leaves the other exchange whole, which is the whole
    // reason the identifiers have to be distinct
    let first_result = tool_results(&kernel)[0].id;
    kernel.set_state([first_result], ContextState::Excluded, Some("noise".into()));

    let projection = kernel.project();
    assert_eq!(projection.repairs.len(), 1, "{:?}", projection.repairs);
    let request = kernel.preview_request().unwrap();
    let calls = request
        .messages
        .iter()
        .flat_map(|m| m.tool_calls.iter())
        .count();
    let answers = request
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .count();
    assert_eq!(
        (calls, answers),
        (1, 1),
        "every call in the request has exactly one result"
    );
}

#[tokio::test]
async fn a_step_that_cannot_proceed_says_so() {
    let (kernel, _) = permissive([ModelResponse::text("never asked")]);

    let mut events = kernel.subscribe();
    // nothing has been pushed, so there is nothing to send
    assert!(kernel.step().await.is_err());

    let names = names(&mut events);
    assert!(
        names.contains(&"step.failed".to_owned()),
        "a pair of state changes with nothing between them is not an explanation: {names:?}"
    );
    assert_eq!(kernel.state(), State::Idle);
}
