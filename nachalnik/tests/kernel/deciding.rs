//! Who decides: what the policy is asked, what it is told, and what a refusal reaches the model
//! as.
//!
//! note: two policies here rather than one, and the difference is the whole subject. `Fussy`
//! refuses by standing rule; `Fussy2` asks, and the answer comes from `Kernel::decide`. A model
//! reads those two refusals differently and should - one says the same call will be refused
//! again, the other says nothing about the next one - so the kernel words them differently and
//! these check that it does.

use std::sync::Arc;

use nachalnik::{
    Capability, ContextItem, Event, Grant, ModelResponse, State, Verdict,
    test::{ConstTool, DenyAll, Table, call},
};
use serde_json::json;

use crate::{
    common::{count, drain, inquisitive},
    tool_results,
};

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
