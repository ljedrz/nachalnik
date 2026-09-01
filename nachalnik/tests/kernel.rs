//! Tests for the loop: what gets sent, what gets run, and who decides.

mod common;

use std::sync::Arc;

use common::{count, drain, inquisitive, names, permissive};
use nachalnik::{
    Capability, Config, ContextItem, ContextKind, ContextState, Event, Grant, Kernel, Message,
    ModelInfo, ModelResponse, Params, Projection, Projector, Record, Role, Skipped, State,
    StopReason, Verdict,
    selectors::Selector,
    test::{
        AllowAll, BrokenTool, ConstTool, DenyAll, EchoTool, LargestFirstCompactor,
        ScriptedProvider, Table, call,
    },
};
use serde_json::json;

fn tool_results(kernel: &Kernel) -> Vec<Arc<ContextItem>> {
    "kind:tool_result"
        .parse::<Selector>()
        .unwrap()
        .matches(&kernel.items())
        .into_iter()
        .filter_map(|id| kernel.item(id))
        .collect()
}

#[tokio::test]
async fn the_request_contains_exactly_what_the_user_put_in_it() {
    let (kernel, provider) = permissive([ModelResponse::text("hello")]);
    kernel.push(ContextItem::user("hi"));

    let State::Finished { item, stop } = kernel.turn().await.unwrap() else {
        panic!("no tools were involved")
    };
    assert_eq!(kernel.item(item).unwrap().source, "model");
    assert_eq!(
        stop,
        StopReason::EndTurn,
        "the model's own reason, carried by the state"
    );
    assert_eq!(
        kernel
            .last_response()
            .unwrap()
            .content
            .clone()
            .unwrap()
            .to_text(),
        "hello"
    );

    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages.len(), 1, "no prompt was added");
    assert_eq!(requests[0].messages[0].role, Role::User);
    assert_eq!(
        requests[0].messages[0].content.as_ref().unwrap().to_text(),
        "hi"
    );
    assert!(requests[0].tools.is_empty());
    assert!(requests[0].params.is_empty(), "no knobs were invented");
}

#[tokio::test]
async fn params_are_carried_verbatim() {
    let (kernel, provider) = permissive([ModelResponse::text("ok")]);
    kernel.push(ContextItem::user("hi"));

    let mut params = Params::new();
    params.insert("temperature".into(), json!(0.0));
    params.insert("thinking".into(), json!({ "type": "enabled" }));
    kernel.set_params(params.clone());

    kernel.turn().await.unwrap();
    assert_eq!(provider.requests()[0].params, params);
    assert_eq!(kernel.params()["thinking"]["type"], "enabled");
}

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
async fn the_budget_carries_the_providers_own_numbers_next_to_the_estimate() {
    let usage = nachalnik::Usage {
        input_tokens: Some(901),
        output_tokens: Some(8),
        reasoning_tokens: Some(139),
        cached_input_tokens: Some(16),
    };
    let (kernel, _) = permissive([ModelResponse {
        usage: Some(usage),
        ..ModelResponse::text("hi")
    }]);
    kernel.push(ContextItem::user("hi"));

    assert_eq!(kernel.budget().reported, None, "nothing has been sent yet");

    kernel.turn().await.unwrap();

    let budget = kernel.budget();
    assert_eq!(budget.reported, Some(usage));
    assert!(
        budget.used() < usage.input_tokens.unwrap() as usize,
        "the estimate is a floor, and a compactor can now see by how much"
    );
}

/// A policy that refuses everything and says which rule did it.
struct Fussy;

#[nachalnik::async_trait]
impl nachalnik::PermissionPolicy for Fussy {
    async fn evaluate(&self, _request: &nachalnik::PermissionRequest) -> Verdict {
        Verdict::Deny
    }

    fn why(&self, _call: &nachalnik::ToolCallId) -> Option<String> {
        Some("`shell` is off for the whole of this session".to_owned())
    }
}

