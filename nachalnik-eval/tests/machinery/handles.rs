//! The suite's handles: what a policy grants and what `amend` refuses.

use nachalnik::{Config, ContextId, ContextItem, Kernel};

#[tokio::test]
async fn the_policy_that_grants_two_handles_grants_only_those_two() {
    use nachalnik::{Capability, PermissionId, PermissionPolicy, PermissionRequest, Verdict};
    use nachalnik_eval::suite::handles::Granted;

    let asking = |capabilities: Vec<Capability>| PermissionRequest {
        id: PermissionId(1),
        call: nachalnik::ToolCallId("c1".to_owned()),
        tool: "whatever".to_owned(),
        capabilities,
        args: std::sync::Arc::new(serde_json::json!({})),
    };
    let verdict = |capabilities: Vec<Capability>| async move {
        Granted.evaluate(&asking(capabilities)).await
    };

    // the two the handles declare
    assert_eq!(
        verdict(vec![
            Capability::parse("introspect:read").expect("a subject")
        ])
        .await,
        Verdict::Allow
    );
    assert_eq!(
        verdict(vec![
            Capability::parse("context:revise").expect("a subject")
        ])
        .await,
        Verdict::Allow
    );

    // and nothing else, including the one a tool gets by saying nothing. `ToolSpec::new` leaves a
    // tool with no capabilities at all, and `all` over an empty list is `true` - so a shell that
    // forgot to declare itself used to be granted by the policy whose whole point is that it
    // grants exactly two things
    assert_eq!(verdict(Vec::new()).await, Verdict::Deny);
    assert_eq!(verdict(vec![Capability::exec("run")]).await, Verdict::Deny);
    // and not another operation in one of the two domains, which another tool may well declare
    for other in ["context:elide", "introspect:write"] {
        assert_eq!(
            verdict(vec![Capability::parse(other).expect("a subject")]).await,
            Verdict::Deny,
            "{other}"
        );
    }
    assert_eq!(
        verdict(vec![
            Capability::parse("introspect:read").expect("a subject"),
            Capability::exec("run"),
        ])
        .await,
        Verdict::Deny,
        "one of the two beside something else is still something else"
    );
}

/// The policy says why it refuses, and the words reach the model rather than a screen.
///
/// note: `Granted` refuses everything the two handles do not cover, which is most things, and the
/// reason is the one thing a subject that calls a tool it was not granted gets to read.
#[tokio::test]
async fn the_policy_answers_for_itself_when_it_refuses() {
    use std::sync::Arc;

    use nachalnik::{
        Capability, ModelResponse, State,
        test::{ConstTool, ScriptedProvider, call},
    };
    use nachalnik_eval::suite::handles::Granted;

    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new([
        ModelResponse::tool_calls(vec![call("c1", "shell", serde_json::json!({}))]),
        ModelResponse::text("fine"),
    ])));
    kernel.add_tool(Arc::new(
        ConstTool::new("shell", "it ran!").with_capabilities([Capability::exec("run")]),
    ));
    kernel.set_policy(Arc::new(Granted));
    kernel.push(ContextItem::user("run it"));
    assert!(matches!(
        kernel.turn().await.expect("a turn"),
        State::Finished { .. }
    ));

    let said = kernel
        .items()
        .iter()
        .find(|item| matches!(item.kind, nachalnik::ContextKind::ToolResult { .. }))
        .expect("a tool result")
        .content
        .to_text()
        .into_owned();
    assert!(said.contains("only the two handles"), "{said}");
    assert!(!said.contains("the permission policy refused it"), "{said}");
    // and the shape of it is the standing rule, so a refused subject knows retrying is worth
    // nothing - which is the runtime's wording and this policy's verdict that put it there
    assert!(said.contains("a standing rule"), "{said}");
}
#[tokio::test]
async fn amend_refuses_the_turn_it_is_called_from_and_no_earlier_one() {
    use nachalnik::{OutputSink, Tool, ToolCall};
    use nachalnik_eval::{Act, Journal, suite::handles::Amend};
    use std::sync::Arc;

    let kernel = Arc::new(Kernel::new(Config::default()));
    kernel.push(ContextItem::user("Which depot runs out first?"));
    let answer = kernel.push(ContextItem::assistant("Omsk", Vec::new()));
    kernel.push(ContextItem::user("Something in your context is wrong."));
    let revise = |id: ContextId| {
        ToolCall::new(
            "c1",
            "amend",
            serde_json::json!({
                "action": "revise", "id": id.0.to_string(), "content": "Kirov", "reason": "wrong",
            }),
        )
    };
    let journal = Journal::default();
    let amend = Amend::new(&kernel, journal.clone());

    // the description promises that only the turn a call is made from is refused, and an earlier
    // answer is the subject's own to rewrite
    let speaking = kernel.push(ContextItem::assistant("", vec![revise(answer)]));
    let out = amend
        .invoke(&revise(answer), OutputSink::disconnected())
        .await
        .expect("a tool result");
    assert!(!out.is_error, "{}", out.content.to_text());
    assert_eq!(
        kernel.item(answer).expect("the answer").content.to_text(),
        "Kirov"
    );

    // and the turn carrying the call is refused, with the reason the description gives
    let out = amend
        .invoke(&revise(speaking), OutputSink::disconnected())
        .await
        .expect("a tool result");
    assert!(out.is_error);
    assert!(
        out.content
            .to_text()
            .contains("the turn you are speaking in")
    );
    let acts = journal.lock();
    assert!(matches!(
        acts.as_slice(),
        [Act::Revised { .. }, Act::Refused { .. }]
    ));
    assert!(matches!(&acts[0], Act::Revised { id, .. } if id.0 == answer.0));
    // the record says what the item used to say, so a report can show the change rather than
    // only that one happened - and it is the *revised* item's own words, not another's
    assert!(
        matches!(&acts[0], Act::Revised { was, .. } if was == "Omsk"),
        "{:?}",
        acts[0]
    );
}

