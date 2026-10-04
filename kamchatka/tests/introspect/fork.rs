//! `fork`: a copy of the session, standing up to answer something without spending the
//! original's context on the reply.

use crate::{agent, answered, answers_from, one_turn};
use kamchatka::{
    introspect,
    tools::{Careful, Limits, Subject},
};
use nachalnik::{
    Config, ContextItem, ContextState, Kernel, ModelResponse, Role, Verdict,
    test::{ScriptedProvider, call},
};
use serde_json::json;
use std::sync::Arc;

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

/// Two forks asked in one turn are asked about the same context: the second is not handed what the
/// first one said.
///
/// note: a turn's calls run one after another, so the second fork's copy was taken with the
/// first's answer already in it - two copies meant to be compared, differing by one of their
/// answers. `without` could not keep it out, because the item did not exist when it was written.
#[tokio::test]
async fn forks_asked_in_one_turn_do_not_read_each_other() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![
            call(
                "c1",
                "fork",
                json!({ "action": "ask", "question": "one way?" }),
            ),
            call(
                "c2",
                "fork",
                json!({ "action": "ask", "question": "the other way?" }),
            ),
        ]),
        ModelResponse::text("THE FIRST COPY'S ANSWER"),
        ModelResponse::text("the second copy's answer"),
        ModelResponse::text("done"),
    ]);
    kernel.push(ContextItem::user("which way?"));

    kernel.turn().await.expect("the turn failed");

    let text = |n: usize| -> String {
        provider.requests()[n]
            .messages
            .iter()
            .filter_map(|message| message.content.as_ref())
            .map(|content| content.to_text().into_owned())
            .collect()
    };
    assert!(text(2).contains("the other way?"), "{}", text(2));
    assert!(
        !text(2).contains("THE FIRST COPY'S ANSWER"),
        "the second fork read the first: {}",
        text(2)
    );
    // and the session itself has both, as it should
    assert!(text(3).contains("THE FIRST COPY'S ANSWER"), "{}", text(3));
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

/// A fork says whether it actually left anything out, because asking it to pretend is not the same.
///
/// note: found live. A session asked a copy what it would conclude "without knowing my earlier
/// statement about quicksort", passed no `without` at all, got the same answer back, and reported
/// that as an ablation - the item it named was in front of the copy the whole time. The reply said
/// "on 9 of your items", which cannot be read as "on all of them", so nothing in it contradicted
/// the story. The difference between taking an item away and asking a model to disregard it is the
/// whole of what `fork` is for.
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

    // note: what it can say, which is that nothing was withheld at the caller's request. It used
    // to say the copy saw all of the caller's items, and the projector had already repaired the
    // unfinished call out of it; it says what was named now, because a fork beside another call in
    // the same turn is held away by this tool rather than by the caller, and the clause about that
    // is below
    assert!(
        pretended.contains("Nothing you named with `without` was taken away"),
        "{pretended}"
    );
    assert!(
        !pretended.contains("Nothing of yours was taken away"),
        "that sentence has to be the one about what was named, or a sibling's result kept out of \
         the copy reads as nothing withheld: {pretended}"
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

/// A fork beside another call in the same turn says what that call kept out of the copy.
///
/// note: two `fork` calls in one turn each exclude the other's result from their own copy -
/// deliberately, since the results do not exist when either snapshot is taken, and a fork handed
/// the second one the first one's answer would be comparing two different contexts. The exclusion
/// was not reported. The header sentence is chosen by whether `without` named anything, so a fork
/// whose copy was silently shortened by a sibling's result still said "Nothing of yours was taken
/// away, so this is the same context answering again" - a false statement about the copy the
/// answer came from, in the one line whose whole job is to say what the copy could and could not
/// read. A model comparing two forks reads it as "these two runs saw the same context", when in
/// fact each was missing an item the other might have had - the one inference two forks in a turn
/// exist to support.
#[tokio::test]
async fn a_fork_beside_another_call_says_what_that_call_kept_out_of_the_copy() {
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![
            call(
                "c1",
                "context",
                json!({ "action": "note", "content": "a finding", "reason": "because" }),
            ),
            call(
                "c2",
                "fork",
                json!({ "action": "ask", "question": "what is the latest note?" }),
            ),
        ]),
        ModelResponse::text("the copy's answer"),
        ModelResponse::text("done"),
    ]);
    kernel.push(ContextItem::user("write something down, then ask a copy"));

    kernel.turn().await.expect("the turn failed");

    // the fork ran after the note - calls in a turn run one at a time, so the note's result was
    // already in the context when the fork's snapshot was taken
    let said = answers_from(&kernel, &["fork"])[0].clone();
    assert!(
        said.contains("other call in this turn had not been answered"),
        "the copy is one result short of the caller's context, and nothing in the answer said so: \
         {said}"
    );
    assert!(
        !said.contains("Nothing of yours was taken away"),
        "and the sentence that says nothing was withheld has to be the one about what was named: \
         {said}"
    );
    assert!(
        said.contains("is not in it"),
        "and what is missing is named, so a model can go and read it here: {said}"
    );
    assert!(
        !said.contains(".."),
        "the clause joins a sentence that already ended: {said}"
    );
}