#[tokio::test]
async fn a_policy_that_knows_why_it_refused_can_tell_the_model() {
    let (kernel, _) = inquisitive([
        ModelResponse::tool_calls(vec![call("c1", "shell", json!({}))]),
        ModelResponse::text("fine"),
    ]);
    kernel.set_policy(Arc::new(Fussy));
    kernel.add_tool(Arc::new(
        ConstTool::new("shell", "it ran!").with_capabilities([Capability::Shell]),
    ));
    kernel.push(ContextItem::user("do it"));
    kernel.turn().await.unwrap();

    // the policy's own words, carried to the model rather than kept for a screen: a reason made
    // of *this* policy's vocabulary is the one thing the kernel could never invent
    let said = tool_results(&kernel)[0].content.to_text().into_owned();
    assert!(
        said.contains("`shell` is off for the whole of this session"),
        "{said}"
    );
    assert!(
        said.contains("a standing rule rather than an answer to this one call"),
        "{said}"
    );
}

#[tokio::test]
async fn a_call_refused_by_whoever_was_asked_says_it_was_about_this_call() {
    let (kernel, _) = inquisitive([
        ModelResponse::tool_calls(vec![call("c1", "shell", json!({}))]),
        ModelResponse::text("fine"),
    ]);
    kernel.set_policy(Arc::new(Fussy2));
    kernel.add_tool(Arc::new(
        ConstTool::new("shell", "it ran!").with_capabilities([Capability::Shell]),
    ));
    kernel.push(ContextItem::user("do it"));

    let State::Deciding { calls } = kernel.step().await.unwrap() else {
        panic!("it should have stopped to ask")
    };
    kernel.decide(calls[0], Grant::Deny).unwrap();
    kernel.turn().await.unwrap();

    // the same refusal, and the opposite advice: nothing standing was decided here, so a
    // different approach is worth trying. The policy's reason is not used, because the policy is
    // not what refused it
    let said = tool_results(&kernel)[0].content.to_text().into_owned();
    assert!(said.contains("answer to this call"), "{said}");
    assert!(
        said.contains("an answer to this call rather than a standing rule"),
        "{said}"
    );
    assert!(!said.contains("off for the whole"), "{said}");
}

/// The same, but it asks rather than refusing - so the answer comes from `decide`.
struct Fussy2;

#[nachalnik::async_trait]
impl nachalnik::PermissionPolicy for Fussy2 {
    async fn evaluate(&self, _request: &nachalnik::PermissionRequest) -> Verdict {
        Verdict::Ask
    }

    fn why(&self, _call: &nachalnik::ToolCallId) -> Option<String> {
        Some("`shell` is off for the whole of this session".to_owned())
    }
}

#[tokio::test]
async fn a_refused_call_does_not_run_but_the_model_is_told() {
    let (kernel, _) = inquisitive([
        ModelResponse::tool_calls(vec![call("c1", "shell", json!({}))]),
        ModelResponse::text("fine"),
    ]);
    kernel.set_policy(Arc::new(DenyAll));
    kernel.add_tool(Arc::new(
        ConstTool::new("shell", "it ran!").with_capabilities([Capability::Shell]),
    ));
    kernel.push(ContextItem::user("do it"));

    let mut events = kernel.subscribe();
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));

    let results = tool_results(&kernel);
    assert_eq!(results.len(), 1);
    let said = results[0].content.to_text();
    assert!(said.contains("not permitted"), "{said}");
    assert_ne!(said, "it ran!");

    // and told *which kind* of refusal it was, because `not permitted` on its own leaves open
    // the one question a refused model has to answer: is trying again worth anything? This one
    // is a standing rule, so it is not
    assert!(
        said.contains("a standing rule rather than an answer to this one call"),
        "{said}"
    );
    assert!(
        !said.contains("  "),
        "no run-on spacing from a wrapped literal: {said}"
    );

    let events = drain(&mut events);
    assert_eq!(count(&events, "tool.started"), 0, "it never started");
    assert!(events.iter().any(|e| matches!(
        e,
        Event::PermissionDecided {
            grant: Grant::Deny,
            ..
        }
    )));
}

