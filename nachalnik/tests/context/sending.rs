//! What the context projects to, and what that costs.
//!
//! note: the budget quotes for the request that would actually be sent rather than for what the
//! context is holding, and the difference is the whole point: an elided item is a marker the size
//! of a line standing in front of ten thousand tokens. The repair tests are the other half - a
//! projector may drop a message to keep a request valid, and what it dropped and why is reported
//! rather than quietly done.

use std::sync::Arc;

use nachalnik::{
    BytesPerToken, ContextItem, ContextState, TokenCounter,
    test::{ConstTool, EchoTool},
};
use serde_json::json;

use crate::{kernel, select};

#[test]
fn what_a_client_needs_to_render_a_breakdown_is_all_there() {
    let kernel = kernel();
    kernel.add_tool(Arc::new(EchoTool::new("echo", [])));
    kernel.push(ContextItem::system("be terse"));
    let big = kernel.push(ContextItem::file("src/big.rs", "x".repeat(8_000)));
    kernel.set_state([big], ContextState::Excluded, Some("garbage".into()));

    let budget = kernel.budget();
    assert!(budget.tool_tokens > 0, "tool definitions cost tokens too");
    assert_eq!(budget.used(), budget.context_tokens + budget.tool_tokens);
    assert_eq!(budget.limit, None, "no provider, no limit to know");
    assert_eq!(kernel.with_context(|c| c.tokens_withheld()), 2_000);

    // ... per item: an identity, a label, a size, a state and a reason
    let item = kernel.item(big).unwrap();
    assert_eq!(
        (
            item.id,
            item.label.as_str(),
            item.tokens,
            item.state,
            item.note.as_deref()
        ),
        (
            big,
            "src/big.rs",
            2_000,
            ContextState::Excluded,
            Some("garbage")
        ),
    );
    assert_eq!(item.source, "file");
    assert_eq!(item.kind.name(), "reference");
}

#[test]
fn counting_is_replaceable() {
    struct OneEach;

    impl TokenCounter for OneEach {
        fn count(&self, _content: &nachalnik::Content) -> usize {
            1
        }
    }

    let kernel = kernel();
    kernel.push(ContextItem::file("src/a.rs", "a".repeat(400)));
    // 400 bytes of content plus the `src/a.rs:\n` the projector labels a reference with, which
    // is what the request will actually carry
    let projected = "src/a.rs:\n".len() + 400;
    assert_eq!(kernel.budget().context_tokens, projected.div_ceil(4));

    kernel.set_counter(Arc::new(OneEach));
    assert_eq!(kernel.budget().context_tokens, 1);

    kernel.set_counter(Arc::new(BytesPerToken { bytes_per_token: 2 }));
    assert_eq!(kernel.budget().context_tokens, projected.div_ceil(2));
}

#[tokio::test]
async fn the_projection_says_what_is_being_sent_and_what_is_not() {
    let kernel = kernel();
    kernel.add_tool(Arc::new(ConstTool::new("grep", "hits")));
    kernel.push(ContextItem::system("be terse"));
    kernel.push(ContextItem::user("look for foo"));
    let file = kernel.push(ContextItem::file("src/a.rs", "fn main() {}"));

    let projection = kernel.project();
    assert_eq!(projection.messages.len(), 3);
    assert_eq!(projection.included, select(&kernel, "all"));
    assert_eq!(
        projection.messages[2].content.as_ref().unwrap().to_text(),
        "src/a.rs:\nfn main() {}",
        "references are labelled, so the model knows what it is looking at"
    );

    kernel.set_state([file], ContextState::Excluded, Some("not relevant".into()));
    let projection = kernel.project();
    assert_eq!(projection.messages.len(), 2);
    assert_eq!(projection.skipped.len(), 1);
    assert_eq!(projection.skipped[0].id, file);
    assert_eq!(projection.skipped[0].reason, "excluded: not relevant");
}

#[test]
fn the_budget_quotes_for_the_request_that_would_actually_be_sent() {
    let kernel = kernel();
    kernel.push(ContextItem::user("hi"));

    // a tool result with no turn asking for it is one the projector drops, so it is not part of
    // what the next request costs - a budget that counted it would be quoting for a request that
    // is never going to exist
    let orphan = kernel.push(ContextItem::tool_result(
        "nobody-asked".into(),
        "grep",
        "x".repeat(4_000),
        false,
    ));
    assert!(kernel.item(orphan).unwrap().is_projected());
    assert_eq!(kernel.project().skipped.len(), 1);

    let budget = kernel.budget();
    let request = kernel.preview_request().unwrap();
    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        budget.context_tokens,
        kernel.item(kernel.items()[0].id).unwrap().tokens,
        "the orphan is listed and inspectable, but it is not part of the quote"
    );

    // and once the turn that asked for it is there, it counts
    kernel.push(ContextItem::assistant(
        "",
        vec![nachalnik::ToolCall::new("nobody-asked", "grep", json!({}))],
    ));
    assert!(kernel.budget().context_tokens > budget.context_tokens + 900);
}