/// Two forks in a turn are not called the same context when a call between them wrote to it.
///
/// note: the sentence "this is the same context answering again" is the one a model reads as
/// "these two runs are comparable", and it stood whatever the copy actually held. A sibling call
/// that writes to the context rather than answering one is not caught by the exclusion of the
/// turn's results: a `context note` is an item nobody's result answers, and the second fork's
/// copy carried it while the first's did not - two forks asked together differing by one of the
/// items between them, with both replies saying the copies were alike.
#[tokio::test]
async fn a_fork_beside_a_call_that_writes_to_the_context_says_the_two_are_not_alike() {
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![
            call(
                "c1",
                "fork",
                json!({ "action": "ask", "question": "one way?" }),
            ),
            call(
                "c2",
                "context",
                json!({ "action": "note", "content": "a finding", "reason": "because" }),
            ),
            call(
                "c3",
                "fork",
                json!({ "action": "ask", "question": "the other way?" }),
            ),
        ]),
        ModelResponse::text("the first copy's answer"),
        ModelResponse::text("the second copy's answer"),
        ModelResponse::text("done"),
    ]);
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["fork"]);
    let (first, second) = (&said[0], &said[1]);

    // the note landed between the two forks, so the second copy has it and the first does not
    assert!(
        first.contains("on 1 of your items"),
        "the copy before the note had one item: {first}"
    );
    assert!(
        second.contains("on 2 of your items"),
        "and the copy after it had one more: {second}"
    );
    // the first has nobody to compare itself with, so the sentence is the one it was
    assert!(
        first.contains("this is the same context answering again"),
        "{first}"
    );
    // the second is not told the same thing
    assert!(
        !second.contains("this is the same context answering again"),
        "the two copies differ by an item, and the reply says they are the same one: {second}"
    );
    // and says what the difference is, by number, so a model can go and read it here
    assert!(second.contains("wrote to your context"), "{second}");
    let noted = kernel
        .items()
        .iter()
        .find(|item| item.content.to_text().contains("a finding"))
        .expect("the note is in the context")
        .id;
    assert!(
        second.contains(&noted.to_string()),
        "the item the second copy has and the first did not is named: {second}"
    );
    assert!(
        second.contains("not a comparison"),
        "and what that makes of reading the two answers beside each other: {second}"
    );
}

/// Two forks in a turn with nothing between them that writes to the context still say so.
///
/// note: the clause above is earned, not a stock sentence. A turn of two forks and nothing else
/// is the comparison `fork` exists for, and a copy saying it cannot be compared to anything
/// would be no more true than the one it replaced.
#[tokio::test]
async fn two_forks_with_nothing_writing_between_them_still_say_they_are_the_same_context() {
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![
            call(
                "c1",
                "fork",
                json!({ "action": "ask", "question": "one way?" }),
            ),
            call(
                "c2",
                "fork",
                json!({ "action": "ask", "question": "the other way?" }),
            ),
        ]),
        ModelResponse::text("the first copy's answer"),
        ModelResponse::text("the second copy's answer"),
        ModelResponse::text("done"),
    ]);
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["fork"]);
    assert!(
        said[0].contains("this is the same context answering again"),
        "{}",
        said[0]
    );
    assert!(
        said[1].contains("this is the same context answering again"),
        "two forks and nothing between them: {}",
        said[1]
    );
    let read = |said: &str| -> String {
        said.split(" on ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .expect("the count is in the first line")
            .to_owned()
    };
    assert_eq!(
        read(&said[0]),
        read(&said[1]),
        "and both read the same number of items: {} and {}",
        said[0],
        said[1]
    );
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
    // and it says where the numbers come from, since `context` is on offer and nothing refuses it
    assert!(said.contains("`context` prints"), "{said}");
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

/// A refusal sends the model where the item numbers come from only while it could use `context`.
///
/// note: everything named in an answer is read as something to try, so the refusal naming
/// `context` is advice for a call the session would be refused for - the cost of it being that
/// the model spends a request on being told what it had just been told it does not have. The
/// policy the tools are handed is the one the kernel is consulting, and a session run
/// `--deny context` is refused every `look`, so the sentence has to go with the capability.
#[tokio::test]
async fn a_refusal_names_where_the_numbers_come_from_only_while_context_is_reachable() {
    // the policy `agent` hands out does not stop the model reading its own context
    let (open, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "fork",
        json!({ "action": "ask", "question": "and without it?", "without": [999] }),
    )]));
    open.push(ContextItem::user("what do you make of this?"));
    open.turn().await.expect("the turn failed");

    let said = answers_from(&open, &["fork"])[0].clone();
    assert!(said.contains("999"), "{said}");
    assert!(
        said.contains("`context` prints"),
        "with `context` on offer and allowed, the refusal says where the numbers come from: {said}"
    );

    // and a session where every `look` is refused is not sent to one
    let kernel = Kernel::new(Config::default());
    kernel.set_provider(Arc::new(ScriptedProvider::new(one_turn(vec![call(
        "c1",
        "fork",
        json!({ "action": "ask", "question": "and without it?", "without": [999] }),
    )]))));
    let policy = Arc::new(Careful::new());
    for domain in ["fork", "log", "setup"] {
        policy.set(&Subject::parse(domain), Verdict::Allow);
    }
    policy.set(&Subject::parse("context"), Verdict::Deny);
    kernel.set_policy(policy.clone());
    let _anchor = introspect::install(&kernel, policy, Limits::default());

    kernel.push(ContextItem::user("what do you make of this?"));
    kernel.turn().await.expect("the turn failed");

    let said = answers_from(&kernel, &["fork"])[0].clone();
    assert!(said.contains("999"), "{said}");
    assert!(
        !said.contains("`context`"),
        "the refusal points a model at a tool whose every call is refused: {said}"
    );
}