#[tokio::test]
async fn a_partly_permitted_batch_waits_for_the_whole_answer() {
    let (kernel, _) = inquisitive([
        ModelResponse::tool_calls(vec![
            call("c1", "read", json!({ "path": "src/a.rs" })),
            call("c2", "shell", json!({ "cmd": "curl evil.example" })),
        ]),
        ModelResponse::text("ok"),
    ]);
    kernel.set_policy(Arc::new(
        Table::new(Verdict::Ask).rule(Capability::Read, Verdict::Allow),
    ));
    kernel.add_tool(Arc::new(
        ConstTool::new("read", "fn main() {}").with_capabilities([Capability::Read]),
    ));
    kernel.add_tool(Arc::new(
        ConstTool::new("shell", "it ran!").with_capabilities([Capability::Shell]),
    ));
    kernel.push(ContextItem::user("look around"));

    let State::Deciding { calls } = kernel.step().await.unwrap() else {
        panic!("the shell call needs an answer")
    };
    assert_eq!(calls.len(), 1, "only the undecided call is asked about");
    assert_eq!(kernel.pending_permissions()[0].tool, "shell");
    assert!(
        tool_results(&kernel).is_empty(),
        "the permitted call waits for its neighbour, so the batch stays atomic"
    );

    kernel.decide(calls[0], Grant::Deny).unwrap();
    assert!(matches!(kernel.step().await.unwrap(), State::Idle));

    let results = tool_results(&kernel);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].content.to_text(), "fn main() {}");
    assert!(results[1].content.to_text().contains("not permitted"));
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

    // a truncated output is recorded twice: the whole of it, archived, and the shortened copy
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
    assert_eq!(
        shown.note.as_deref(),
        Some(&*format!(
            "949 bytes were truncated by the output limit; the whole output is item {}",
            whole.id
        ))
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
            .note
            .as_deref()
            .is_some_and(|n| n.contains("truncated"))
    );
    assert!(
        !results[0]
            .note
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
async fn every_step_of_the_way_is_an_event() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "echo", json!({}))]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(EchoTool::new("echo", [])));

    let mut events = kernel.subscribe();
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();

    assert_eq!(
        names(&mut events),
        [
            "context.added",   // the user's message
            "state.changed",   // idle -> requesting
            "model.requested", // ... and what it turned into
            "context.added",   // the model's turn
            "model.finished",
            "tool.requested",
            "permission.decided",
            "state.changed", // requesting -> ready
            "state.changed", // ready -> executing
            "tool.started",
            "context.added", // the tool's result
            "tool.finished",
            "state.changed", // executing -> idle
            "state.changed", // idle -> requesting
            "model.requested",
            "model.delta", // the scripted provider streams its text
            "context.added",
            "model.finished",
            "state.changed", // requesting -> finished
        ]
    );
}

