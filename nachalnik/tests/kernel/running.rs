//! What gets run: tool calls matched to tools, executed, shortened where a limit says so, and
//! handed back.
//!
//! note: a failing tool is not a kernel error here, and neither is an unknown one - both become
//! an error tool result the model is shown, because that is information the model needs and a
//! loop that stopped would be a loop the model cannot recover from. The output-limit tests are
//! the other half of the same idea: what the model is shown is shortened, and the whole of it is
//! excluded beside the short copy rather than thrown away.

use std::sync::Arc;

use nachalnik::{
    Block, Capability, Config, Content, ContextItem, ContextKind, ContextState, Event, Kernel,
    ModelResponse, OutputSink, Role, State, StopReason,
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
    kernel.add_tool(Arc::new(EchoTool::new("echo", [Capability::fs("read")])));
    kernel.push(ContextItem::user("echo x"));

    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].tools.len(), 1, "the tool was offered");
    assert_eq!(
        requests[0].tools[0].capabilities,
        vec![Capability::fs("read")]
    );

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
    // one real tool, because half of what this answer is for is naming the ones that are here
    kernel.add_tool(Arc::new(EchoTool::new("echo", [])));
    kernel.push(ContextItem::user("teleport me"));

    let mut events = kernel.subscribe();
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));
    assert!(names(&mut events).contains(&"tool.unknown".to_owned()));

    let results = tool_results(&kernel);
    assert_eq!(results.len(), 1);
    let said = results[0].content.to_text();
    assert!(said.contains("no tool named `teleport`"), "{said}");
    // and what is here, which is the half a live session needed: a model that had misspelled the
    // one tool it wanted was told four times that its word was wrong and never which word was
    // right, so it kept guessing and then stopped trying
    assert!(said.contains("the tools here are"), "{said}");
    assert!(said.contains("`echo`"), "{said}");
}

/// A call whose arguments are not a JSON object is answered by the kernel: which call, that they
/// did not parse, what the parser said and where, what arrived there, and what the tool adds. The
/// policy is not asked and the tool is not run, and the call goes back to the model with no
/// arguments rather than in a wrapper it would copy.
#[tokio::test]
async fn a_call_whose_arguments_did_not_parse_is_answered_by_the_kernel() {
    /// A tool that would ask about every call, and says how its arguments go.
    struct Advising;

    #[async_trait::async_trait]
    impl nachalnik::Tool for Advising {
        fn spec(&self) -> nachalnik::ToolSpec {
            nachalnik::ToolSpec::new("grep", "searches").with_capabilities([Capability::fs("read")])
        }

        fn unparsed(&self, written: &str) -> Option<String> {
            written
                .contains("\\(")
                .then(|| "Write every backslash in a pattern twice.".to_owned())
        }

        async fn invoke(
            &self,
            _call: &nachalnik::ToolCall,
            _output: OutputSink,
        ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
            panic!("a call that did not parse was run")
        }
    }

    // the default policy asks about everything, so a question would leave the turn deciding
    let kernel = Kernel::new(Config::default());
    let written = "{\"pattern\": \"f\\(x\", \"path\": ";
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![nachalnik::ToolCall::unparsed("c1", "grep", written)]),
        ModelResponse::text("ok"),
    ])));
    kernel.add_tool(Arc::new(Advising));
    kernel.push(ContextItem::user("look"));

    let mut events = kernel.subscribe();
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));
    let said = names(&mut events);
    assert!(said.contains(&"tool.unparsed".to_owned()), "{said:?}");
    assert!(
        !said.contains(&"permission.requested".to_owned()),
        "{said:?}"
    );
    assert!(!said.contains(&"tool.started".to_owned()), "{said:?}");

    let results = tool_results(&kernel);
    assert_eq!(results.len(), 1);
    let answer = results[0].content.to_text();
    for part in [
        "call to `grep` (`c1`)",
        "not a JSON object",
        "invalid escape at line 1 column",
        "f\\(x",
        "Write every backslash in a pattern twice.",
    ] {
        assert!(answer.contains(part), "{part} is not in: {answer}");
    }

    // and the model is sent back its call with nothing in it, never the text in a wrapper
    let request = kernel.preview_request().unwrap();
    let sent = serde_json::to_string(&request.messages).unwrap();
    assert!(!sent.contains("_unparsed"), "{sent}");
}