/// `look` shows the context the subject is carrying: every item, by the number and label the
/// session knows it by, and whether it is going into the next request.
///
/// note: the whole of `look` is this text.
#[tokio::test]
async fn look_lists_every_item_by_number_and_label() {
    use nachalnik::{OutputSink, Tool, test::call};
    use nachalnik_eval::{Journal, Origin, Probe, Reading, Subject, suite::handles::Inspect};
    use std::sync::Arc;

    let kernel = Arc::new(Kernel::new(Config::default()));
    kernel.set_provider(Arc::new(nachalnik::test::ScriptedProvider::new([
        nachalnik::ModelResponse::text("kirov"),
    ])));
    kernel.push(ContextItem::system("you are being measured").pinned());
    let note = kernel.push(ContextItem::memory("records/capacity", "3,593 tonnes"));
    kernel.push(ContextItem::user("which depot?"));
    let origin = Arc::new(Origin::of(&Subject::new(kernel.as_ref().clone())).expect("a provider"));
    let inspect = Inspect::new(
        &kernel,
        origin,
        Probe::new("which?", Reading::Choice(vec!["kirov".to_owned()])),
        [],
        1,
        Journal::default(),
    );

    let out = inspect
        .invoke(
            &call("1", "inspect", serde_json::json!({ "action": "look" })),
            OutputSink::disconnected(),
        )
        .await
        .expect("a tool result");
    assert!(!out.is_error, "{}", out.content.to_text());
    let listed = out.content.to_text().into_owned();
    // the count, so a subject knows what it is carrying without counting the lines
    assert!(listed.starts_with("3 items,"), "{listed}");
    for wanted in [
        "system",
        "pinned",
        "records/capacity",
        "3,593 tonnes",
        "user",
    ] {
        assert!(listed.contains(wanted), "{wanted} is missing:\n{listed}");
    }
    // and the number, which is how the two tools are addressed afterwards
    assert!(listed.contains(&note.0.to_string()), "{listed}");
}