#[tokio::test]
async fn the_session_log_is_append_only_and_exportable() {
    let (kernel, _) = permissive([ModelResponse::text("hello")]);
    kernel.push(ContextItem::user("hi"));
    kernel.turn().await.unwrap();
    kernel.finish();

    let history = kernel.history();
    assert_eq!(history[0].event.name(), "session.started");
    assert_eq!(history.last().unwrap().event.name(), "session.finished");
    assert!(history.windows(2).all(|w| w[0].seq < w[1].seq));
    assert!(
        !history.iter().any(|r| r.event.name() == "model.delta"),
        "deltas are broadcast, not recorded"
    );

    let seq = history[2].seq;
    assert_eq!(kernel.history_since(seq).len(), history.len() - 3);

    // a session is a list of events, so exporting it is a line per record
    let jsonl: Vec<String> = history
        .iter()
        .map(|r| serde_json::to_string(r).unwrap())
        .collect();
    assert!(jsonl[0].contains(r#""event":"session.started""#));
    let restored: Record = serde_json::from_str(&jsonl[0]).unwrap();
    assert_eq!(restored, history[0]);
}

#[tokio::test]
async fn progress_can_be_recorded_too() {
    let kernel = Kernel::new(Config {
        record_progress: true,
        ..Default::default()
    });
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse::text("hi")])));
    kernel.push(ContextItem::user("hi"));
    kernel.turn().await.unwrap();

    assert!(
        kernel
            .history()
            .iter()
            .any(|r| r.event.name() == "model.delta")
    );
}

#[tokio::test]
async fn compaction_is_visible_and_reversible() {
    let provider = Arc::new(
        ScriptedProvider::new([ModelResponse::text("thanks")]).with_info(ModelInfo {
            context_limit: Some(1_000),
            tool_calling: true,
            ..ModelInfo::new("scripted", "small")
        }),
    );
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(provider);
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor {
        threshold: 0.5,
        target: 0.2,
    })));

    // the turn that asked for them, so that the results are answering something and the budget
    // counts them: an orphaned result is one the projector drops, and a budget that counted it
    // would be quoting for a request that is not going to be sent
    kernel.push(ContextItem::assistant(
        "",
        vec![
            call("c1", "cargo", json!({})),
            call("c2", "grep", json!({})),
        ],
    ));
    let huge = kernel.push(ContextItem::tool_result(
        "c1".into(),
        "cargo",
        "x".repeat(2_000),
        false,
    ));
    kernel.push(ContextItem::tool_result(
        "c2".into(),
        "grep",
        "y".repeat(400),
        false,
    ));
    kernel.push(ContextItem::user("what now?"));
    let before = kernel.budget().context_tokens;

    let mut events = kernel.subscribe();
    kernel.turn().await.unwrap();

    let report = drain(&mut events)
        .into_iter()
        .find_map(|e| match e {
            Event::Compacted { report } => Some(report),
            _ => None,
        })
        .expect("the context was over the threshold");

    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].label, "cargo");
    assert_eq!(report.removed[0].tokens, 500);
    assert!(report.refused.is_empty());
    assert!(report.summary.is_some());
    assert!(report.reason.contains("1000-token limit"));
    assert!(report.tokens_after < report.tokens_before);

    // "no, put it back"
    assert_eq!(
        kernel.set_state([huge], ContextState::Active, None).changed,
        vec![huge]
    );
    assert!(kernel.budget().context_tokens > before);
}

#[tokio::test]
async fn a_pin_is_a_promise() {
    let kernel = Kernel::new(Config::default());
    let pinned = kernel.push(ContextItem::file("src/foo.rs", "x".repeat(400)).pinned());
    let doomed = kernel.push(ContextItem::file("src/bar.rs", "y".repeat(400)));

    let report = kernel.apply_compaction(nachalnik::CompactionPlan {
        remove: vec![pinned, doomed],
        elide: Vec::new(),
        summary: None,
        reason: "an overzealous compactor".into(),
    });

    assert_eq!(report.refused.len(), 1);
    assert_eq!(report.refused[0].id, pinned);
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].id, doomed);
    assert!(kernel.item(pinned).unwrap().is_projected());

    assert!(kernel.undo(), "and even that is one operation");
    assert!(kernel.item(doomed).unwrap().is_projected());
}