/// A call the tool says is malformed is answered with what the tool said, before the policy is
/// asked, and the tool is not run; a call it finds nothing wrong with is asked about as before.
#[tokio::test]
async fn a_malformed_call_is_refused_before_anybody_is_asked() {
    /// A tool with two operations, which knows a call naming neither is not one it can run.
    struct Picky;

    #[async_trait::async_trait]
    impl nachalnik::Tool for Picky {
        fn spec(&self) -> nachalnik::ToolSpec {
            nachalnik::ToolSpec::new("fs", "files")
                .with_capabilities([Capability::fs("read"), Capability::fs("write")])
        }

        fn malformed(&self, call: &nachalnik::ToolCall) -> Option<String> {
            (call.args["action"].as_str() != Some("read"))
                .then(|| "no `action`; `fs` does read".to_owned())
        }

        async fn invoke(
            &self,
            call: &nachalnik::ToolCall,
            _output: OutputSink,
        ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
            assert_eq!(call.args["action"], "read", "a malformed call was run");
            Ok(nachalnik::ToolOutput::new("read it"))
        }
    }

    // the default policy asks about everything, so a question would leave the turn deciding
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "fs", json!({ "path": "x" }))]),
        ModelResponse::text("ok"),
    ])));
    kernel.add_tool(Arc::new(Picky));
    kernel.push(ContextItem::user("look"));

    let mut events = kernel.subscribe();
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Finished { .. }
    ));
    let said = names(&mut events);
    assert!(
        !said.contains(&"permission.requested".to_owned()),
        "{said:?}"
    );
    assert!(!said.contains(&"tool.started".to_owned()), "{said:?}");
    let results = tool_results(&kernel);
    assert_eq!(results.len(), 1);
    assert!(
        results[0]
            .content
            .to_text()
            .contains("no `action`; `fs` does read")
    );

    // and a well-formed call is still a question
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call(
            "c2",
            "fs",
            json!({ "action": "read", "path": "x" }),
        )]),
    ])));
    kernel.push(ContextItem::user("again"));
    assert!(matches!(
        kernel.turn().await.unwrap(),
        State::Deciding { .. }
    ));
}