/// A partial `exclude` says what went and what did not, and a `revise` naming nothing, or a name
/// that is not here, says so rather than going through to a replace.
#[tokio::test]
async fn exclude_says_what_it_refused_and_revise_needs_a_name_that_is_here() {
    use std::sync::Arc;

    use nachalnik::{ContextState, OutputSink, Tool, ToolCall};
    use nachalnik_eval::{Act, Journal, suite::handles::Amend};

    let kernel = Arc::new(Kernel::new(Config::default()));
    kernel.push(ContextItem::system("you are being measured").pinned());
    let note = kernel.push(ContextItem::memory("records/capacity", "3,593 tonnes"));
    let journal = Journal::default();
    let amend = Amend::new(&kernel, journal.clone());
    let call = |args: serde_json::Value| ToolCall::new("c1", "amend", args);

    // a system instruction and a memory together: one refused, one moved, and both said. The
    // refusals are appended to the success rather than replacing it, so a subject that asked for
    // two things is told which of them it got
    let out = amend
        .invoke(
            &call(serde_json::json!({
                "action": "exclude", "ids": ["1", "records/capacity"], "reason": "noise",
            })),
            OutputSink::disconnected(),
        )
        .await
        .expect("a tool result");
    assert!(!out.is_error, "{}", out.content.to_text());
    let said = out.content.to_text().into_owned();
    assert!(said.contains("1 item(s) are out"), "{said}");
    assert!(
        said.contains("a system instruction is not yours to move"),
        "{said}"
    );
    assert_eq!(
        kernel.item(note).expect("the note").state,
        ContextState::Excluded
    );

    // and a `revise` naming something that is not here is told so, with no replace attempted
    let before = kernel
        .item(note)
        .expect("the note")
        .content
        .to_text()
        .into_owned();
    let out = amend
        .invoke(
            &call(serde_json::json!({
                "action": "revise", "id": "records/nowhere", "content": "x", "reason": "guess",
            })),
            OutputSink::disconnected(),
        )
        .await
        .expect("a tool result");
    assert!(out.is_error);
    assert!(
        out.content
            .to_text()
            .contains("`revise` needs an `id` that is here"),
        "{}",
        out.content.to_text()
    );
    // and so is one naming nothing at all
    let out = amend
        .invoke(
            &call(serde_json::json!({ "action": "revise", "content": "x", "reason": "guess" })),
            OutputSink::disconnected(),
        )
        .await
        .expect("a tool result");
    assert!(
        out.content
            .to_text()
            .contains("`revise` needs an `id` that is here"),
        "{}",
        out.content.to_text()
    );
    assert_eq!(
        kernel.item(note).expect("the note").content.to_text(),
        before
    );

    let acts = journal.lock();
    assert!(
        matches!(acts.as_slice(), [Act::Refused { .. }, Act::Excluded { .. }]),
        "{acts:?}"
    );
}

/// `amend`'s two refusals that are not about the turn a call is made from, which the tool
/// description promises by name.
#[tokio::test]
async fn amend_refuses_a_system_instruction_and_a_pinned_item() {
    use nachalnik::{OutputSink, Tool, ToolCall};
    use nachalnik_eval::{Act, Journal, suite::handles::Amend};
    use std::sync::Arc;

    let kernel = Arc::new(Kernel::new(Config::default()));
    let brief = kernel.push(ContextItem::system("The brief."));
    let note = kernel.push(ContextItem::memory("records/capacity", "3,593 tonnes"));
    kernel.set_state([note], nachalnik::ContextState::Pinned, None);
    let movable = kernel.push(ContextItem::memory("records/annex", "a note"));

    let exclude = |ids: &[ContextId]| {
        ToolCall::new(
            "c1",
            "amend",
            serde_json::json!({
                "action": "exclude",
                "ids": ids.iter().map(|id| id.0.to_string()).collect::<Vec<_>>(),
                "reason": "shorten it",
            }),
        )
    };
    let journal = Journal::default();
    let amend = Amend::new(&kernel, journal.clone());

    // the pinned item beside an ordinary one: the call does what it can and says what it
    // would not do, which is one output and not an error
    let out = amend
        .invoke(&exclude(&[note, movable]), OutputSink::disconnected())
        .await
        .expect("a tool result");
    assert!(!out.is_error);
    let said = out.content.to_text().into_owned();
    assert!(said.contains("that item is pinned"), "{said}");
    assert!(
        said.contains("1 item(s) are out"),
        "the one ordinary item beside the refusal is still excluded: {said}"
    );

    // and the brief on its own is nothing but a refusal
    let out = amend
        .invoke(&exclude(&[brief]), OutputSink::disconnected())
        .await
        .expect("a tool result");
    assert!(out.is_error);
    assert!(
        out.content
            .to_text()
            .contains("a system instruction is not yours to move"),
        "{}",
        out.content.to_text()
    );

    // the refused ones are where they were, and the ordinary one is not
    assert_eq!(
        kernel.item(brief).expect("the brief").state,
        nachalnik::ContextState::Active
    );
    assert_eq!(
        kernel.item(note).expect("the note").state,
        nachalnik::ContextState::Pinned
    );
    assert_eq!(
        kernel.item(movable).expect("the annex").state,
        nachalnik::ContextState::Excluded
    );

    // and each refusal is on the record with its own reason
    let acts = journal.lock();
    let said = |id: ContextId| format!("exclude {id}");
    let refused: Vec<(String, &str)> = acts
        .iter()
        .filter_map(|act| match act {
            Act::Refused { what, why } => Some((what.clone(), why.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        refused,
        vec![
            (said(note), "that item is pinned"),
            (said(brief), "a system instruction is not yours to move"),
        ]
    );
    assert!(
        matches!(
            acts.as_slice(),
            [
                Act::Refused { .. },
                Act::Excluded { .. },
                Act::Refused { .. }
            ]
        ),
        "{acts:?}"
    );
}
