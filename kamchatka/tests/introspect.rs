//! Tests for the two tools an agent inspects and manages its own context with.
//!
//! note: these drive the real loop rather than calling `Tool::invoke` by hand, because most of
//! what is worth checking is about the loop: that the tool is reached through the permission
//! policy, that what it says lands in the context as a tool result, that a fork's request really
//! is a second request to the same provider, and that changing the context from inside a turn
//! changes the request the turn goes on to send.
//!
//! note: the model is a `ScriptedProvider`, so a fork takes the next response off the same script.
//! That is not a limitation being worked around - it is what lets a test say exactly what the
//! fork was sent and exactly what it heard back.

use std::sync::Arc;

use kamchatka::introspect;
use nachalnik::{
    Config, ContextItem, ContextKind, ContextState, Kernel, ModelResponse, Role, ToolCallId,
    test::{AllowAll, ScriptedProvider, call},
};
use serde_json::json;

/// A kernel with both tools installed, the provider that will answer it, and the handle the tools
/// reach it through - which the caller has to hold on to, or they stop working.
fn agent(
    script: impl IntoIterator<Item = ModelResponse>,
) -> (Kernel, Arc<ScriptedProvider>, Arc<Kernel>) {
    let kernel = Kernel::new(Config::default());
    let provider = Arc::new(ScriptedProvider::new(script));
    kernel.set_provider(provider.clone());
    kernel.set_policy(Arc::new(AllowAll));
    let anchor = introspect::install(&kernel);

    (kernel, provider, anchor)
}

/// A tool call by id, for building an assistant turn by hand.
fn call_of(id: &str) -> nachalnik::ToolCall {
    call(id, "shell", json!({}))
}

/// A turn in which the model makes exactly these calls, and then says it is done.
fn one_turn(calls: Vec<nachalnik::ToolCall>) -> Vec<ModelResponse> {
    vec![
        ModelResponse::tool_calls(calls),
        ModelResponse::text("done"),
    ]
}

/// What the last tool result in the context says.
fn answered(kernel: &Kernel) -> String {
    kernel
        .items()
        .iter()
        .rev()
        .find(|item| matches!(item.kind, ContextKind::ToolResult { .. }))
        .map(|item| item.content.to_text().into_owned())
        .expect("the turn recorded no tool result")
}

/// Every tool result, oldest first.
fn all_answers(kernel: &Kernel) -> Vec<String> {
    answers_from(kernel, &["introspect", "amend"])
}

/// Every result one of these tools produced, oldest first.
fn answers_from(kernel: &Kernel, tools: &[&str]) -> Vec<String> {
    kernel
        .items()
        .iter()
        .filter(|item| {
            matches!(&item.kind, ContextKind::ToolResult { tool, .. } if tools.contains(&tool.as_str()))
        })
        .map(|item| item.content.to_text().into_owned())
        .collect()
}

#[tokio::test]
async fn look_lists_every_item_with_its_state_and_why() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "introspect",
        json!({ "action": "look" }),
    )]));

    kernel.push(ContextItem::system("be brief").pinned());
    let file = kernel.push(ContextItem::file("src/parser.rs", "fn parse() {}"));
    kernel.push(ContextItem::user("why is this failing?"));
    kernel.set_state([file], ContextState::Excluded, Some("too big".into()));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    // the state, the reason for it, and the label, which is what makes an item findable again
    assert!(said.contains("excluded"), "{said}");
    assert!(said.contains("too big"), "{said}");
    assert!(said.contains("src/parser.rs"), "{said}");
    assert!(said.contains("pinned"), "{said}");
    // the turn it is speaking in is in its own context, and it can see it
    assert!(said.contains("assistant_message"), "{said}");
    // an excluded item is listed and is not counted as going
    assert!(
        said.contains("3 of them go into the next request"),
        "{said}"
    );
}