#[tokio::test]
async fn the_model_can_be_swapped_mid_session() {
    let (kernel, _) = permissive([ModelResponse::text("one")]);
    kernel.push(ContextItem::user("hi"));
    kernel.turn().await.unwrap();

    let mut events = kernel.subscribe();
    let previous = kernel
        .set_provider(Arc::new(
            ScriptedProvider::new([ModelResponse::text("two")])
                .with_info(ModelInfo::new("other", "model-2")),
        ))
        .unwrap();
    assert_eq!(previous.info().model, "scripted");
    assert_eq!(kernel.model_info().unwrap().provider, "other");
    assert!(names(&mut events).contains(&"model.changed".to_owned()));

    kernel.push(ContextItem::user("and again"));
    kernel.turn().await.unwrap();
    assert_eq!(
        kernel
            .last_response()
            .unwrap()
            .content
            .clone()
            .unwrap()
            .to_text(),
        "two"
    );
}

#[tokio::test]
async fn a_whole_session_survives_a_round_trip() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![
            call("c1", "chatty", json!({ "value": "x" })),
            call("c2", "nope", json!({})),
        ]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(
        ConstTool::new("chatty", "x".repeat(400)).with_output_limit(100),
    ));
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor::default())));
    kernel.push(ContextItem::instruction("AGENTS.md", "no unsafe").pinned());
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();

    let results: Vec<_> = tool_results(&kernel).iter().map(|i| i.id).collect();
    kernel.set_state(results, ContextState::Excluded, Some("noise".into()));
    kernel.undo();
    let mut params = Params::new();
    params.insert("temperature".into(), json!(0.0));
    kernel.set_params(params);
    kernel.finish();

    let history = kernel.history();
    assert!(history.len() > 15, "{} records", history.len());
    for record in &history {
        let json = serde_json::to_string(record).unwrap();
        let restored: Record = serde_json::from_str(&json).unwrap();
        assert_eq!(&restored, record, "{json}");
    }
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

#[tokio::test]
async fn reasoning_is_recorded_on_its_turn_and_offered_back() {
    let (kernel, provider) = permissive([
        ModelResponse {
            content: Some("the answer is 4".into()),
            reasoning: Some("2 and 2 is 4".into()),
            tool_calls: Vec::new(),
            stop: StopReason::EndTurn,
            usage: None,
            raw: None,
        },
        ModelResponse::text("still 4"),
    ]);
    kernel.push(ContextItem::user("what is 2 + 2?"));
    kernel.turn().await.unwrap();

    let turn = kernel
        .items()
        .into_iter()
        .find(|item| item.source == "model")
        .unwrap();
    assert_eq!(
        turn.reasoning()
            .map(|r| r.to_text().into_owned())
            .as_deref(),
        Some("2 and 2 is 4")
    );

    // the next request carries it back, so a provider whose API verifies its own thinking can
    // hand it over verbatim
    kernel.push(ContextItem::user("are you sure?"));
    kernel.turn().await.unwrap();

    let second = &provider.requests()[1];
    let assistant = second
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert_eq!(
        assistant
            .reasoning
            .as_ref()
            .map(|r| r.to_text().into_owned())
            .as_deref(),
        Some("2 and 2 is 4")
    );

    // ... and a projector told not to send it does not, while the record keeps it
    kernel.set_projector(Arc::new(nachalnik::LinearProjector {
        send_reasoning: false,
        ..Default::default()
    }));
    let quiet = kernel.preview_request().unwrap();
    assert!(quiet.messages.iter().all(|m| m.reasoning.is_none()));
    assert!(kernel.item(turn.id).unwrap().reasoning().is_some());
}

