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
}