#[tokio::test]
async fn hiding_an_item_says_how_to_get_it_back_and_takes_any_word_for_it() {
    // the failure this closes: a session elided twenty-two items, then spent two calls asking for
    // an `action` called `restore`, was told no such thing existed, and gave up. The reversal is
    // a `state`, and the moment worth saying so is the one where something has just been hidden
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "amend",
            json!({"action": "prune", "ids": [1], "state": "elide", "reason": "done with it"}),
        ),
        // not a word the schema lists, and unambiguous: there is one state that is "put it back"
        call(
            "c2",
            "amend",
            json!({"action": "prune", "ids": [1], "state": "unelide", "reason": "wanted it after all"}),
        ),
    ]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said: Vec<String> = kernel
        .items()
        .iter()
        .filter(|item| item.label == "amend")
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(said.len(), 2, "{said:?}");

    assert!(said[0].contains("now elided"), "{}", said[0]);
    assert!(
        said[0].contains("restore"),
        "the way back is on the line: {}",
        said[0]
    );
    assert!(
        said[0].contains("undo"),
        "and so is the bigger hammer: {}",
        said[0]
    );

    // `unelide` is not in the enum and means exactly one thing
    assert_eq!(kernel.items()[0].state, ContextState::Active, "{}", said[1]);

    // and putting something back does not then advertise a way back from that
    assert!(!said[1].contains("back:"), "{}", said[1]);
}

#[tokio::test]
async fn eliding_something_small_says_that_it_cost_more_than_it_saved() {
    // an elided item leaves a marker carrying the reason given for eliding it, so on a short item
    // the marker is the more expensive of the two. A live session elided twenty-two of them and
    // added 162 tokens doing it; both numbers were on the screen and it did not notice
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({
            "action": "prune",
            "ids": [1],
            "state": "elide",
            "reason": "a reason long enough to outweigh the four words it is replacing, which is                        the ordinary case for a short item rather than a contrived one",
        }),
    )]));

    kernel.push(ContextItem::user("what does it do?"));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("more than before, not less"), "{said}");
    assert!(said.contains("marker"), "and why: {said}");
}

#[tokio::test]
async fn a_long_item_comes_back_as_a_sample_unless_the_whole_of_it_is_asked_for() {
    // the trap this closes: reading an item copies it into the context, so asking to see a big
    // tool result in order to decide whether to keep it costs about what keeping it costs. A live
    // session did that twice and finished an honest clean-up heavier than the waste it removed
    let long = format!("HEAD-MARKER\n{}\nTAIL-MARKER", "noise line\n".repeat(2_000));
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call("c1", "introspect", json!({ "action": "look", "ids": [1] })),
        call(
            "c2",
            "introspect",
            json!({ "action": "look", "ids": [1], "whole": true }),
        ),
    ]));

    kernel.push(ContextItem::file("noise.log", long.clone()));
    kernel.push(ContextItem::user("go"));
    kernel.turn().await.expect("the turn failed");

    let results: Vec<String> = kernel
        .items()
        .iter()
        .filter(|item| item.label == "introspect")
        .map(|item| item.content.to_text().into_owned())
        .collect();
    assert_eq!(results.len(), 2, "both calls answered");
    let (sampled, whole) = (&results[0], &results[1]);

    // a sample keeps both ends, so the shape of the thing is still legible
    assert!(sampled.contains("HEAD-MARKER"), "{sampled:.400}");
    assert!(
        sampled.contains("TAIL-MARKER"),
        "the end is worth seeing too"
    );
    assert!(
        sampled.contains("bytes not shown"),
        "and it says what it left out"
    );
    assert!(
        sampled.contains("whole"),
        "and how to get it: {sampled:.400}"
    );
    assert!(
        sampled.len() < long.len() / 2,
        "the point is that it is smaller: {} vs {}",
        sampled.len(),
        long.len()
    );

    // and asking for it costs what it costs, which is the caller's decision to make
    assert!(whole.contains(&long), "the whole of it, when asked for");
    assert!(!whole.contains("bytes not shown"), "{whole:.200}");
}

#[tokio::test]
async fn look_with_ids_reads_the_whole_item_and_its_reasoning() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "introspect",
        json!({ "action": "look", "ids": [1, 99] }),
    )]));

    kernel.push(
        ContextItem::assistant("I will try the parser", Vec::new())
            .with_reasoning(Some("the stack trace points at parse()".into())),
    );
    kernel.push(ContextItem::user("go on"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("the stack trace points at parse()"), "{said}");
    assert!(said.contains("I will try the parser"), "{said}");
    // an id that names nothing is said so rather than silently skipped
    assert!(said.contains("[99] there is no such item"), "{said}");
}