#[test]
fn a_request_does_not_copy_the_context_to_build_itself() {
    let kernel = kernel();
    let id = kernel.push(ContextItem::user("x".repeat(1 << 20)));

    let item = kernel.item(id).unwrap();
    let projected = kernel.project();
    let sent = projected.messages[0].content.clone().unwrap();

    let (nachalnik::Content::Text(held), nachalnik::Content::Text(going)) = (&item.content, &sent)
    else {
        unreachable!()
    };
    assert!(
        Arc::ptr_eq(held, going),
        "every request re-projects the whole context; copying it each time would make a large \
         context cost more to look at than to think about"
    );

    // the same holds across the state changes an undo snapshot pins the old version for
    kernel.set_state([id], ContextState::Excluded, Some("too big".into()));
    let after = kernel.item(id).unwrap();
    let (nachalnik::Content::Text(before), nachalnik::Content::Text(after)) =
        (&item.content, &after.content)
    else {
        unreachable!()
    };
    assert!(
        Arc::ptr_eq(before, after),
        "pruning moved a pointer, not a megabyte"
    );
}

#[test]
fn a_result_follows_the_call_it_answers_whatever_lands_between_them() {
    // the failure this closes: a tool that writes something into the context - `amend note`, in
    // kamchatka - pushes its item while the turn that called it is still collecting results. The
    // item lands between the assistant message and the results, and every OpenAI-compatible API
    // refuses the request: "an assistant message with 'tool_calls' must be followed by tool
    // messages responding to each 'tool_call_id'". Five of seven live runs died this way, each
    // one immediately after the model had written down all ten of its correct findings
    let kernel = kernel();
    kernel.push(ContextItem::user("go"));
    kernel.push(ContextItem::assistant(
        "",
        vec![
            nachalnik::ToolCall::new("c1", "amend", json!({})),
            nachalnik::ToolCall::new("c2", "shell", json!({})),
        ],
    ));
    // what the tool wrote, pushed the moment it ran and so before the second result exists
    kernel.push(ContextItem::memory("q1", "Cargo.lock is 3593 lines"));
    kernel.push(ContextItem::tool_result(
        "c1".into(),
        "amend",
        "written down",
        false,
    ));
    kernel.push(ContextItem::tool_result(
        "c2".into(),
        "shell",
        "3593",
        false,
    ));

    let roles: Vec<&str> = kernel
        .project()
        .messages
        .iter()
        .map(|message| message.role.as_str())
        .collect();
    assert_eq!(
        roles,
        vec!["user", "assistant", "tool", "tool", "user"],
        "the results have to reach the wire before anything else the turn produced"
    );

    // and it is a repair like any other: moving somebody's item is not something to do quietly
    let projection = kernel.project();
    assert_eq!(
        projection.included.len(),
        5,
        "nothing was dropped to achieve it"
    );
    assert!(
        projection
            .repairs
            .iter()
            .any(|said| said.contains("moved item")),
        "the move is on the record: {:?}",
        projection.repairs
    );
}

/// Restoring the whole of a truncated output beside the copy the model was shown is one keystroke,
/// and the projector is what makes it safe: the pair is one call's answer, so the whole claims the
/// call and the shortened copy is dropped. What it says about that has to be true, though.
///
/// note: the two ways to fail to claim a call read the same and are not the same thing. A call
/// this projection does not carry is an orphan; a call it carries that is already answered is a
/// second result for it. Telling somebody who has just restored an archive that the call "is not
/// in the projection" sends them looking for a call that is on their screen.
#[tokio::test]
async fn a_second_result_for_one_call_is_not_reported_as_a_missing_call() {
    let kernel = kernel();
    let asked = nachalnik::test::call("c1", "read", json!({}));
    kernel.push(ContextItem::user("read it"));
    kernel.push(ContextItem::assistant("reading", vec![asked.clone()]));

    // the shape `Config::keep_truncated_output` leaves behind: the whole, archived, and the
    // shortened copy the model was handed, both answering the one call
    let whole = kernel.push(ContextItem::tool_result(
        asked.id.clone(),
        "read",
        "the whole file".repeat(50),
        false,
    ));
    kernel.set_state([whole], ContextState::Archived, None);
    kernel.push(ContextItem::tool_result(
        asked.id.clone(),
        "read",
        "the whole file[... truncated ...]",
        false,
    ));

    let sent = kernel.project();
    assert!(sent.repairs.is_empty(), "as it stands: {:?}", sent.repairs);

    // "send the whole one instead", which is `space` on the context tab
    kernel.set_state([whole], ContextState::Active, None);
    let sent = kernel.project();
    assert_eq!(
        sent.messages
            .iter()
            .filter(|m| m.tool_call_id.is_some())
            .count(),
        1,
        "one call still has exactly one answer"
    );
    assert_eq!(sent.repairs.len(), 1, "{:?}", sent.repairs);
    assert!(
        sent.repairs[0].contains("already has a result"),
        "the call is right there, and the repair has to say so: {}",
        sent.repairs[0]
    );
    assert!(
        !sent.repairs[0].contains("not in the projection"),
        "{}",
        sent.repairs[0]
    );

    // and a result nobody ever asked for still reads the other way
    kernel.push(ContextItem::tool_result(
        "nobody-asked".into(),
        "grep",
        "x",
        false,
    ));
    let sent = kernel.project();
    assert!(
        sent.repairs
            .iter()
            .any(|r| r.contains("`nobody-asked` is not in the projection")),
        "{:?}",
        sent.repairs
    );
}