/// The text quoted back from a call that did not parse is the text around the fault, cut at
/// characters, and a tool that adds nothing adds nothing to the answer.
#[tokio::test]
async fn what_is_quoted_from_a_call_that_did_not_parse_is_the_text_around_the_fault() {
    let kernel = Kernel::new(Config::default());
    // two bytes a character, laid out so that both ends of the quote fall inside one: it starts
    // a hundred bytes before the fault and is two hundred long
    let written = format!(
        "{{\"pattern\": \"{}\\q {}\"}}",
        "é".repeat(200),
        "ü".repeat(200)
    );
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![nachalnik::ToolCall::unparsed(
            "c1",
            "echo",
            written.as_str(),
        )]),
        ModelResponse::text("ok"),
    ])));
    kernel.add_tool(Arc::new(EchoTool::new("echo", [])));
    kernel.push(ContextItem::user("look"));
    kernel.turn().await.unwrap();

    let results = tool_results(&kernel);
    assert_eq!(
        results[0].content.to_text(),
        format!(
            "the arguments of this call to `echo` (`c1`) are not a JSON object, so it was not run \
             and nothing was asked about it: invalid escape at line 1 column 415. What arrived \
             there was `...{}\\q {}...`. Send the call again with its arguments as one JSON object.",
            "é".repeat(50),
            "ü".repeat(49)
        )
    );
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
async fn a_failing_tool_is_reported_with_what_its_error_wraps() {
    #[derive(Debug)]
    struct Wrapped(std::io::Error);

    impl std::fmt::Display for Wrapped {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("reading the config")
        }
    }

    impl std::error::Error for Wrapped {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    struct Failing;

    #[nachalnik::async_trait]
    impl nachalnik::Tool for Failing {
        fn spec(&self) -> nachalnik::ToolSpec {
            nachalnik::ToolSpec::new("failing", "fails with a cause")
        }

        async fn invoke(
            &self,
            _call: &nachalnik::ToolCall,
            _output: OutputSink,
        ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
            Err(Box::new(Wrapped(std::io::Error::other(
                "permission denied",
            ))))
        }
    }

    let (kernel, _) = permissive([
        ModelResponse::tool_calls(vec![call("c1", "failing", json!({}))]),
        ModelResponse::text("ok"),
    ]);
    kernel.add_tool(Arc::new(Failing));
    kernel.push(ContextItem::user("try it"));
    kernel.turn().await.unwrap();

    assert_eq!(
        tool_results(&kernel)[0].content.to_text(),
        "reading the config: permission denied"
    );
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

/// A tool that answers whether the sink it was handed goes anywhere.
struct Connected;

#[nachalnik::async_trait]
impl nachalnik::Tool for Connected {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec::new("connected", "says whether its progress is watched")
    }

    async fn invoke(
        &self,
        _call: &nachalnik::ToolCall,
        output: OutputSink,
    ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
        Ok(nachalnik::ToolOutput::new(
            output.is_connected().to_string(),
        ))
    }
}

/// The sink the kernel hands a running tool says it is connected, and a disconnected one says not.
///
/// note: a tool asks before building progress only a watcher would see, so a sink that answered
/// wrongly either way would waste that work or hide it.
#[tokio::test]
async fn an_output_sink_says_whether_its_progress_goes_anywhere() {
    let (kernel, _) = permissive([ModelResponse::tool_calls(vec![call(
        "c1",
        "connected",
        json!({}),
    )])]);
    kernel.add_tool(Arc::new(Connected));
    kernel.push(ContextItem::user("go"));

    kernel.step().await.unwrap();
    kernel.step().await.unwrap();

    assert_eq!(tool_results(&kernel)[0].content.to_text(), "true");
    assert!(!OutputSink::disconnected().is_connected());
}