#[tokio::test]
async fn request_reports_what_is_going_and_what_was_left_out() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "introspect",
        json!({ "action": "request" }),
    )]));

    kernel.push(ContextItem::system("be brief"));
    let file = kernel.push(ContextItem::file("secrets.env", "TOKEN=hunter2"));
    kernel.push(ContextItem::user("what now?"));
    kernel.set_state([file], ContextState::Excluded, Some("not yours".into()));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("left out:"), "{said}");
    assert!(said.contains("not yours"), "{said}");
    assert!(said.contains("system"), "{said}");
    // the request is summarized, never quoted: printing it would double every token being asked
    // about, and the excluded item's contents would come back in the answer
    assert!(!said.contains("hunter2"), "{said}");
}

#[tokio::test]
async fn draft_answers_on_a_fork_and_leaves_the_context_alone() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call("c1", "introspect", json!({ "action": "draft" }))]),
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

#[tokio::test]
async fn a_fork_is_asked_a_question_without_the_items_it_was_told_to_leave_out() {
    let (kernel, provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call(
            "c1",
            "introspect",
            json!({
                "action": "fork",
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
async fn amend_prunes_what_is_the_models_and_refuses_what_is_not() {
    let (kernel, provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({
            "action": "prune",
            "ids": [1, 2, 3],
            "state": "exclude",
            "reason": "I am done with this",
        }),
    )]));

    kernel.push(ContextItem::file("keep.rs", "keep me").pinned());
    kernel.push(ContextItem::system("be brief"));
    kernel.push(ContextItem::file("junk.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("tidy up"));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("a pin is a promise"), "{said}");
    assert!(said.contains("system instruction"), "{said}");
    assert!(said.contains("1 item(s) are now excluded: 3"), "{said}");

    let items = kernel.items();
    assert_eq!(items[0].state, ContextState::Pinned);
    assert_eq!(items[1].state, ContextState::Active);
    assert_eq!(items[2].state, ContextState::Excluded);
    assert_eq!(items[2].note.as_deref(), Some("I am done with this"));

    // the reason it gave is the reason the request reports, and the item really is gone from it
    let after = provider.requests().last().unwrap().clone();
    assert!(
        !after
            .messages
            .iter()
            .filter_map(|message| message.content.as_ref())
            .any(|content| content.to_text().contains("0000"))
    );
    // it is still listed, though: nothing was destroyed
    assert_eq!(kernel.items().len(), items.len());
}

#[tokio::test]
async fn amend_may_unpin_only_what_it_pinned_itself() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "amend",
            json!({ "action": "prune", "ids": [1], "state": "pin", "reason": "I need this" }),
        ),
        call(
            "c2",
            "amend",
            json!({ "action": "prune", "ids": [1], "state": "restore", "reason": "no I do not" }),
        ),
    ]));

    kernel.push(ContextItem::file("maybe.rs", "..."));
    kernel.push(ContextItem::user("think about it"));

    kernel.turn().await.expect("the turn failed");

    let answers = all_answers(&kernel);
    assert!(
        answers[0].contains("1 item(s) are now pinned"),
        "{answers:?}"
    );
    assert!(
        answers[1].contains("1 item(s) are now active"),
        "{answers:?}"
    );
    assert!(!answers[1].contains("a pin is a promise"), "{answers:?}");
    assert_eq!(kernel.items()[0].state, ContextState::Active);
}

#[tokio::test]
async fn amend_will_not_touch_the_turn_it_is_speaking_in() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({ "action": "prune", "ids": [2], "state": "exclude", "reason": "on reflection" }),
    )]));

    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    // item 2 is the assistant turn carrying the call: excluding it would take the call down with
    // it, and the answer to the call with it
    let said = answered(&kernel);
    assert!(said.contains("the assistant turn this very call"), "{said}");
    assert_eq!(kernel.items()[1].state, ContextState::Active);
}