#[tokio::test]
async fn what_a_provider_attaches_to_a_call_comes_back_attached_to_it() {
    // Google's `thought_signature` is the case that made this necessary: the API hands one back
    // per function call and rejects the *next* request if it does not come back with the call it
    // belongs to. The kernel has no idea what it is, which is exactly the point
    let signature = json!({ "google": { "thought_signature": "El4KXAERTTIP" } });
    let (kernel, provider) = permissive([
        ModelResponse::tool_calls(vec![
            nachalnik::ToolCall::new("c1", "peek", json!({})).with_extra(signature.clone()),
        ]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.unwrap();

    let second = &provider.requests()[1];
    let assistant = second
        .messages
        .iter()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    assert_eq!(
        *assistant.tool_calls[0].extra, signature,
        "the call went back out without its signature"
    );

    // it is part of the turn, so it survives a session round trip and it is counted
    let turn = kernel
        .items()
        .into_iter()
        .find(|item| item.source == "model")
        .unwrap();
    let ContextKind::AssistantMessage { tool_calls, .. } = &turn.kind else {
        unreachable!()
    };
    assert_eq!(*tool_calls[0].extra, signature);
    assert!(turn.tokens > 0);

    let json = serde_json::to_string(&*turn).unwrap();
    let restored: ContextItem = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, *turn);
}

#[tokio::test]
async fn the_log_says_why_an_item_was_not_sent() {
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "peek", json!({}))]),
        ModelResponse::text("first"),
        ModelResponse::text("second"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("peek", "ok")));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();

    // the user prunes the result, so the projector has to drop the call that asked for it
    let result = tool_results(&kernel)[0].id;
    kernel.set_state([result], ContextState::Excluded, Some("noise".into()));
    kernel.push(ContextItem::user("and now?"));
    kernel.turn().await.unwrap();

    let requested = kernel
        .history()
        .into_iter()
        .filter_map(|record| match record.event {
            Event::ModelRequested {
                skipped, repairs, ..
            } => Some((skipped, repairs)),
            _ => None,
        })
        .next_back()
        .unwrap();

    // "why was that not in the request?" has to be answerable from the record alone, long after
    // the projection that decided it has been dropped
    let (skipped, repairs) = requested;
    assert!(
        skipped
            .iter()
            .any(|s| s.id == result && s.reason.contains("noise")),
        "{skipped:?}"
    );
    assert_eq!(repairs.len(), 1, "{repairs:?}");
    assert!(repairs[0].contains("dropped the call `c1`"), "{repairs:?}");

    // and it survives being written out and read back, which is the point of a log
    let json = serde_json::to_string(&kernel.history()).unwrap();
    assert!(json.contains("dropped the call `c1`"));
    let restored: Vec<Record> = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, kernel.history());
}

/// A provider that renders a payload, and sends exactly what it rendered.
struct Rendering;

#[nachalnik::async_trait]
impl nachalnik::Provider for Rendering {
    fn info(&self) -> ModelInfo {
        ModelInfo::new("example", "renders")
    }

    fn render(&self, request: &nachalnik::ModelRequest) -> Option<serde_json::Value> {
        Some(json!({ "messages": request.messages.len(), "params": request.params }))
    }

    async fn respond(
        &self,
        _request: nachalnik::ModelRequest,
        _deltas: nachalnik::DeltaSink,
    ) -> Result<ModelResponse, nachalnik::BoxError> {
        Ok(ModelResponse::text("ok"))
    }
}

#[tokio::test]
async fn the_payload_is_the_providers_own_account_and_is_kept_only_if_asked() {
    let kernel = Kernel::new(Config {
        record_payloads: true,
        ..Default::default()
    });
    kernel.set_provider(Arc::new(Rendering));
    kernel.push(ContextItem::user("hi"));

    // before it is sent, on demand, whatever the config says
    let previewed = kernel.preview_payload().unwrap().unwrap();
    assert_eq!(previewed["messages"], 1);

    kernel.turn().await.unwrap();
    let recorded = kernel
        .history()
        .into_iter()
        .find_map(|record| match record.event {
            Event::ModelPayload { payload } => Some(payload),
            _ => None,
        })
        .expect("the payload was asked for, so it is on the record");
    assert_eq!(
        recorded, previewed,
        "the preview is the thing that was sent"
    );

    // a provider that cannot render one says so rather than inventing it
    let (bare, _) = permissive([ModelResponse::text("ok")]);
    bare.push(ContextItem::user("hi"));
    assert_eq!(bare.preview_payload().unwrap(), None);
}

