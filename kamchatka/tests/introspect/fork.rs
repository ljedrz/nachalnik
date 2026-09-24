//! `fork`: a copy of the session, standing up to answer something without spending the
//! original's context on the reply.

use crate::{agent, answered, answers_from};
use nachalnik::{ContextItem, ContextState, ModelResponse, Role, test::call};
use serde_json::json;

#[tokio::test]
async fn draft_answers_on_a_fork_and_leaves_the_context_alone() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call("c1", "fork", json!({ "action": "draft" }))]),
        // the fork's answer, taken off the same script
        ModelResponse::text("I would say the parser is fine"),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("is the parser fine?"));
    let before = kernel.items().len();

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("I would say the parser is fine"), "{said}");
    assert!(said.contains("nobody has read it"), "{said}");

    // three requests: the one that asked for the tool, the fork's, and the one after it
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    // a fork has no tools, which is the whole of what stops it acting
    assert!(requests[1].tools.is_empty());
    // and nothing it did is in this context: the turn added its own assistant turn and the tool
    // result, and nothing else
    assert_eq!(kernel.items().len(), before + 3);
    assert!(
        !kernel
            .items()
            .iter()
            .any(|item| item.source == "model" && item.content.to_text().contains("parser is fine"))
    );
}

/// A fork leads with the answer, because an output limit cuts from the end.
///
/// note: the reasoning is the bulk of a fork on a reasoning model - 68% of one real 34KB fork
/// against the answer's 30% - so with the thinking first the limit ate the answer and left the
/// deliberation about how to answer. One session asked a copy of itself three questions, and what
/// came back was the copy working out how to reply, with all three answers cut off the end.
#[tokio::test]
async fn a_fork_leads_with_the_answer_so_a_limit_cuts_the_thinking_instead() {
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "fork",
            json!({ "action": "ask", "question": "why did you stop?" }),
        )]),
        ModelResponse {
            reasoning: Some("X".repeat(2_000).into()),
            ..ModelResponse::text("Because the glob had crossed a line.")
        },
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("ask a copy of yourself"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    let answer = said
        .find("--- what it said")
        .expect("the answer is in there");
    let thinking = said.find("--- its reasoning").expect("so is the thinking");
    assert!(
        answer < thinking,
        "the answer has to come first, or a limit takes it: answer at {answer}, thinking at \
         {thinking}"
    );
    // and the section still says which is which, so the order is not a claim about what it did
    assert!(
        said.contains("which it produced before the answer above"),
        "{said}"
    );
}

#[tokio::test]
async fn a_fork_is_asked_a_question_without_the_items_it_was_told_to_leave_out() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "fork",
            json!({
                "action": "ask",
                "question": "does the note change your answer?",
                "without": [2],
            }),
        )]),
        ModelResponse::text("without it, no"),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("what do you make of this?"));
    kernel.push(ContextItem::memory(
        "a note",
        "the parser was rewritten last week",
    ));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("without it, no"), "{said}");
    assert!(said.contains("without 2"), "{said}");

    let asked = &provider.requests()[1];
    let text: String = asked
        .messages
        .iter()
        .filter_map(|message| message.content.as_ref())
        .map(|content| content.to_text().into_owned())
        .collect();
    assert!(!text.contains("rewritten last week"), "{text}");
    assert!(text.contains("does the note change your answer?"), "{text}");
    // and the item is still here, in the state it was in
    assert_eq!(
        kernel.item(nachalnik::ContextId(2)).unwrap().state,
        ContextState::Active
    );
}

#[tokio::test]
async fn a_fork_that_asks_for_a_tool_says_so_rather_than_answering_blank() {
    // what a real model does when handed a copy of a context full of tool traffic and no tools:
    // it asks for one. Nothing in a fork can run it, so the answer arrives as a call and no
    // words - and a blank draft gives the caller no way to tell that from a copy that had
    // nothing to say
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call("c1", "fork", json!({ "action": "draft" }))]),
        ModelResponse::tool_calls(vec![call("f1", "shell", json!({ "cmd": "ls" }))]),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("what would you say?"));
    kernel.turn().await.expect("the turn ran");

    let said = answered(&kernel);
    assert!(said.contains("it said nothing"), "{said}");
    assert!(
        said.contains("`shell`"),
        "it names what was asked for: {said}"
    );
    assert!(said.contains("a fork has no tools"), "{said}");
}