#[tokio::test]
async fn output_limits_are_enforced_and_admitted() {
    let (kernel, _) = permissive([ModelResponse::tool_calls(vec![
        call("c1", "chatty", json!({})),
        call("c2", "verbose", json!({})),
        call("c3", "exact", json!({})),
    ])]);
    kernel.add_tool(Arc::new(
        ConstTool::new("chatty", "x".repeat(1_000)).with_output_limit(100),
    ));
    kernel.add_tool(Arc::new(ConstTool::new("verbose", "y".repeat(1_000))));
    // exactly as long as its limit, which is not over it
    kernel.add_tool(Arc::new(
        ConstTool::new("exact", "z".repeat(100)).with_output_limit(100),
    ));
    kernel.push(ContextItem::user("talk"));

    let mut events = kernel.subscribe();
    kernel.step().await.unwrap();
    assert!(matches!(kernel.step().await.unwrap(), State::Idle));

    // a truncated output is recorded twice: the whole of it, excluded, and the truncated copy
    // the model is shown
    let results = tool_results(&kernel);
    assert_eq!(results.len(), 4, "three results, one of them a pair");

    let (whole, shown) = (results[0].clone(), results[1].clone());
    assert_eq!(whole.state, ContextState::Excluded);
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
    assert_eq!(
        results[3].content.to_text(),
        "z".repeat(100),
        "and the one at its limit, whole and alone"
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
    assert_eq!(
        finished,
        vec![(Some(949), Some(whole.id)), (None, None), (None, None)]
    );

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
    assert!(kernel.undo().unwrap());
    assert!(kernel.undo().unwrap());
    assert_eq!(kernel.item(whole.id).unwrap().state, ContextState::Excluded);
    assert!(kernel.item(shown.id).unwrap().is_projected());
}

/// Both halves of a shortened output say which operation produced them.
///
/// note: the label is the only name a tool result's row carries, and the pair an output limit
/// leaves behind is two rows for one call. The whole was the one still labelled with the bare
/// tool name, so a live run's context listed `fs` beside the `fs:read` it holds the rest of -
/// which reads as a second call, by a tool that would not say what it did, on precisely the row
/// somebody opens to read the part that was cut.
#[tokio::test]
async fn the_whole_of_a_shortened_output_is_named_for_the_operation_too() {
    let (kernel, _) = permissive([ModelResponse::tool_calls(vec![call(
        "c1",
        "files",
        json!({ "action": "read" }),
    )])]);
    kernel.add_tool(Arc::new(Narrowing));
    kernel.push(ContextItem::user("read it"));

    kernel.step().await.unwrap();
    assert!(matches!(kernel.step().await.unwrap(), State::Idle));

    let results = tool_results(&kernel);
    assert_eq!(
        results.len(),
        2,
        "the whole and the copy the model is shown"
    );
    assert_eq!(results[0].state, ContextState::Excluded);
    assert_eq!(
        (results[0].label.as_str(), results[1].label.as_str()),
        ("files:read", "files:read"),
    );
}

/// A call whose tool left the registry after it was decided is still named for its operation, run
/// or dropped.
///
/// note: the label was read off the registry, so a tool taken out while its call waited in
/// `Ready` ran the call and labelled the result with the bare tool name.
#[tokio::test]
async fn a_call_whose_tool_was_taken_out_is_still_named_for_its_operation() {
    for dropped in [false, true] {
        let (kernel, _) = permissive([ModelResponse::tool_calls(vec![call(
            "c1",
            "files",
            json!({ "action": "write" }),
        )])]);
        kernel.add_tool(Arc::new(Narrowing));
        kernel.push(ContextItem::user("write it"));

        assert!(matches!(kernel.step().await.unwrap(), State::Ready { .. }));
        kernel.remove_tool("files");
        match dropped {
            true => assert_eq!(kernel.cancel_pending_calls("not now"), 1),
            false => assert!(matches!(kernel.step().await.unwrap(), State::Idle)),
        }

        let labels: Vec<_> = tool_results(&kernel)
            .into_iter()
            .map(|result| result.label.clone())
            .collect();
        assert!(!labels.is_empty(), "dropped: {dropped}");
        assert!(
            labels.iter().all(|label| label == "files:write"),
            "dropped: {dropped}, {labels:?}"
        );
    }
}

/// A tool of several operations that says which one a call is, which is what earns a label.
struct Narrowing;

#[async_trait::async_trait]
impl nachalnik::Tool for Narrowing {
    fn spec(&self) -> nachalnik::ToolSpec {
        nachalnik::ToolSpec {
            capabilities: vec![Capability::fs("read"), Capability::fs("write")],
            output_limit: Some(100),
            ..nachalnik::ToolSpec::new("files", "reads a file or writes one")
        }
    }

    fn needs(&self, call: &nachalnik::ToolCall) -> Vec<Capability> {
        vec![Capability::fs(
            call.args["action"].as_str().unwrap_or("read"),
        )]
    }

    async fn invoke(
        &self,
        _call: &nachalnik::ToolCall,
        _output: nachalnik::OutputSink,
    ) -> Result<nachalnik::ToolOutput, nachalnik::BoxError> {
        Ok(nachalnik::ToolOutput::new("x".repeat(1_000)))
    }
}

/// What a tool that sets no limit of its own is cut at moves while the session runs, from the next
/// call, and `None` cuts nothing.
#[tokio::test]
async fn the_default_output_limit_moves_while_the_session_runs() {
    let kernel = Kernel::new(Config {
        default_tool_output_limit: Some(100),
        ..Config::default()
    });
    kernel.set_policy(Arc::new(AllowAll));
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "big", json!({}))]),
        ModelResponse::text("one"),
        ModelResponse::tool_calls(vec![call("c2", "big", json!({}))]),
        ModelResponse::text("two"),
        ModelResponse::tool_calls(vec![call("c3", "big", json!({}))]),
        ModelResponse::text("three"),
    ])));
    kernel.add_tool(Arc::new(ConstTool::new("big", "y".repeat(1_000))));
    assert_eq!(
        kernel.default_tool_output_limit(),
        Some(100),
        "it starts where the configuration put it"
    );

    kernel.push(ContextItem::user("go"));
    kernel.turn().await.unwrap();
    assert_eq!(kernel.set_default_tool_output_limit(Some(500)), Some(100));
    kernel.push(ContextItem::user("again"));
    kernel.turn().await.unwrap();
    assert_eq!(kernel.set_default_tool_output_limit(None), Some(500));
    kernel.push(ContextItem::user("and again"));
    kernel.turn().await.unwrap();

    let cut: Vec<Option<usize>> = kernel
        .history()
        .iter()
        .filter_map(|record| match &record.event {
            Event::ToolFinished { truncated, .. } => Some(*truncated),
            _ => None,
        })
        .collect();
    // the cut makes room for the line saying so, the same at either limit, so the two differ by
    // exactly what the limit moved
    let [Some(first), Some(second), None] = cut[..] else {
        panic!("cut at both limits and not at all once there was none: {cut:?}");
    };
    assert_eq!(first - second, 400, "{cut:?}");
    assert_eq!(
        kernel.config().default_tool_output_limit,
        Some(100),
        "the configuration says where it started"
    );
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
/// and which no state change touches. The excluded half keeps a note as well, because "the model
/// was shown a truncated copy" really is why *that* one is out.
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
        "the whole of it, excluded"
    );

    let truncated = results[1].content.to_text().into_owned();
    assert_eq!(truncated.len(), 50);
    assert!(
        truncated.contains("bytes truncated by an output limit"),
        "{truncated}"
    );
}