#[tokio::test]
async fn the_log_does_not_carry_payloads_unless_it_is_told_to() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(Rendering));
    kernel.push(ContextItem::user("hi"));
    kernel.turn().await.unwrap();

    // the log is affordable to keep forever because its events name things rather than carrying
    // them, and a request body in every record would end that
    assert!(
        !kernel
            .history()
            .iter()
            .any(|r| r.event.name() == "model.payload")
    );
    // ... and it is still there for the asking
    assert!(kernel.preview_payload().unwrap().is_some());
}

#[tokio::test]
async fn the_log_holds_the_same_bytes_as_the_context_not_a_copy() {
    let args = json!({ "path": "src/a.rs", "text": "x".repeat(4096) });
    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![nachalnik::ToolCall::new("c1", "write", args.clone())]),
        ModelResponse::text("done"),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("write", "ok")));
    kernel.push(ContextItem::user("write it"));

    let mut events = kernel.subscribe();
    kernel.turn().await.unwrap();

    let logged = drain(&mut events)
        .into_iter()
        .find_map(|event| match event {
            Event::ToolRequested { args, .. } => Some(args),
            _ => None,
        })
        .unwrap();

    let turn = kernel
        .items()
        .into_iter()
        .find(|item| item.source == "model")
        .unwrap();
    let ContextKind::AssistantMessage { tool_calls, .. } = &turn.kind else {
        unreachable!()
    };

    assert!(
        Arc::ptr_eq(&logged, &tool_calls[0].args),
        "the log should not be a second place the same bytes live"
    );

    // and a request carries the same allocation onward rather than copying it again
    kernel.push(ContextItem::user("and again"));
    let request = kernel.preview_request().unwrap();
    let sent = request
        .messages
        .iter()
        .find_map(|m| m.tool_calls.first())
        .unwrap();
    assert!(Arc::ptr_eq(&sent.args, &tool_calls[0].args));
}

#[tokio::test]
async fn a_tool_definition_is_not_copied_for_every_request() {
    let kernel = Kernel::new(Config::default());
    let schema = json!({ "type": "object", "properties": { "value": { "type": "string" } } });
    kernel.add_tool(Arc::new(
        ConstTool::new("peek", "ok").with_schema(schema.clone()),
    ));

    // `Tool::spec` runs afresh for every request; the schema it hands back is shared, not rebuilt
    let first = kernel.tool_specs();
    let second = kernel.tool_specs();
    assert!(Arc::ptr_eq(&first[0].schema, &second[0].schema));

    let request = {
        kernel.push(ContextItem::user("hi"));
        kernel.preview_request().unwrap()
    };
    assert!(Arc::ptr_eq(&request.tools[0].schema, &first[0].schema));
}

/// A projector for the dialect in which the whole context is one user message - as different a
/// shape as there is, and still one method.
struct OneMessage;

impl Projector for OneMessage {
    fn project(&self, items: &[Arc<ContextItem>]) -> Projection {
        let (mut text, mut included, mut skipped) = (String::new(), Vec::new(), Vec::new());

        for item in items {
            if !item.is_projected() {
                skipped.push(Skipped {
                    id: item.id,
                    reason: item.state.to_string(),
                });
                continue;
            }
            text.push_str(&format!("{}: {}\n", item.label, item.content.to_text()));
            included.push(item.id);
        }

        Projection {
            messages: vec![Message::new(Role::User, text)],
            included,
            skipped,
            repairs: Vec::new(),
        }
    }
}