#[tokio::test]
async fn a_fork_is_told_it_cannot_act() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call("c1", "fork", json!({ "action": "draft" }))]),
        ModelResponse::text("I would say this"),
        ModelResponse::text("done"),
    ]);

    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn ran");

    // the copy cannot work it out from a context that shows tool calls and offers no tools, so
    // it is told - and told inside the fork, where it costs this session nothing
    let asked = &provider.requests()[1];
    let instructions: String = asked
        .messages
        .iter()
        .filter(|message| message.role == Role::System)
        .filter_map(|message| message.content.as_ref())
        .map(|content| content.to_text().into_owned())
        .collect();
    assert!(instructions.contains("no tools here"), "{instructions}");
    assert!(asked.tools.is_empty(), "and really has none");
}

/// A fork says whether it actually left anything out, because asking it to pretend is not the same.
///
/// note: found live. A session asked a copy what it would conclude "without knowing my earlier
/// statement about quicksort", passed no `without` at all, got the same answer back, and reported
/// that as an ablation - the item it named was in front of the copy the whole time. The reply said
/// "on 9 of your items", which cannot be read as "on all of them", so nothing in it contradicted
/// the story. The difference between taking an item away and asking a model to disregard it is the
/// whole of what `fork` is for.
/// A `draft` carrying `without` is refused, rather than answered without the items it names.
///
/// note: `fork` and `setup` were the two tools that never asked `unread`, and this is the one
/// where it costs something: `without` belongs to `ask`, `draft` takes no arguments at all, and a
/// `draft` that carried one bought a request whose answer looked like the experiment the caller
/// had asked for. An ablation nobody performed is worse than a refusal - it is read as evidence.
#[tokio::test]
async fn a_draft_that_names_items_to_leave_out_is_refused_rather_than_answered() {
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "fork",
            json!({ "action": "draft", "without": [1] }),
        )]),
        ModelResponse::text("done"),
    ]);
    kernel.push(ContextItem::user("quicksort is fastest"));
    kernel.push(ContextItem::user("go on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["fork"]);
    let refusal = said.last().expect("the tool answered");
    assert!(refusal.contains("`without`"), "{refusal}");
    assert!(
        refusal.contains("`ask`"),
        "it says whose argument it is: {refusal}"
    );
    assert!(
        !refusal.contains("what you would say if you answered now"),
        "the draft was answered anyway: {refusal}"
    );
}

#[tokio::test]
async fn a_fork_says_whether_anything_was_actually_kept_from_it() {
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "fork",
            json!({ "action": "ask", "question": "ignoring item 1, what now?" }),
        )]),
        ModelResponse::text("the copy's answer"),
        ModelResponse::tool_calls(vec![call(
            "c2",
            "fork",
            json!({ "action": "ask", "question": "what now?", "without": [1] }),
        )]),
        ModelResponse::text("the second copy's answer"),
        ModelResponse::text("done"),
    ]);
    kernel.push(ContextItem::user("quicksort is fastest"));
    kernel.push(ContextItem::user("go on"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["fork"]);
    let (pretended, ablated) = (&said[0], &said[1]);

    // note: what it can say, which is that nothing was withheld. It used to say the copy saw all
    // of the caller's items, and the projector had already repaired the unfinished call out of it
    assert!(
        pretended.contains("Nothing of yours was taken away"),
        "{pretended}"
    );
    // and the count is the caller's items, not the two this tool adds: the copy's own system
    // instruction and the question put to it
    assert!(
        pretended.contains("on 2 of your items"),
        "the count is of the caller's items: {pretended}"
    );
    assert!(
        pretended.contains("is still reading it"),
        "and says why a question asking it to disregard something is not an ablation: {pretended}"
    );
    assert!(
        !pretended.contains("without 1"),
        "nothing was left out, so nothing is reported as left out: {pretended}"
    );

    // and the real thing says what the copy could not read
    assert!(ablated.contains("without 1"), "{ablated}");
    assert!(ablated.contains("could not read at all"), "{ablated}");
    assert!(!ablated.contains("saw all of them"), "{ablated}");
}

/// An item to leave out that is not there is refused before a request is spent on the copy.
#[tokio::test]
async fn a_fork_told_to_leave_out_an_item_that_is_not_there_is_refused() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "fork",
            json!({ "action": "ask", "question": "and without it?", "without": [999] }),
        )]),
        ModelResponse::text("the same as before"),
        ModelResponse::text("done"),
    ]);
    kernel.push(ContextItem::user("what do you make of this?"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["fork"])[0].clone();
    assert!(said.contains("999"), "{said}");
    assert!(
        !provider
            .requests()
            .iter()
            .any(|request| request.messages.iter().any(|message| message
                .content
                .as_ref()
                .is_some_and(|content| content.to_text().contains("and without it?")))),
        "the copy was asked anyway"
    );
}