#[tokio::test]
async fn revise_rewrites_an_item_and_says_who_did_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({
            "action": "revise",
            "ids": [1],
            "content": "the parser is in src/parse.rs, not src/parser.rs",
            "reason": "I wrote down the wrong path",
        }),
    )]));

    kernel.push(ContextItem::memory(
        "scratch",
        "the parser is in src/parser.rs",
    ));
    kernel.push(ContextItem::user("carry on"));

    kernel.turn().await.expect("the turn failed");

    let item = kernel.item(nachalnik::ContextId(1)).unwrap();
    assert_eq!(
        item.content.to_text(),
        "the parser is in src/parse.rs, not src/parser.rs"
    );
    assert_eq!(item.meta["revised"]["by"], "amend");
    assert_eq!(
        item.meta["revised"]["reason"],
        "I wrote down the wrong path"
    );

    // and the whole of what it said before is on the record, because nothing else could recover it
    assert!(kernel.history().iter().any(|record| matches!(
        &record.event,
        nachalnik::Event::ContextReplaced { was, .. }
            if was.to_text().contains("src/parser.rs")
    )));
}

#[tokio::test]
async fn undo_walks_back_this_tools_own_changes_and_nothing_else() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "amend",
            json!({ "action": "prune", "ids": [1], "state": "exclude", "reason": "too long" }),
        ),
        call(
            "c2",
            "amend",
            json!({
                "action": "revise",
                "ids": [2],
                "content": "shorter",
                "reason": "it was verbose",
            }),
        ),
        call(
            "c3",
            "amend",
            json!({ "action": "undo", "steps": 5, "reason": "I was wrong about both" }),
        ),
    ]));

    // the person's own decision, made before the turn: it must survive an undo that is not theirs
    let theirs = kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::memory("notes", "a long note"));
    kernel.push(ContextItem::user("tidy up"));
    kernel.set_state([theirs], ContextState::Elided, Some("their call".into()));

    kernel.turn().await.expect("the turn failed");

    let said = all_answers(&kernel);
    let walked = said.last().unwrap();
    assert!(
        walked.contains("walked 2 of your own change(s) back"),
        "{walked}"
    );
    assert!(
        walked.contains("0 change(s) of yours can still be undone"),
        "{walked}"
    );

    // both of its own changes are gone, including the note it wrote
    let big = kernel.item(nachalnik::ContextId(1)).unwrap();
    assert_eq!(big.state, ContextState::Elided);
    assert_eq!(big.note.as_deref(), Some("their call"));
    let notes = kernel.item(nachalnik::ContextId(2)).unwrap();
    assert_eq!(notes.content.to_text(), "a long note");
    assert!(notes.meta["revised"].is_null());
}

#[tokio::test]
async fn undo_with_nothing_of_its_own_says_whose_undo_it_is_not() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({ "action": "undo", "reason": "let me try" }),
    )]));

    let file = kernel.push(ContextItem::file("theirs.rs", "..."));
    kernel.push(ContextItem::user("go"));
    kernel.set_state([file], ContextState::Excluded, Some("their call".into()));

    kernel.turn().await.expect("the turn failed");

    let said = answered(&kernel);
    assert!(said.contains("nothing of yours to walk back"), "{said}");
    // the person's exclusion is exactly where they left it
    assert_eq!(kernel.items()[0].state, ContextState::Excluded);
}

#[tokio::test]
async fn a_reason_is_required_before_anything_changes() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({ "action": "prune", "ids": [1], "state": "exclude" }),
    )]));

    kernel.push(ContextItem::file("junk.rs", "..."));
    kernel.push(ContextItem::user("tidy up"));

    kernel.turn().await.expect("the turn failed");

    assert!(answered(&kernel).contains("`reason` is required"));
    assert_eq!(kernel.items()[0].state, ContextState::Active);
}

#[tokio::test]
async fn the_tools_stop_working_when_the_handle_goes() {
    let (kernel, _provider, anchor) = agent(one_turn(vec![call(
        "c1",
        "introspect",
        json!({ "action": "look" }),
    )]));

    kernel.push(ContextItem::user("look at yourself"));
    // whoever installed them has gone; the tools are still registered and still answer, and what
    // they answer is why they cannot do anything
    drop(anchor);

    kernel.turn().await.expect("the turn failed");

    assert!(answered(&kernel).contains("this session is over"));
}