#[tokio::test]
async fn the_shape_of_a_request_is_the_projectors_and_the_kernel_sends_what_it_says() {
    let (kernel, provider) = permissive([ModelResponse::text("noted")]);
    kernel.set_projector(Arc::new(OneMessage));

    kernel.push(ContextItem::system("be terse"));
    let dropped = kernel.push(ContextItem::file("src/lexer.rs", "fn lex() {}"));
    kernel.push(ContextItem::user("what is wrong?"));
    kernel.set_state(
        [dropped],
        ContextState::Excluded,
        Some("not this one".into()),
    );

    // the preview is the projector's work, not a description of it
    let preview = kernel.preview_request().unwrap();
    assert_eq!(preview.messages.len(), 1, "this dialect has one message");
    assert_eq!(preview.messages[0].role, Role::User);

    let projection = kernel.project();
    assert_eq!(projection.included.len(), 2);
    assert_eq!(projection.skipped.len(), 1);
    assert_eq!(projection.skipped[0].id, dropped);

    kernel.turn().await.unwrap();

    // and what the provider was handed is that, byte for byte
    let sent = &provider.requests()[0];
    assert_eq!(sent.messages, preview.messages);
    let text = sent.messages[0]
        .content
        .clone()
        .unwrap()
        .to_text()
        .into_owned();
    assert!(text.contains("system: be terse"));
    assert!(text.contains("user: what is wrong?"));
    assert!(
        !text.contains("fn lex"),
        "an excluded item is excluded whatever the shape of the request"
    );
}

#[tokio::test]
async fn every_seam_can_say_what_is_plugged_into_it() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse::text("hi")])));
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_compactor(Some(Arc::new(LargestFirstCompactor::default())));

    // a trait object nobody can name is a seam nobody can inspect, which for a runtime whose
    // whole claim is that the parts are visible and replaceable is the wrong way round
    assert!(
        kernel.policy().name().ends_with("AllowAll"),
        "{}",
        kernel.policy().name()
    );
    assert!(
        kernel.projector().name().ends_with("LinearProjector"),
        "the default projector should name itself: {}",
        kernel.projector().name()
    );
    // the default counter is `Calibrating<BytesPerToken>`, and the name says both halves: which
    // estimate is being made, and that it is being corrected
    let counter = kernel.counter().name();
    assert!(
        counter.contains("Calibrating") && counter.contains("BytesPerToken"),
        "{counter}"
    );
    assert!(
        kernel
            .compactor()
            .expect("one was set")
            .name()
            .ends_with("LargestFirstCompactor")
    );
    assert!(kernel.provider().is_some());

    // and swapping one through the seam is visible through the same accessor
    kernel.set_policy(Arc::new(DenyAll));
    assert!(kernel.policy().name().ends_with("DenyAll"));
}

#[tokio::test]
async fn a_policy_can_say_something_friendlier_than_its_type() {
    struct Bespoke;

    #[async_trait::async_trait]
    impl nachalnik::PermissionPolicy for Bespoke {
        async fn evaluate(&self, _: &nachalnik::PermissionRequest) -> Verdict {
            Verdict::Ask
        }
        fn name(&self) -> &'static str {
            "the one from the config file"
        }
    }

    let kernel = Kernel::new(Config::default());
    kernel.set_policy(Arc::new(Bespoke));

    assert_eq!(kernel.policy().name(), "the one from the config file");
}

#[tokio::test]
async fn a_provider_can_be_taken_out_again() {
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([ModelResponse::text("hi")])));
    let mut events = kernel.subscribe();

    let previous = kernel.clear_provider();
    assert!(previous.is_some(), "the one that was there is handed back");
    assert!(kernel.provider().is_none());
    assert!(kernel.model_info().is_none());

    // detaching is a change to the session like any other, so it is on the record
    assert!(
        drain(&mut events)
            .iter()
            .any(|event| matches!(event, Event::ModelChanged { to: None, .. })),
        "clearing the provider should be announced"
    );

    // and a step with nothing to talk to says so rather than doing something surprising
    assert!(matches!(
        kernel.step().await,
        Err(nachalnik::Error::NoProvider)
    ));
}