/// With a picture limit set, a tool's picture reaches the model whole beside its text cut to the
/// output limit, and the whole output is still kept beside it.
///
/// note: measured with the rest, a screenshot is over any limit a tool's text is held to, and the
/// model was shown a sentence naming it in its place.
#[tokio::test]
async fn a_tools_picture_reaches_the_model_whole_under_a_picture_limit() {
    let kernel = Kernel::new(Config {
        default_tool_output_limit: Some(50),
        ..Default::default()
    });
    assert_eq!(kernel.set_picture_limit(Some(1 << 20)), None);
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "screenshot", json!({}))]),
    ])));
    kernel.set_policy(Arc::new(AllowAll));
    let shot = Content::blocks([
        Block::text(Content::blob("image/png", "A".repeat(100_000))),
        Block::text(Content::text("y".repeat(1_000))),
    ]);
    kernel.add_tool(Arc::new(ConstTool::new("screenshot", shot.clone())));
    kernel.push(ContextItem::user("look"));

    kernel.step().await.unwrap();
    kernel.step().await.unwrap();

    let results = tool_results(&kernel);
    assert_eq!(results[0].content, shot, "the whole of it, excluded");
    assert_eq!(results[0].state, ContextState::Excluded);
    let shown = &results[1].content;
    assert_eq!(
        shown.blobs().len(),
        1,
        "the picture is in what the model is shown"
    );
    assert!(
        shown
            .to_text()
            .contains("bytes truncated by an output limit"),
        "{}",
        shown.to_text()
    );

    // the setter hands back what it was
    assert_eq!(kernel.set_picture_limit(None), Some(1 << 20));
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