#[tokio::test]
async fn an_action_nobody_implements_is_named_rather_than_guessed_at() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({ "action": "elide", "ids": [1], "reason": "it is enormous" }),
    )]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    // `elide` is a state, not an action, and a tool that quietly did the nearest thing would be
    // teaching the model an argument that does not exist. So it is refused - but a word this tool
    // does know, at another level, is worth saying so about: two live models in a row spent a
    // call each on `restore` and were told only that no such thing existed
    let said = answered(&kernel);
    assert!(
        said.contains("`elide` is a `state`, not an `action`"),
        "{said}"
    );
    assert!(
        said.contains("\"action\":\"prune\""),
        "the way out is spelled: {said}"
    );
    assert!(said.contains("\"state\":\"elide\""), "{said}");
    assert_eq!(kernel.items()[0].state, ContextState::Active);
}

#[tokio::test]
async fn an_action_that_is_no_part_of_this_tool_gets_the_list_of_the_ones_that_are() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({ "action": "delete", "ids": [1], "reason": "it is enormous" }),
    )]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    // nothing in here is called `delete` at any level, so there is nowhere to point: the answer
    // is the list, and pointedly not a suggestion, since `delete` is the one thing this tool
    // will not do to anything
    let said = answered(&kernel);
    assert!(said.contains("there is no `delete`"), "{said}");
    assert!(said.contains("prune") && said.contains("revise"), "{said}");
    assert_eq!(kernel.items()[0].state, ContextState::Active);
}

#[tokio::test]
async fn what_the_context_says_is_what_the_next_request_carries() {
    // the point of the whole exercise, in one test: a tool changed the context in the middle of a
    // turn, and the request that same turn goes on to send is the changed one
    let (kernel, provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({
            "action": "prune",
            "ids": [1],
            "state": "elide",
            "reason": "400 bytes of nothing",
        }),
    )]));

    kernel.push(ContextItem::file("big.rs", "0".repeat(400)));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn failed");

    let text = |request: &nachalnik::ModelRequest| -> String {
        request
            .messages
            .iter()
            .filter_map(|message| message.content.as_ref())
            .map(|content| content.to_text().into_owned())
            .collect()
    };
    let requests = provider.requests();
    assert!(text(&requests[0]).contains("0000"));
    // the marker carries the model's own words for why, because the projector supplies only the
    // brackets around the item's note
    assert!(
        !text(&requests[1]).contains("0000"),
        "{}",
        text(&requests[1])
    );
    assert!(
        text(&requests[1]).contains("[... 400 bytes of nothing ...]"),
        "{}",
        text(&requests[1])
    );
    // and the item is still there, still holding what it holds
    assert_eq!(
        kernel
            .item(nachalnik::ContextId(1))
            .unwrap()
            .content
            .byte_len(),
        400
    );
}

// ------------------------------------------------------------------- managing what it carries

#[tokio::test]
async fn budget_reports_what_is_really_going_and_what_it_would_buy_to_drop_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "introspect",
        json!({ "action": "budget" }),
    )]));

    kernel.push(ContextItem::system("be brief").pinned());
    kernel.push(ContextItem::file("big.rs", "0".repeat(4_000)));
    // an orphan: active, and repaired out of every request by the projector, so it is costing
    // nothing at all however expensive it looks
    kernel.push(ContextItem::tool_result(
        ToolCallId::from("nobody-asked"),
        "shell",
        "1".repeat(4_000),
        false,
    ));
    kernel.push(ContextItem::user("go"));

    kernel.turn().await.expect("the turn ran");

    let said = answered(&kernel);
    assert!(said.contains("the next request is"), "{said}");
    assert!(said.contains("in the tool definitions"), "{said}");
    // the figures are said to be estimates until a provider has charged for something
    assert!(said.contains("estimate"), "{said}");

    // the expensive list is what the request actually carries, so the orphan is not offered as
    // something to save tokens by giving up - eliding it would buy nothing
    let listed = said
        .lines()
        .skip_while(|line| !line.contains("most expensive"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(listed.contains("big.rs"), "{listed}");
    assert!(
        !listed.contains("shell"),
        "the orphan is not going anyway: {listed}"
    );
    // and what is not the agent's to move says so, rather than costing it a refused call
    assert!(listed.contains("not yours"), "{listed}");
}

#[tokio::test]
async fn a_class_of_items_can_be_pruned_without_naming_each_one() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![
        call(
            "c1",
            "amend",
            json!({
                "action": "prune",
                "select": "all:tool_results",
                "state": "elide",
                "reason": "I have what I needed from them",
            }),
        ),
        call(
            "c2",
            "amend",
            json!({
                "action": "prune",
                "select": "kind:nothing_like_this",
                "state": "elide",
                "reason": "trying it on",
            }),
        ),
    ]));

    kernel.push(ContextItem::assistant(
        "",
        vec![call_of("t1"), call_of("t2")],
    ));
    for id in ["t1", "t2"] {
        kernel.push(ContextItem::tool_result(
            ToolCallId::from(id),
            "shell",
            "0".repeat(500),
            false,
        ));
    }
    kernel.push(ContextItem::user("tidy up"));

    kernel.turn().await.expect("the turn ran");

    let answers = all_answers(&kernel);
    assert!(
        answers[0].contains("2 item(s) are now elided"),
        "{answers:?}"
    );
    for item in kernel.items() {
        if matches!(item.kind, ContextKind::ToolResult { ref tool, .. } if tool == "shell") {
            assert_eq!(item.state, ContextState::Elided);
        }
    }
    // a selector it got wrong is answered with the whole grammar, so the next attempt is an
    // informed one rather than another guess
    assert!(answers[1].contains("is not a selector"), "{answers:?}");
    assert!(answers[1].contains("tool:grep:latest"), "{answers:?}");
    assert!(answers[1].contains("state:excluded"), "{answers:?}");
}

#[tokio::test]
async fn a_note_is_written_down_where_compaction_cannot_reach_it() {
    let (kernel, _provider, _anchor) = agent(one_turn(vec![call(
        "c1",
        "amend",
        json!({
            "action": "note",
            "label": "the plan",
            "content": "read the tests first, and do not touch the lexer",
            "pin": true,
            "reason": "so it outlives this turn",
        }),
    )]));

    kernel.push(ContextItem::user("what is your plan?"));
    kernel.turn().await.expect("the turn ran");

    let written = kernel
        .items()
        .into_iter()
        .find(|item| item.label == "the plan")
        .expect("it was written down");

    assert_eq!(written.state, ContextState::Pinned);
    assert!(written.content.to_text().contains("do not touch the lexer"));
    // attributed to whoever wrote it, so "who put these tokens in here?" has an answer
    assert_eq!(written.source, "agent");
    assert_eq!(
        written.included_because.as_deref(),
        Some("so it outlives this turn")
    );

    // and it is one of its own changes, so it can walk it back - which archives it rather than
    // destroying it, like everything else here
    let tool = kernel.tool("amend").expect("installed");
    tool.invoke(
        &nachalnik::ToolCall::new("c2", "amend", json!({ "action": "undo", "reason": "no" })),
        nachalnik::OutputSink::disconnected(),
    )
    .await
    .expect("the tool answered");

    let written = kernel.item(written.id).expect("still listed");
    assert_eq!(written.state, ContextState::Archived);
    assert!(written.content.to_text().contains("do not touch the lexer"));
}

#[tokio::test]
async fn a_fork_that_asks_for_a_tool_says_so_rather_than_answering_blank() {
    // what a real model does when handed a copy of a context full of tool traffic and no tools:
    // it asks for one. Nothing in a fork can run it, so the answer arrives as a call and no
    // words - and a blank draft gives the caller no way to tell that from a copy that had
    // nothing to say
    let (kernel, _provider, _anchor) = agent([
        ModelResponse::tool_calls(vec![call("c1", "introspect", json!({ "action": "draft" }))]),
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
        ModelResponse::tool_calls(vec![call("c1", "introspect", json!({ "action": "draft" }))]),
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
